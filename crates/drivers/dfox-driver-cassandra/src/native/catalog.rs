//! The Cassandra catalog as a tree: keyspaces ▸ tables and materialized
//! views ▸ columns, read from `system_schema`. Keys show which columns are
//! the partition key and which the clustering key; definitions come from
//! the server's own `DESCRIBE`.

use std::{cmp::Ordering, sync::Arc};

use dfox_core::{
    arrow::ResultStream,
    catalog::{
        BrowseQuery, Catalog, ColumnInfo, ConstraintInfo, ConstraintKind, EntityInfo, FilterOp,
        IndexInfo, Node, NodeKind, NodePath, Order,
    },
    errors::DbError,
    models::schema::TableColumn,
    result::OwnedCell,
};
use futures_util::{future::BoxFuture, stream, StreamExt, TryStreamExt};
use scylla::{
    client::session::Session as Scylla,
    value::{CqlValue, Row},
};

use super::{convert::Columns, db_error, send_rows, spawn_run};

/// Rows read to sort a browse, which CQL can only order within a
/// partition.
const SORTED_ROWS: usize = 10_000;

pub struct CqlCatalog {
    session: Arc<Scylla>,
}

/// A name as CQL writes it.
fn ident(name: &str) -> String {
    dfox_sql::quote_ident(name, dfox_sql::Dialect::Postgres)
}

/// The keyspace and table of a node path `[keyspace, table]`.
fn table(node: &NodePath) -> Result<(&str, &str), DbError> {
    match node.parts() {
        [keyspace, table] => Ok((keyspace, table)),
        parts => Err(DbError::General(format!("not a table: {parts:?}"))),
    }
}

/// A column of a table, as `system_schema.columns` describes it.
#[derive(Debug, Clone)]
struct Column {
    name: String,
    /// `partition_key`, `clustering`, `static` or `regular`.
    kind: String,
    position: i32,
    type_name: String,
    /// `asc` or `desc`, for clustering columns.
    order: String,
}

impl Column {
    fn rank(&self) -> u8 {
        match self.kind.as_str() {
            "partition_key" => 0,
            "clustering" => 1,
            "static" => 2,
            _ => 3,
        }
    }

    fn key(&self) -> bool {
        self.rank() < 2
    }
}

/// Columns in the order `SELECT *` gives them: the partition key, the
/// clustering key, then the rest by name.
fn ordered(mut columns: Vec<Column>) -> Vec<Column> {
    columns.sort_by(|a, b| {
        a.rank().cmp(&b.rank()).then(if a.key() {
            a.position.cmp(&b.position)
        } else {
            a.name.cmp(&b.name)
        })
    });
    columns
}

/// Reads every row of `cql` with `values`.
async fn rows(session: &Scylla, cql: &str, values: &[&str]) -> Result<Vec<Row>, DbError> {
    let values: Vec<CqlValue> = values
        .iter()
        .map(|v| CqlValue::Text((*v).to_string()))
        .collect();
    session
        .query_iter(cql.to_string(), values)
        .await
        .map_err(db_error)?
        .rows_stream::<Row>()
        .map_err(db_error)?
        .try_collect()
        .await
        .map_err(db_error)
}

fn text_at(row: &Row, i: usize) -> String {
    match row.columns.get(i) {
        Some(Some(CqlValue::Text(s) | CqlValue::Ascii(s))) => s.clone(),
        _ => String::new(),
    }
}

/// A value from the grid as the CQL type of its column.
fn bind(value: &OwnedCell, type_name: &str) -> Result<CqlValue, DbError> {
    let text = || value.clone().into_text().unwrap_or_default();
    let number = || -> Result<i64, DbError> {
        match value {
            OwnedCell::Int(n) => Ok(*n),
            OwnedCell::UInt(n) => i64::try_from(*n).map_err(db_error),
            _ => text().parse::<i64>().map_err(db_error),
        }
    };
    Ok(match type_name {
        "int" => CqlValue::Int(i32::try_from(number()?).map_err(db_error)?),
        "bigint" | "counter" => CqlValue::BigInt(number()?),
        "smallint" => CqlValue::SmallInt(i16::try_from(number()?).map_err(db_error)?),
        "tinyint" => CqlValue::TinyInt(i8::try_from(number()?).map_err(db_error)?),
        "boolean" => CqlValue::Boolean(matches!(value, OwnedCell::Bool(true)) || text() == "true"),
        "double" => CqlValue::Double(text().parse().map_err(db_error)?),
        "float" => CqlValue::Float(text().parse().map_err(db_error)?),
        "ascii" => CqlValue::Ascii(text()),
        "text" | "varchar" => CqlValue::Text(text()),
        other => {
            return Err(DbError::General(format!(
                "dfox can't filter a {other} column yet"
            )))
        }
    })
}

/// Orders two values the way their column sorts.
fn compare(a: Option<&CqlValue>, b: Option<&CqlValue>) -> Ordering {
    fn number(v: &CqlValue) -> Option<f64> {
        match v {
            CqlValue::Int(n) => Some(f64::from(*n)),
            CqlValue::BigInt(n) => Some(*n as f64),
            CqlValue::SmallInt(n) => Some(f64::from(*n)),
            CqlValue::TinyInt(n) => Some(f64::from(*n)),
            CqlValue::Float(n) => Some(f64::from(*n)),
            CqlValue::Double(n) => Some(*n),
            CqlValue::Counter(c) => Some(c.0 as f64),
            CqlValue::Timestamp(t) => Some(t.0 as f64),
            _ => None,
        }
    }
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => match (number(a), number(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => super::convert::text(a).cmp(&super::convert::text(b)),
        },
    }
}

impl CqlCatalog {
    pub fn new(session: Arc<Scylla>) -> Self {
        Self { session }
    }

    async fn keyspaces(&self) -> Result<Vec<Node>, DbError> {
        let found = rows(
            &self.session,
            "SELECT keyspace_name FROM system_schema.keyspaces",
            &[],
        )
        .await?;
        let mut names: Vec<String> = found.iter().map(|r| text_at(r, 0)).collect();
        // Yours first, then Cassandra's own.
        names.sort_by_key(|n| (n.starts_with("system"), n.clone()));
        Ok(names
            .into_iter()
            .map(|name| Node::new(NodeKind::Database, name, true))
            .collect())
    }

    async fn tables(&self, keyspace: &str) -> Result<Vec<Node>, DbError> {
        let tables = rows(
            &self.session,
            "SELECT table_name FROM system_schema.tables WHERE keyspace_name = ?",
            &[keyspace],
        )
        .await?;
        let indexes = self.index_names(keyspace).await?;
        let views = rows(
            &self.session,
            "SELECT view_name FROM system_schema.views WHERE keyspace_name = ?",
            &[keyspace],
        )
        .await?;
        let mut nodes: Vec<Node> = tables
            .iter()
            .map(|r| Node::new(NodeKind::Table, text_at(r, 0), true))
            .chain(
                views
                    .iter()
                    .map(|r| text_at(r, 0))
                    // ScyllaDB keeps each secondary index as a view.
                    .filter(|name| !indexes.iter().any(|i| *name == format!("{i}_index")))
                    .map(|name| Node::new(NodeKind::MaterializedView, name, true)),
            )
            .collect();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(nodes)
    }

    async fn index_names(&self, keyspace: &str) -> Result<Vec<String>, DbError> {
        Ok(rows(
            &self.session,
            "SELECT index_name FROM system_schema.indexes WHERE keyspace_name = ?",
            &[keyspace],
        )
        .await?
        .iter()
        .map(|r| text_at(r, 0))
        .collect())
    }

    async fn columns(&self, keyspace: &str, table: &str) -> Result<Vec<Column>, DbError> {
        let found = rows(
            &self.session,
            "SELECT column_name, kind, position, type, clustering_order \
             FROM system_schema.columns WHERE keyspace_name = ? AND table_name = ?",
            &[keyspace, table],
        )
        .await?;
        Ok(ordered(
            found
                .iter()
                .map(|r| Column {
                    name: text_at(r, 0),
                    kind: text_at(r, 1),
                    position: match r.columns.get(2) {
                        Some(Some(CqlValue::Int(n))) => *n,
                        _ => 0,
                    },
                    type_name: text_at(r, 3),
                    order: text_at(r, 4),
                })
                .collect(),
        ))
    }

    async fn describe_table(&self, keyspace: &str, name: &str) -> Result<EntityInfo, DbError> {
        let columns = self.columns(keyspace, name).await?;
        if columns.is_empty() {
            return Err(DbError::General(format!("no table {keyspace}.{name}")));
        }
        let partition: Vec<&Column> = columns
            .iter()
            .filter(|c| c.kind == "partition_key")
            .collect();
        let clustering: Vec<&Column> = columns.iter().filter(|c| c.kind == "clustering").collect();
        let names = |cols: &[&Column]| {
            cols.iter()
                .map(|c| ident(&c.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let key = if clustering.is_empty() && partition.len() == 1 {
            names(&partition)
        } else if clustering.is_empty() {
            format!("({})", names(&partition))
        } else {
            format!("({}), {}", names(&partition), names(&clustering))
        };
        let constraints = vec![ConstraintInfo {
            name: "PRIMARY KEY".to_string(),
            kind: ConstraintKind::PrimaryKey,
            definition: format!("PRIMARY KEY ({key})\n"),
        }];
        let target = format!("{}.{}", ident(keyspace), ident(name));
        let indexes = rows(
            &self.session,
            "SELECT index_name, options FROM system_schema.indexes \
             WHERE keyspace_name = ? AND table_name = ?",
            &[keyspace, name],
        )
        .await?
        .iter()
        .map(|r| {
            let index = text_at(r, 0);
            let on = match r.columns.get(1) {
                Some(Some(CqlValue::Map(options))) => options
                    .iter()
                    .find(|(k, _)| matches!(k, CqlValue::Text(k) if k == "target"))
                    .map(|(_, v)| super::convert::text(v))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            IndexInfo {
                definition: format!("CREATE INDEX {} ON {target} ({on});\n", ident(&index)),
                name: index,
                columns: vec![on],
                unique: false,
            }
        })
        .collect();
        let comment = rows(
            &self.session,
            "SELECT comment FROM system_schema.tables WHERE keyspace_name = ? AND table_name = ?",
            &[keyspace, name],
        )
        .await?
        .first()
        .map(|r| text_at(r, 0))
        .filter(|c| !c.is_empty());
        Ok(EntityInfo {
            columns: columns
                .iter()
                .map(|c| ColumnInfo {
                    name: c.name.clone(),
                    type_name: c.type_name.clone(),
                    nullable: !c.key(),
                    primary_key: c.key(),
                    comment: match c.kind.as_str() {
                        "partition_key" => Some("partition key".to_string()),
                        "clustering" => Some(format!("clustering key, {}", c.order.to_uppercase())),
                        "static" => Some("static".to_string()),
                        _ => None,
                    },
                    ..ColumnInfo::default()
                })
                .collect(),
            constraints,
            indexes,
            comment,
            ..EntityInfo::default()
        })
    }
}

impl Catalog for CqlCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        Box::pin(async move {
            match node.parts() {
                [] => self.keyspaces().await,
                [keyspace] => self.tables(keyspace).await,
                [keyspace, name] => Ok(self
                    .columns(keyspace, name)
                    .await?
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
            let (keyspace, name) = table(node)?;
            self.describe_table(keyspace, name).await
        })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        Box::pin(async move {
            let (keyspace, name) = table(node)?;
            let columns = self.columns(keyspace, name).await?;
            let type_of = |column: &str| {
                columns
                    .iter()
                    .find(|c| c.name == column)
                    .map(|c| c.type_name.clone())
                    .ok_or_else(|| DbError::General(format!("no column {column} in {name}")))
            };
            let mut conditions = Vec::new();
            let mut values = Vec::new();
            for filter in &query.filters {
                let column = ident(&filter.column);
                let ty = type_of(&filter.column)?;
                let sign = match filter.op {
                    FilterOp::Eq => "=",
                    FilterOp::Ne => "!=",
                    FilterOp::Lt => "<",
                    FilterOp::Le => "<=",
                    FilterOp::Gt => ">",
                    FilterOp::Ge => ">=",
                    FilterOp::IsNull | FilterOp::NotNull | FilterOp::Contains => {
                        return Err(DbError::General(
                            "CQL filters only compare values".to_string(),
                        ))
                    }
                };
                conditions.push(format!("{column} {sign} ?"));
                values.push(bind(&filter.value, &ty)?);
            }
            let mut cql = format!("SELECT * FROM {}.{}", ident(keyspace), ident(name));
            if !conditions.is_empty() {
                cql.push_str(&format!(
                    " WHERE {} ALLOW FILTERING",
                    conditions.join(" AND ")
                ));
            }
            // CQL orders only within a partition: sorting and skipping read
            // the first rows and do it here.
            let sorted = !query.sort.is_empty() || query.offset > 0;
            let wanted = query.limit.map_or(usize::MAX, |l| l as usize);
            let read = if sorted { SORTED_ROWS } else { wanted };
            let session = self.session.clone();
            let sort: Vec<(usize, Order)> = query
                .sort
                .iter()
                .filter_map(|(column, order)| {
                    columns
                        .iter()
                        .position(|c| &c.name == column)
                        .map(|i| (i, *order))
                })
                .collect();
            let offset = query.offset as usize;
            Ok(spawn_run(move |tx| async move {
                let pager = session.query_iter(cql, values).await.map_err(db_error)?;
                let shape = Columns::new(pager.column_specs().as_slice());
                let stream = pager.rows_stream::<Row>().map_err(db_error)?;
                if !sorted {
                    return send_rows(shape, stream.map_err(db_error), read, &tx).await;
                }
                let mut all: Vec<Row> = stream.map_err(db_error).take(read).try_collect().await?;
                all.sort_by(|a, b| {
                    sort.iter().fold(Ordering::Equal, |done, (i, order)| {
                        done.then_with(|| {
                            let by = compare(
                                a.columns.get(*i).and_then(Option::as_ref),
                                b.columns.get(*i).and_then(Option::as_ref),
                            );
                            if *order == Order::Descending {
                                by.reverse()
                            } else {
                                by
                            }
                        })
                    })
                });
                let page: Vec<Result<Row, DbError>> =
                    all.into_iter().skip(offset).take(wanted).map(Ok).collect();
                send_rows(shape, stream::iter(page), usize::MAX, &tx).await
            }))
        })
    }

    fn count_rows<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let (keyspace, name) = table(node)?;
            let cql = format!("SELECT COUNT(*) FROM {}.{}", ident(keyspace), ident(name));
            let found = rows(&self.session, &cql, &[]).await?;
            match found.first().and_then(|r| r.columns.first()) {
                Some(Some(CqlValue::BigInt(n))) => Ok(*n as u64),
                _ => Err(DbError::General("the count came back empty".to_string())),
            }
        })
    }

    fn definition<'a>(
        &'a mut self,
        node: &'a NodePath,
        kind: NodeKind,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move {
            let (keyspace, name) = table(node)?;
            let what = match kind {
                NodeKind::Table => "TABLE",
                NodeKind::MaterializedView => "MATERIALIZED VIEW",
                kind => {
                    return Err(DbError::General(format!(
                        "Cassandra can't show the definition of a {}",
                        kind.label()
                    )))
                }
            };
            let cql = format!("DESCRIBE {what} {}.{}", ident(keyspace), ident(name));
            let found = rows(&self.session, &cql, &[]).await?;
            // The statement is the last text column of each row.
            let statements: Vec<String> = found
                .iter()
                .filter_map(|r| {
                    r.columns.iter().rev().find_map(|c| match c {
                        Some(CqlValue::Text(s)) => Some(s.clone()),
                        _ => None,
                    })
                })
                .collect();
            if statements.is_empty() {
                return Err(DbError::General(format!(
                    "no {} {keyspace}.{name}",
                    kind.label()
                )));
            }
            Ok(format!("{}\n", statements.join("\n\n").trim_end()))
        })
    }

    fn all_columns<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<Vec<TableColumn>, DbError>> {
        Box::pin(async move {
            let [keyspace] = node.parts() else {
                return Err(DbError::General(format!(
                    "not a keyspace: {:?}",
                    node.parts()
                )));
            };
            let found = rows(
                &self.session,
                "SELECT table_name, column_name, kind, position, type, clustering_order \
                 FROM system_schema.columns WHERE keyspace_name = ?",
                &[keyspace],
            )
            .await?;
            let mut by_table: Vec<(String, Vec<Column>)> = Vec::new();
            for r in &found {
                let table = text_at(r, 0);
                let column = Column {
                    name: text_at(r, 1),
                    kind: text_at(r, 2),
                    position: match r.columns.get(3) {
                        Some(Some(CqlValue::Int(n))) => *n,
                        _ => 0,
                    },
                    type_name: text_at(r, 4),
                    order: text_at(r, 5),
                };
                match by_table.iter_mut().find(|(t, _)| *t == table) {
                    Some((_, columns)) => columns.push(column),
                    None => by_table.push((table, vec![column])),
                }
            }
            by_table.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(by_table
                .into_iter()
                .flat_map(|(table, columns)| {
                    ordered(columns).into_iter().map(move |c| TableColumn {
                        table: table.clone(),
                        name: c.name,
                        data_type: c.type_name,
                    })
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, kind: &str, position: i32) -> Column {
        Column {
            name: name.into(),
            kind: kind.into(),
            position,
            type_name: "int".into(),
            order: "none".into(),
        }
    }

    #[test]
    fn columns_come_in_select_star_order() {
        let names: Vec<String> = ordered(vec![
            column("zeta", "regular", -1),
            column("day", "clustering", 0),
            column("tenant", "partition_key", 0),
            column("alpha", "regular", -1),
            column("region", "partition_key", 1),
            column("total", "static", -1),
        ])
        .into_iter()
        .map(|c| c.name)
        .collect();
        assert_eq!(names, ["tenant", "region", "day", "total", "alpha", "zeta"]);
    }

    #[test]
    fn values_bind_as_their_column_type() {
        assert_eq!(
            bind(&OwnedCell::Int(7), "int").expect("int"),
            CqlValue::Int(7)
        );
        assert_eq!(
            bind(&OwnedCell::Text("b".into()), "text").expect("text"),
            CqlValue::Text("b".into())
        );
        assert!(bind(&OwnedCell::Int(1 << 40), "int").is_err());
        assert!(bind(&OwnedCell::Text("x".into()), "uuid").is_err());
        assert_eq!(
            compare(Some(&CqlValue::Int(2)), Some(&CqlValue::BigInt(10))),
            Ordering::Less
        );
    }
}
