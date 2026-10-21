//! The MongoDB catalog as a tree: databases ▸ collections and views. A
//! collection's fields are learnt from a sample of its documents, and its
//! indexes come with the `createIndex` that makes each.

use dfox_core::{
    arrow::ResultStream,
    catalog::{
        BrowseQuery, Catalog, ColumnInfo, EntityInfo, FilterOp, IndexInfo, Node, NodeKind,
        NodePath, Order,
    },
    errors::DbError,
    models::schema::TableColumn,
    result::OwnedCell,
};
use futures_util::{future::BoxFuture, StreamExt, TryStreamExt};
use mongodb::{
    bson::{doc, Bson, Document, Regex},
    Client,
};

use super::{convert::Shape, db_error, send_docs, spawn_run};

/// Documents read to learn a collection's fields.
const SAMPLE: i64 = 100;
/// Databases with more collections than this list without counts, and
/// autocomplete learns the fields of this many at most.
const COUNTED: usize = 200;
/// Documents read per collection for autocomplete.
const COLUMN_SAMPLE: i64 = 20;

pub struct MongoCatalog {
    client: Client,
}

/// `~12.4k`: a count, short.
fn estimate(rows: u64) -> String {
    match rows {
        n if n < 1000 => format!("~{n}"),
        n if n < 1_000_000 => format!("~{:.1}k", n as f64 / 1000.0),
        n => format!("~{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// The database and collection of a node path `[database, collection]`.
fn collection(node: &NodePath) -> Result<(&str, &str), DbError> {
    match node.parts() {
        [database, collection] => Ok((database, collection)),
        parts => Err(DbError::General(format!("not a collection: {parts:?}"))),
    }
}

/// A value from the grid as BSON.
fn bson(value: &OwnedCell) -> Bson {
    match value {
        OwnedCell::Null => Bson::Null,
        OwnedCell::Bool(b) => Bson::Boolean(*b),
        OwnedCell::Int(n) => Bson::Int64(*n),
        OwnedCell::UInt(n) => i64::try_from(*n).map_or(Bson::Double(*n as f64), Bson::Int64),
        OwnedCell::Float(f) => Bson::Double(*f),
        other => Bson::String(other.clone().into_text().unwrap_or_default()),
    }
}

/// `text` as a regular expression that matches it literally.
fn literal_pattern(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The `find` filter for a browse query's conditions.
pub fn filter(query: &BrowseQuery) -> Result<Document, DbError> {
    let mut filter = Document::new();
    for condition in &query.filters {
        let value = bson(&condition.value);
        let test = match condition.op {
            FilterOp::Eq => value,
            FilterOp::Ne => Bson::Document(doc! { "$ne": value }),
            FilterOp::Lt => Bson::Document(doc! { "$lt": value }),
            FilterOp::Le => Bson::Document(doc! { "$lte": value }),
            FilterOp::Gt => Bson::Document(doc! { "$gt": value }),
            FilterOp::Ge => Bson::Document(doc! { "$gte": value }),
            FilterOp::Contains => {
                let text = condition.value.clone().into_text().unwrap_or_default();
                let invalid = |_| DbError::General(format!("can't search for {text:?}"));
                Bson::RegularExpression(Regex {
                    pattern: literal_pattern(&text).try_into().map_err(invalid)?,
                    options: "i".try_into().map_err(invalid)?,
                })
            }
            FilterOp::IsNull => Bson::Null,
            FilterOp::NotNull => Bson::Document(doc! { "$ne": Bson::Null }),
        };
        filter.insert(condition.column.clone(), test);
    }
    Ok(filter)
}

/// JSON that reads like the shell.
fn shell_json(value: Bson) -> String {
    let json = value.into_relaxed_extjson();
    serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string())
}

/// `db.users`, or `db.getCollection("odd name")`.
fn handle(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        format!("db.{name}")
    } else {
        format!("db.getCollection({})", Bson::String(name.to_string()))
    }
}

impl MongoCatalog {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    async fn databases(&self) -> Result<Vec<Node>, DbError> {
        let mut names = self.client.list_database_names().await.map_err(db_error)?;
        names.sort();
        Ok(names
            .into_iter()
            .map(|name| Node::new(NodeKind::Database, name, true))
            .collect())
    }

    async fn collections(&self, database: &str) -> Result<Vec<Node>, DbError> {
        let db = self.client.database(database);
        let mut specs: Vec<(String, NodeKind)> = db
            .list_collections()
            .await
            .map_err(db_error)?
            .map_ok(|spec| {
                let kind = match spec.collection_type {
                    mongodb::results::CollectionType::View => NodeKind::View,
                    _ => NodeKind::Collection,
                };
                (spec.name, kind)
            })
            .try_collect()
            .await
            .map_err(db_error)?;
        specs.retain(|(name, _)| !name.starts_with("system."));
        specs.sort_by(|a, b| a.0.cmp(&b.0));
        let count = specs.len() <= COUNTED;
        let mut nodes = Vec::with_capacity(specs.len());
        for (name, kind) in specs {
            let mut node = Node::new(kind, name.clone(), true);
            if count && kind == NodeKind::Collection {
                if let Ok(n) = db
                    .collection::<Document>(&name)
                    .estimated_document_count()
                    .await
                {
                    node = node.with_detail(estimate(n));
                }
            }
            nodes.push(node);
        }
        Ok(nodes)
    }

    async fn describe_collection(&self, database: &str, name: &str) -> Result<EntityInfo, DbError> {
        let db = self.client.database(database);
        let coll = db.collection::<Document>(name);
        let sample: Vec<Document> = coll
            .find(doc! {})
            .limit(SAMPLE)
            .await
            .map_err(db_error)?
            .try_collect()
            .await
            .map_err(db_error)?;
        let mut shape = Shape::default();
        for doc in &sample {
            shape.learn(doc);
        }
        let columns = shape
            .columns()
            .iter()
            .map(|(field, kind)| ColumnInfo {
                name: field.clone(),
                type_name: kind.name().to_string(),
                // Missing from some documents, or null in some.
                nullable: sample
                    .iter()
                    .any(|d| matches!(d.get(field), None | Some(Bson::Null))),
                primary_key: field == "_id",
                ..ColumnInfo::default()
            })
            .collect();

        let reply = db
            .run_command(doc! { "listIndexes": name })
            .await
            .map_err(db_error)?;
        let specs = reply
            .get_document("cursor")
            .and_then(|c| c.get_array("firstBatch"))
            .map(|b| b.to_vec())
            .unwrap_or_default();
        let indexes = specs
            .into_iter()
            .filter_map(|spec| match spec {
                Bson::Document(spec) => Some(spec),
                _ => None,
            })
            .map(|spec| {
                let keys = spec.get_document("key").cloned().unwrap_or_default();
                let index_name = spec.get_str("name").unwrap_or_default().to_string();
                let unique = spec.get_bool("unique").unwrap_or(false) || index_name == "_id_";
                let mut options = spec.clone();
                for internal in ["v", "key", "ns"] {
                    options.remove(internal);
                }
                IndexInfo {
                    columns: keys.keys().cloned().collect(),
                    definition: format!(
                        "{}.createIndex({}, {});\n",
                        handle(name),
                        shell_json(Bson::Document(keys)),
                        shell_json(Bson::Document(options))
                    ),
                    name: index_name,
                    unique,
                }
            })
            .collect();
        let row_estimate = coll.estimated_document_count().await.ok();
        Ok(EntityInfo {
            columns,
            indexes,
            row_estimate,
            ..EntityInfo::default()
        })
    }

    async fn collection_definition(&self, database: &str, name: &str) -> Result<String, DbError> {
        let reply = self
            .client
            .database(database)
            .run_command(doc! { "listCollections": 1, "filter": { "name": name } })
            .await
            .map_err(db_error)?;
        let spec = reply
            .get_document("cursor")
            .and_then(|c| c.get_array("firstBatch"))
            .ok()
            .and_then(|b| b.first())
            .and_then(Bson::as_document)
            .cloned()
            .ok_or_else(|| DbError::General(format!("no collection {database}.{name}")))?;
        let mut options = spec.get_document("options").cloned().unwrap_or_default();
        let quoted = Bson::String(name.to_string());
        // A view is its source and pipeline.
        if let Ok(source) = options.get_str("viewOn") {
            let pipeline = options
                .get("pipeline")
                .cloned()
                .unwrap_or(Bson::Array(vec![]));
            return Ok(format!(
                "db.createView({quoted}, {}, {});\n",
                Bson::String(source.to_string()),
                shell_json(pipeline)
            ));
        }
        options.remove("uuid");
        if options.is_empty() {
            return Ok(format!("db.createCollection({quoted});\n"));
        }
        Ok(format!(
            "db.createCollection({quoted}, {});\n",
            shell_json(Bson::Document(options))
        ))
    }
}

impl Catalog for MongoCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        Box::pin(async move {
            match node.parts() {
                [] => self.databases().await,
                [database] => self.collections(database).await,
                [database, name] => Ok(self
                    .describe_collection(database, name)
                    .await?
                    .columns
                    .into_iter()
                    .map(|c| Node::new(NodeKind::Column, c.name, false).with_detail(c.type_name))
                    .collect()),
                parts => Err(DbError::General(format!("nothing under {parts:?}"))),
            }
        })
    }

    fn describe<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<EntityInfo, DbError>> {
        Box::pin(async move {
            let (database, name) = collection(node)?;
            self.describe_collection(database, name).await
        })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        Box::pin(async move {
            let (database, name) = collection(node)?;
            let coll = self.client.database(database).collection::<Document>(name);
            let sort: Document = query
                .sort
                .iter()
                .map(|(field, order)| {
                    let direction = if *order == Order::Descending { -1 } else { 1 };
                    (field.clone(), Bson::Int32(direction))
                })
                .collect();
            let limit = query.limit.map_or(usize::MAX, |l| l as usize);
            let filter = filter(&query)?;
            let (skip, take) = (query.offset, query.limit.map_or(0, |l| l as i64));
            Ok(spawn_run(move |tx| async move {
                let cursor = coll
                    .find(filter)
                    .sort(sort)
                    .skip(skip)
                    .limit(take)
                    .await
                    .map_err(db_error)?;
                send_docs(cursor.map(|d| d.map_err(db_error)), limit, &tx).await
            }))
        })
    }

    fn all_columns<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<Vec<TableColumn>, DbError>> {
        Box::pin(async move {
            let [database] = node.parts() else {
                return Err(DbError::General(format!(
                    "not a database: {:?}",
                    node.parts()
                )));
            };
            let db = self.client.database(database);
            let mut names = db.list_collection_names().await.map_err(db_error)?;
            names.retain(|name| !name.starts_with("system."));
            names.sort();
            names.truncate(COUNTED);
            let mut out = Vec::new();
            for name in names {
                // A few documents each: enough to suggest field names.
                let docs: Vec<Document> = db
                    .collection::<Document>(&name)
                    .find(doc! {})
                    .limit(COLUMN_SAMPLE)
                    .await
                    .map_err(db_error)?
                    .try_collect()
                    .await
                    .map_err(db_error)?;
                let mut shape = Shape::default();
                for doc in &docs {
                    shape.learn(doc);
                }
                out.extend(shape.columns().iter().map(|(field, kind)| TableColumn {
                    table: name.clone(),
                    name: field.clone(),
                    data_type: kind.name().to_string(),
                }));
            }
            Ok(out)
        })
    }

    fn count_rows<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let (database, name) = collection(node)?;
            self.client
                .database(database)
                .collection::<Document>(name)
                .count_documents(doc! {})
                .await
                .map_err(db_error)
        })
    }

    fn definition<'a>(
        &'a mut self,
        node: &'a NodePath,
        kind: NodeKind,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move {
            let (database, name) = collection(node)?;
            match kind {
                NodeKind::Collection | NodeKind::View | NodeKind::Table => {
                    self.collection_definition(database, name).await
                }
                kind => Err(DbError::General(format!(
                    "MongoDB can't show the definition of a {}",
                    kind.label()
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use dfox_core::catalog::Filter;

    use super::*;

    #[test]
    fn browse_conditions_become_a_filter() {
        let condition = |column: &str, op, value| Filter {
            column: column.into(),
            op,
            value,
        };
        let query = BrowseQuery {
            filters: vec![
                condition("age", FilterOp::Ge, OwnedCell::Int(18)),
                condition("name", FilterOp::Contains, OwnedCell::Text("a.b".into())),
                condition("gone", FilterOp::IsNull, OwnedCell::Null),
            ],
            ..BrowseQuery::default()
        };
        let filter = filter(&query).expect("a filter");
        assert_eq!(
            filter.get_document("age").expect("age"),
            &doc! { "$gte": 18_i64 }
        );
        assert_eq!(
            filter.get("name"),
            Some(&Bson::RegularExpression(Regex {
                pattern: "a\\.b".try_into().expect("pattern"),
                options: "i".try_into().expect("options"),
            }))
        );
        assert_eq!(filter.get("gone"), Some(&Bson::Null));
    }

    #[test]
    fn collections_are_named_as_the_shell_names_them() {
        assert_eq!(handle("users"), "db.users");
        assert_eq!(handle("odd name"), "db.getCollection(\"odd name\")");
        assert_eq!(estimate(12_400), "~12.4k");
    }
}
