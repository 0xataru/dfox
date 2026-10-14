//! The MySQL catalog as a tree: databases ▸ tables, views and routines ▸
//! columns. In MySQL a database is a schema, and one connection reaches all
//! of them, so every database can be browsed. One query per level, read from
//! `information_schema` on the session's metadata connection. Browsing and
//! edits run there too, with values always sent as parameters.

use std::{collections::HashMap, sync::Arc};

use dfox_core::{
    arrow::ResultStream,
    catalog::{
        BrowseQuery, Catalog, ColumnInfo, EntityInfo, FilterOp, IndexInfo, Node, NodeKind,
        NodePath, Order,
    },
    errors::DbError,
    result::OwnedCell,
    session::{ApplyReport, Change, ChangeSet},
};
use futures_util::future::BoxFuture;
use mysql_async::{prelude::Queryable, Conn, Params, Value};
use tokio::sync::Mutex;

use super::{db_error, spawn_run, stream_result};

/// Databases with more tables than this list without row estimates.
const ESTIMATED_TABLES: usize = 1000;

pub struct MyCatalog {
    conn: Arc<Mutex<Conn>>,
}

/// `` `name` ``, with backticks doubled.
pub fn quote(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// A value as a parameter. MySQL converts parameters to the column's type
/// as it compares or stores them.
fn param(value: &OwnedCell) -> Value {
    match value {
        OwnedCell::Null => Value::NULL,
        OwnedCell::Bool(b) => Value::Int(i64::from(*b)),
        OwnedCell::Int(n) => Value::Int(*n),
        OwnedCell::UInt(n) => Value::UInt(*n),
        OwnedCell::Float(f) => Value::Double(*f),
        OwnedCell::Bytes(bytes) => Value::Bytes(bytes.to_vec()),
        other => Value::Bytes(other.clone().into_text().unwrap_or_default().into_bytes()),
    }
}

/// `~12.4k`: a row estimate, short.
fn estimate_text(rows: u64) -> String {
    match rows {
        n if n < 1000 => format!("~{n}"),
        n if n < 1_000_000 => format!("~{:.1}k", n as f64 / 1000.0),
        n => format!("~{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// The database and table of a node path `[database, table]`.
fn table(node: &NodePath) -> Result<(&str, &str), DbError> {
    match node.parts() {
        [database, table] => Ok((database, table)),
        parts => Err(DbError::General(format!("not a table: {parts:?}"))),
    }
}

impl MyCatalog {
    pub fn new(conn: Arc<Mutex<Conn>>) -> Self {
        Self { conn }
    }

    async fn databases(&self) -> Result<Vec<Node>, DbError> {
        let names: Vec<String> = self
            .conn
            .lock()
            .await
            .query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
            .await
            .map_err(db_error)?;
        Ok(names
            .into_iter()
            .map(|name| Node::new(NodeKind::Database, name, true))
            .collect())
    }

    async fn objects(&self, database: &str) -> Result<Vec<Node>, DbError> {
        let mut conn = self.conn.lock().await;
        let rows: Vec<(String, String)> = conn
            .exec(
                "SELECT TABLE_NAME, TABLE_TYPE \
                 FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? \
                 UNION ALL \
                 SELECT ROUTINE_NAME, ROUTINE_TYPE \
                 FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? \
                 ORDER BY 1, 2",
                (database, database),
            )
            .await
            .map_err(db_error)?;
        // Row estimates open every table the server hasn't cached statistics
        // for, about 0.1 ms each, so a huge database lists without them.
        let estimates: HashMap<String, u64> = if rows.len() <= ESTIMATED_TABLES {
            conn.exec::<(String, Option<u64>), _, _>(
                "SELECT TABLE_NAME, TABLE_ROWS FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'",
                (database,),
            )
            .await
            .map_err(db_error)?
            .into_iter()
            .filter_map(|(name, rows)| Some((name, rows?)))
            .collect()
        } else {
            HashMap::new()
        };
        Ok(rows
            .into_iter()
            .map(|(name, kind)| {
                let kind = match kind.as_str() {
                    "BASE TABLE" => NodeKind::Table,
                    "VIEW" | "SYSTEM VIEW" => NodeKind::View,
                    "PROCEDURE" => NodeKind::Procedure,
                    "FUNCTION" => NodeKind::Function,
                    _ => NodeKind::Other,
                };
                let estimate = estimates.get(&name).filter(|_| kind == NodeKind::Table);
                let node = Node::new(kind, name.clone(), kind.has_rows());
                match estimate {
                    Some(&rows) => node.with_detail(estimate_text(rows)),
                    None => node,
                }
            })
            .collect())
    }

    async fn columns(&self, database: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
        let rows: Vec<(String, String, String, Option<String>, String)> = self
            .conn
            .lock()
            .await
            .exec(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY \
                 FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|(name, type_name, nullable, default, key)| ColumnInfo {
                name,
                type_name,
                nullable: nullable == "YES",
                default,
                primary_key: key == "PRI",
            })
            .collect())
    }

    async fn describe_table(&self, database: &str, table: &str) -> Result<EntityInfo, DbError> {
        let columns = self.columns(database, table).await?;
        if columns.is_empty() {
            return Err(DbError::General(format!("no table {database}.{table}")));
        }
        let mut conn = self.conn.lock().await;
        let parts: Vec<(String, i64, String)> = conn
            .exec(
                "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME FROM information_schema.STATISTICS \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let mut indexes: Vec<IndexInfo> = Vec::new();
        for (name, non_unique, column) in parts {
            match indexes.last_mut() {
                Some(index) if index.name == name => index.columns.push(column),
                _ => indexes.push(IndexInfo {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                }),
            }
        }
        let row_estimate: Option<Option<u64>> = conn
            .exec_first(
                "SELECT TABLE_ROWS FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        Ok(EntityInfo {
            columns,
            indexes,
            row_estimate: row_estimate.flatten(),
        })
    }

    /// Fails unless every name in `names` is a column of the table, so
    /// a wrong name is a clear error rather than a SQL one.
    async fn check_columns<'a>(
        &self,
        database: &str,
        table: &str,
        names: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), DbError> {
        let columns = self.columns(database, table).await?;
        if columns.is_empty() {
            return Err(DbError::General(format!("no table {database}.{table}")));
        }
        for name in names {
            if !columns.iter().any(|c| &c.name == name) {
                return Err(DbError::General(format!("no column {name} in {table}")));
            }
        }
        Ok(())
    }

    /// Applies `changes` in a transaction on the metadata connection.
    pub async fn apply(&self, changes: ChangeSet) -> Result<ApplyReport, DbError> {
        let (database, table) = table(&changes.table)?;
        let mentioned =
            changes
                .key
                .iter()
                .chain(&changes.columns)
                .chain(changes.changes.iter().flat_map(|change| match change {
                    Change::Update { set, .. } => set.iter().map(|(c, _)| c).collect(),
                    _ => Vec::new(),
                }));
        self.check_columns(database, table, mentioned.collect::<Vec<_>>())
            .await?;
        let target = format!("{}.{}", quote(database), quote(table));
        let key_clause = changes
            .key
            .iter()
            .map(|k| format!("{} <=> ?", quote(k)))
            .collect::<Vec<_>>()
            .join(" AND ");

        let mut conn = self.conn.lock().await;
        conn.query_drop("START TRANSACTION")
            .await
            .map_err(db_error)?;
        let result = async {
            let mut report = ApplyReport::default();
            for change in &changes.changes {
                let (sql, values): (String, Vec<Value>) = match change {
                    Change::Insert(values) => {
                        let columns: Vec<String> =
                            changes.columns.iter().map(|c| quote(c)).collect();
                        let marks = vec!["?"; columns.len()].join(", ");
                        (
                            format!(
                                "INSERT INTO {target} ({}) VALUES ({marks})",
                                columns.join(", ")
                            ),
                            values.iter().map(param).collect(),
                        )
                    }
                    Change::Update { key, set } => {
                        let assignments: Vec<String> = set
                            .iter()
                            .map(|(c, _)| format!("{} = ?", quote(c)))
                            .collect();
                        let mut values: Vec<Value> = set.iter().map(|(_, v)| param(v)).collect();
                        values.extend(key.iter().map(param));
                        (
                            format!(
                                "UPDATE {target} SET {} WHERE {key_clause}",
                                assignments.join(", ")
                            ),
                            values,
                        )
                    }
                    Change::Delete(key) => (
                        format!("DELETE FROM {target} WHERE {key_clause}"),
                        key.iter().map(param).collect(),
                    ),
                };
                conn.exec_drop(sql, Params::Positional(values))
                    .await
                    .map_err(db_error)?;
                let rows = conn.affected_rows();
                match change {
                    Change::Insert(_) => report.inserted += rows,
                    Change::Update { .. } => report.updated += rows,
                    Change::Delete(_) => report.deleted += rows,
                }
            }
            Ok::<_, DbError>(report)
        }
        .await;
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        conn.query_drop(end).await.map_err(db_error)?;
        result
    }
}

impl Catalog for MyCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        Box::pin(async move {
            match node.parts() {
                [] => self.databases().await,
                [database] => self.objects(database).await,
                [database, table] => Ok(self
                    .columns(database, table)
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
            let (database, table) = table(node)?;
            self.describe_table(database, table).await
        })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        Box::pin(async move {
            let (database, table) = table(node)?;
            self.check_columns(
                database,
                table,
                query
                    .filters
                    .iter()
                    .map(|f| &f.column)
                    .chain(query.sort.iter().map(|(c, _)| c)),
            )
            .await?;
            let mut params: Vec<Value> = Vec::new();
            let mut conditions = Vec::new();
            for filter in &query.filters {
                let column = quote(&filter.column);
                let mut placeholder = || {
                    params.push(param(&filter.value));
                    "?"
                };
                conditions.push(match filter.op {
                    FilterOp::IsNull => format!("{column} IS NULL"),
                    FilterOp::NotNull => format!("{column} IS NOT NULL"),
                    FilterOp::Contains => format!(
                        "LOCATE(LOWER({}), LOWER(CAST({column} AS CHAR))) > 0",
                        placeholder()
                    ),
                    op => {
                        let sign = match op {
                            FilterOp::Eq => "=",
                            FilterOp::Ne => "<>",
                            FilterOp::Lt => "<",
                            FilterOp::Le => "<=",
                            FilterOp::Gt => ">",
                            _ => ">=",
                        };
                        format!("{column} {sign} {}", placeholder())
                    }
                });
            }
            let mut sql = format!("SELECT * FROM {}.{}", quote(database), quote(table));
            if !conditions.is_empty() {
                sql.push_str(&format!(" WHERE {}", conditions.join(" AND ")));
            }
            if !query.sort.is_empty() {
                let order: Vec<String> = query
                    .sort
                    .iter()
                    .map(|(column, order)| {
                        let direction = match order {
                            Order::Ascending => "ASC",
                            Order::Descending => "DESC",
                        };
                        format!("{} {direction}", quote(column))
                    })
                    .collect();
                sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
            }
            // MySQL takes an offset only with a limit.
            match (query.limit, query.offset) {
                (Some(limit), 0) => sql.push_str(&format!(" LIMIT {limit}")),
                (limit, offset) if offset > 0 => sql.push_str(&format!(
                    " LIMIT {} OFFSET {offset}",
                    limit.unwrap_or(u64::MAX)
                )),
                _ => {}
            }
            let conn = self.conn.clone();
            Ok(spawn_run(move |tx| async move {
                let mut conn = conn.lock().await;
                let result = conn
                    .exec_iter(sql, Params::Positional(params))
                    .await
                    .map_err(db_error)?;
                stream_result(result, usize::MAX, &tx).await
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_quoted_and_estimates_short() {
        assert_eq!(quote("we`ird"), "`we``ird`");
        assert_eq!(estimate_text(999), "~999");
        assert_eq!(estimate_text(12_400), "~12.4k");
        assert_eq!(estimate_text(3_000_000), "~3.0M");
    }

    #[test]
    fn values_become_parameters() {
        assert_eq!(param(&OwnedCell::Null), Value::NULL);
        assert_eq!(param(&OwnedCell::Int(7)), Value::Int(7));
        assert_eq!(param(&OwnedCell::Bool(true)), Value::Int(1));
        assert_eq!(
            param(&OwnedCell::Text("b".into())),
            Value::Bytes(b"b".to_vec())
        );
    }

    #[test]
    fn only_two_part_paths_are_tables() {
        let path: NodePath = ["shop", "orders"].into_iter().collect();
        assert_eq!(table(&path).ok(), Some(("shop", "orders")));
        assert!(table(&NodePath::root().child("shop")).is_err());
    }
}
