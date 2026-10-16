//! The SQLite catalog as a tree: databases (`main`, `temp` and attached
//! ones) ▸ tables and views ▸ columns, read from the `pragma_*` table
//! functions on the session's connection. Browsing and edits run there too,
//! with values always bound as parameters.

use dfox_core::{
    arrow::ResultStream,
    catalog::{
        BrowseQuery, Catalog, ColumnInfo, EntityInfo, FilterOp, IndexInfo, Node, NodeKind,
        NodePath, Order,
    },
    errors::DbError,
    models::schema::TableColumn,
    result::OwnedCell,
    session::{ApplyReport, Change, ChangeSet},
};
use futures_util::future::BoxFuture;
use rusqlite::{params, types::Value, Connection};

use super::{db_error, Worker};

pub struct SqliteCatalog {
    worker: Worker,
}

/// `"name"`, with quotes doubled.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A value as a parameter.
fn param(value: &OwnedCell) -> Value {
    match value {
        OwnedCell::Null => Value::Null,
        OwnedCell::Bool(b) => Value::Integer(i64::from(*b)),
        OwnedCell::Int(n) => Value::Integer(*n),
        // Past i64, SQLite keeps it as a real anyway.
        OwnedCell::UInt(n) => i64::try_from(*n).map_or(Value::Real(*n as f64), Value::Integer),
        OwnedCell::Float(f) => Value::Real(*f),
        OwnedCell::Bytes(bytes) => Value::Blob(bytes.to_vec()),
        other => Value::Text(other.clone().into_text().unwrap_or_default()),
    }
}

/// The database and table of a node path `[database, table]`.
fn table(node: &NodePath) -> Result<(String, String), DbError> {
    match node.parts() {
        [database, table] => Ok((database.clone(), table.clone())),
        parts => Err(DbError::General(format!("not a table: {parts:?}"))),
    }
}

fn databases(conn: &Connection) -> Result<Vec<Node>, DbError> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_database_list ORDER BY seq")
        .map_err(db_error)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_error)?;
    names
        .map(|name| Ok(Node::new(NodeKind::Database, name.map_err(db_error)?, true)))
        .collect()
}

fn objects(conn: &Connection, database: &str) -> Result<Vec<Node>, DbError> {
    // Shadow tables belong to virtual tables, and sqlite_* to SQLite.
    let mut stmt = conn
        .prepare(
            "SELECT name, type FROM pragma_table_list \
             WHERE schema = ?1 AND type IN ('table', 'view', 'virtual') \
               AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY name",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map([database], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db_error)?;
    rows.map(|row| {
        let (name, kind) = row.map_err(db_error)?;
        let kind = match kind.as_str() {
            "view" => NodeKind::View,
            _ => NodeKind::Table,
        };
        Ok(Node::new(kind, name, true))
    })
    .collect()
}

fn columns(conn: &Connection, database: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk \
             FROM pragma_table_info(?1, ?2) ORDER BY cid",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map(params![table, database], |row| {
            Ok(ColumnInfo {
                name: row.get(0)?,
                type_name: row.get(1)?,
                nullable: row.get::<_, i64>(2)? == 0,
                default: row.get(3)?,
                primary_key: row.get::<_, i64>(4)? > 0,
            })
        })
        .map_err(db_error)?;
    rows.map(|row| row.map_err(db_error)).collect()
}

fn describe(conn: &Connection, database: &str, table: &str) -> Result<EntityInfo, DbError> {
    let columns = columns(conn, database, table)?;
    if columns.is_empty() {
        return Err(DbError::General(format!("no table {database}.{table}")));
    }
    let mut stmt = conn
        .prepare(
            "SELECT il.name, il.\"unique\", ii.name \
             FROM pragma_index_list(?1, ?2) AS il \
             JOIN pragma_index_info(il.name, ?2) AS ii \
             ORDER BY il.name, ii.seqno",
        )
        .map_err(db_error)?;
    let parts = stmt
        .query_map(params![table, database], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? == 1,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(db_error)?;
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for part in parts {
        let (name, unique, column) = part.map_err(db_error)?;
        // An index on an expression has no column name.
        let column = column.unwrap_or_else(|| "(expression)".into());
        match indexes.last_mut() {
            Some(index) if index.name == name => index.columns.push(column),
            _ => indexes.push(IndexInfo {
                name,
                columns: vec![column],
                unique,
            }),
        }
    }
    Ok(EntityInfo {
        columns,
        indexes,
        row_estimate: None,
    })
}

fn all_columns(conn: &Connection, database: &str) -> Result<Vec<TableColumn>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT t.name, c.name, c.type \
             FROM pragma_table_list AS t \
             JOIN pragma_table_info(t.name, t.schema) AS c \
             WHERE t.schema = ?1 AND t.type IN ('table', 'view', 'virtual') \
               AND t.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY t.name, c.cid",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map([database], |row| {
            Ok(TableColumn {
                table: row.get(0)?,
                name: row.get(1)?,
                data_type: row.get(2)?,
            })
        })
        .map_err(db_error)?;
    rows.map(|row| row.map_err(db_error)).collect()
}

/// Fails unless every name in `names` is a column of the table, so a wrong
/// name is a clear error rather than a SQL one.
fn check_columns<'a>(
    conn: &Connection,
    database: &str,
    table: &str,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<(), DbError> {
    let columns = columns(conn, database, table)?;
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

/// Applies `changes` inside a savepoint, which works whether or not the
/// session has a transaction open.
fn apply(conn: &Connection, changes: &ChangeSet) -> Result<ApplyReport, DbError> {
    let (database, table) = table(&changes.table)?;
    let mentioned: Vec<&String> = changes
        .key
        .iter()
        .chain(&changes.columns)
        .chain(changes.changes.iter().flat_map(|change| match change {
            Change::Update { set, .. } => set.iter().map(|(c, _)| c).collect(),
            _ => Vec::new(),
        }))
        .collect();
    check_columns(conn, &database, &table, mentioned)?;
    let target = format!("{}.{}", quote(&database), quote(&table));
    let key_clause = |first: usize| {
        changes
            .key
            .iter()
            .enumerate()
            .map(|(i, k)| format!("{} IS ?{}", quote(k), first + i))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    conn.execute_batch("SAVEPOINT dfox_apply")
        .map_err(db_error)?;
    let result = (|| {
        let mut report = ApplyReport::default();
        for change in &changes.changes {
            let (sql, values): (String, Vec<Value>) = match change {
                Change::Insert(values) => {
                    let columns: Vec<String> = changes.columns.iter().map(|c| quote(c)).collect();
                    let marks: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
                    (
                        format!(
                            "INSERT INTO {target} ({}) VALUES ({})",
                            columns.join(", "),
                            marks.join(", ")
                        ),
                        values.iter().map(param).collect(),
                    )
                }
                Change::Update { key, set } => {
                    let assignments: Vec<String> = set
                        .iter()
                        .enumerate()
                        .map(|(i, (c, _))| format!("{} = ?{}", quote(c), i + 1))
                        .collect();
                    let mut values: Vec<Value> = set.iter().map(|(_, v)| param(v)).collect();
                    values.extend(key.iter().map(param));
                    (
                        format!(
                            "UPDATE {target} SET {} WHERE {}",
                            assignments.join(", "),
                            key_clause(set.len() + 1)
                        ),
                        values,
                    )
                }
                Change::Delete(key) => (
                    format!("DELETE FROM {target} WHERE {}", key_clause(1)),
                    key.iter().map(param).collect(),
                ),
            };
            let rows = conn
                .execute(&sql, rusqlite::params_from_iter(values))
                .map_err(db_error)? as u64;
            match change {
                Change::Insert(_) => report.inserted += rows,
                Change::Update { .. } => report.updated += rows,
                Change::Delete(_) => report.deleted += rows,
            }
        }
        Ok::<_, DbError>(report)
    })();
    let end = if result.is_ok() {
        "RELEASE dfox_apply"
    } else {
        "ROLLBACK TO dfox_apply; RELEASE dfox_apply"
    };
    conn.execute_batch(end).map_err(db_error)?;
    result
}

/// The SQL and parameters that browse a table.
fn browse_sql(database: &str, table: &str, query: &BrowseQuery) -> (String, Vec<Value>) {
    let mut params: Vec<Value> = Vec::new();
    let mut conditions = Vec::new();
    for filter in &query.filters {
        let column = quote(&filter.column);
        let mut placeholder = || {
            params.push(param(&filter.value));
            format!("?{}", params.len())
        };
        conditions.push(match filter.op {
            FilterOp::IsNull => format!("{column} IS NULL"),
            FilterOp::NotNull => format!("{column} IS NOT NULL"),
            FilterOp::Contains => format!("instr(lower({column}), lower({})) > 0", placeholder()),
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
    // SQLite takes an offset only with a limit; -1 is none.
    match (query.limit, query.offset) {
        (Some(limit), 0) => sql.push_str(&format!(" LIMIT {limit}")),
        (limit, offset) if offset > 0 => {
            let limit = limit.map_or(-1, |l| i64::try_from(l).unwrap_or(i64::MAX));
            sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
        }
        _ => {}
    }
    (sql, params)
}

impl SqliteCatalog {
    pub(crate) fn new(worker: Worker) -> Self {
        Self { worker }
    }

    pub async fn apply(&self, changes: ChangeSet) -> Result<ApplyReport, DbError> {
        self.worker.call(move |conn| apply(conn, &changes)).await
    }
}

impl Catalog for SqliteCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        let parts = node.parts().to_vec();
        Box::pin(self.worker.call(move |conn| {
            match parts.as_slice() {
                [] => databases(conn),
                [database] => objects(conn, database),
                [database, table] => Ok(columns(conn, database, table)?
                    .into_iter()
                    .map(|c| {
                        let node = Node::new(NodeKind::Column, c.name, false);
                        match c.type_name.as_str() {
                            "" => node,
                            type_name => node.with_detail(type_name),
                        }
                    })
                    .collect()),
                parts => Err(DbError::General(format!("nothing under {parts:?}"))),
            }
        }))
    }

    fn describe<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<EntityInfo, DbError>> {
        let target = table(node);
        Box::pin(async move {
            let (database, table) = target?;
            self.worker
                .call(move |conn| describe(conn, &database, &table))
                .await
        })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        let target = table(node);
        Box::pin(async move {
            let (database, table) = target?;
            let (sql, params) = self
                .worker
                .call(move |conn| {
                    let names = query
                        .filters
                        .iter()
                        .map(|f| &f.column)
                        .chain(query.sort.iter().map(|(c, _)| c));
                    check_columns(conn, &database, &table, names)?;
                    Ok(browse_sql(&database, &table, &query))
                })
                .await?;
            self.worker.query(sql, params)
        })
    }
    fn all_columns<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<Vec<TableColumn>, DbError>> {
        let parts = node.parts().to_vec();
        Box::pin(async move {
            let [database] = parts.as_slice() else {
                return Err(DbError::General(format!("not a database: {parts:?}")));
            };
            let database = database.clone();
            self.worker
                .call(move |conn| all_columns(conn, &database))
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_quoted_and_values_bound() {
        assert_eq!(quote("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(param(&OwnedCell::Bool(true)), Value::Integer(1));
        assert_eq!(
            param(&OwnedCell::UInt(u64::MAX)),
            Value::Real(u64::MAX as f64)
        );
        assert_eq!(param(&OwnedCell::Text("b".into())), Value::Text("b".into()));
    }

    #[test]
    fn browsing_pages_with_or_without_a_limit() {
        let query = BrowseQuery {
            offset: 5,
            ..Default::default()
        };
        let (sql, _) = browse_sql("main", "t", &query);
        assert!(sql.ends_with("LIMIT -1 OFFSET 5"), "{sql}");
        let query = BrowseQuery {
            limit: Some(2),
            ..Default::default()
        };
        assert!(browse_sql("main", "t", &query).0.ends_with("LIMIT 2"));
    }
}
