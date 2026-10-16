//! The Postgres catalog as a tree: databases ▸ schemas ▸ tables, views and
//! routines ▸ columns. One query per level, read from `pg_catalog`, on the
//! session's metadata connection. Browsing and edits run there too, with
//! values always sent as parameters.

use std::{collections::HashMap, sync::Arc};

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
use tokio_postgres::Client;

use super::{db_error, execute, spawn_run};

/// Schemas Postgres keeps for itself.
const SYSTEM_SCHEMAS: &str = "n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'";

pub struct PgCatalog {
    client: Arc<Client>,
    /// The database the session is connected to; only its schemas can be
    /// listed.
    database: String,
}

/// `"name"`, with quotes doubled.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A value as a text parameter: NULL is `None`, bytes are `\x…` hex.
fn param(value: &OwnedCell) -> Option<String> {
    match value {
        OwnedCell::Bytes(bytes) => Some(format!(
            "\\x{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )),
        other => other.clone().into_text(),
    }
}

/// `~12.4k`: a row estimate, short.
fn estimate(rows: i64) -> String {
    match rows {
        n if n < 1000 => format!("~{n}"),
        n if n < 1_000_000 => format!("~{:.1}k", n as f64 / 1000.0),
        n => format!("~{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// `(a integer, b text) → integer`: a routine's arguments and result.
fn signature(arguments: Option<String>, result: Option<String>) -> String {
    let arguments = arguments.unwrap_or_default();
    match result {
        Some(result) if !result.is_empty() => format!("({arguments}) → {result}"),
        _ => format!("({arguments})"),
    }
}

impl PgCatalog {
    pub fn new(client: Arc<Client>, database: String) -> Self {
        Self { client, database }
    }

    /// The schema and table of a node path `[database, schema, table]`.
    fn table<'a>(&self, node: &'a NodePath) -> Result<(&'a str, &'a str), DbError> {
        match node.parts() {
            [database, schema, table] if *database == self.database => Ok((schema, table)),
            [database, ..] if *database != self.database => Err(DbError::Config(format!(
                "connect to {database} to browse it"
            ))),
            parts => Err(DbError::General(format!("not a table: {parts:?}"))),
        }
    }

    async fn databases(&self) -> Result<Vec<Node>, DbError> {
        let rows = self
            .client
            .query(
                "SELECT datname::text FROM pg_database \
                 WHERE datallowconn AND NOT datistemplate ORDER BY datname",
                &[],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let current = name == self.database;
                Node::new(NodeKind::Database, name, current)
            })
            .collect())
    }

    async fn schemas(&self) -> Result<Vec<Node>, DbError> {
        let sql = format!(
            "SELECT n.nspname::text FROM pg_namespace n WHERE {SYSTEM_SCHEMAS} \
             ORDER BY n.nspname <> 'public', n.nspname"
        );
        let rows = self.client.query(&sql, &[]).await.map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| Node::new(NodeKind::Schema, row.get::<_, String>(0), true))
            .collect())
    }

    async fn objects(&self, schema: &str) -> Result<Vec<Node>, DbError> {
        let rows = self
            .client
            .query(
                "SELECT c.relname::text, c.relkind::text, \
                        CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END, \
                        NULL::text, NULL::text \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1::text AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                 UNION ALL \
                 SELECT p.proname::text, \
                        CASE p.prokind WHEN 'p' THEN 'P' ELSE 'F' END, count(*), \
                        min(pg_get_function_identity_arguments(p.oid)), \
                        min(pg_get_function_result(p.oid)) \
                 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                 WHERE n.nspname = $1::text AND p.prokind IN ('f', 'p') \
                 GROUP BY 1, 2 \
                 ORDER BY 1, 2",
                &[&schema],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let kind = match row.get::<_, String>(1).as_str() {
                    "r" | "p" => NodeKind::Table,
                    "v" => NodeKind::View,
                    "m" => NodeKind::MaterializedView,
                    "f" => NodeKind::ForeignTable,
                    "P" => NodeKind::Procedure,
                    _ => NodeKind::Function,
                };
                let node = Node::new(kind, name, kind.has_rows());
                // Tables have a row estimate; routines a count of overloads.
                let number = row.get::<_, Option<i64>>(2);
                match (kind, number) {
                    (NodeKind::Table, Some(rows)) => node.with_detail(estimate(rows)),
                    (NodeKind::Function | NodeKind::Procedure, Some(1)) => {
                        node.with_detail(signature(row.get(3), row.get(4)))
                    }
                    (NodeKind::Function | NodeKind::Procedure, Some(n)) => {
                        node.with_detail(format!("{n} overloads"))
                    }
                    _ => node,
                }
            })
            .collect())
    }

    async fn columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
        let rows = self
            .client
            .query(
                "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), \
                        NOT a.attnotnull, pg_get_expr(d.adbin, d.adrelid), \
                        COALESCE(a.attnum = ANY(i.indkey), false) \
                 FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 LEFT JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary \
                 WHERE n.nspname = $1::text AND c.relname = $2::text \
                   AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| ColumnInfo {
                name: row.get(0),
                type_name: row.get(1),
                nullable: row.get(2),
                default: row.get(3),
                primary_key: row.get(4),
            })
            .collect())
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<EntityInfo, DbError> {
        let columns = self.columns(schema, table).await?;
        if columns.is_empty() {
            return Err(DbError::General(format!("no table {schema}.{table}")));
        }
        let indexes = self
            .client
            .query(
                "SELECT ic.relname::text, i.indisunique, \
                        ARRAY(SELECT a.attname::text \
                              FROM unnest(i.indkey) WITH ORDINALITY AS k(num, ord) \
                              JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.num \
                              ORDER BY k.ord) \
                 FROM pg_index i \
                 JOIN pg_class ic ON ic.oid = i.indexrelid \
                 JOIN pg_class c ON c.oid = i.indrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1::text AND c.relname = $2::text \
                 ORDER BY 1",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?
            .iter()
            .map(|row| IndexInfo {
                name: row.get(0),
                unique: row.get(1),
                columns: row.get(2),
            })
            .collect();
        let estimate = self
            .client
            .query_opt(
                "SELECT CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1::text AND c.relname = $2::text",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?
            .and_then(|row| row.get::<_, Option<i64>>(0))
            .map(|n| n as u64);
        Ok(EntityInfo {
            columns,
            indexes,
            row_estimate: estimate,
        })
    }

    /// The SQL that creates a view or every overload of a routine.
    async fn definition(&self, node: &NodePath, kind: NodeKind) -> Result<String, DbError> {
        let (schema, name) = self.table(node)?;
        let missing = || DbError::General(format!("no {} {schema}.{name}", kind.label()));
        match kind {
            NodeKind::Function | NodeKind::Procedure => {
                let prokind = if kind == NodeKind::Procedure {
                    "p"
                } else {
                    "f"
                };
                let rows = self
                    .client
                    .query(
                        "SELECT pg_get_functiondef(p.oid) \
                         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                         WHERE n.nspname = $1::text AND p.proname = $2::text \
                           AND p.prokind = $3::text::\"char\" \
                         ORDER BY p.oid",
                        &[&schema, &name, &prokind],
                    )
                    .await
                    .map_err(db_error)?;
                if rows.is_empty() {
                    return Err(missing());
                }
                Ok(rows
                    .iter()
                    .map(|row| format!("{};\n", row.get::<_, String>(0).trim_end()))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            NodeKind::View | NodeKind::MaterializedView => {
                let relkind = if kind == NodeKind::View { "v" } else { "m" };
                let body: String = self
                    .client
                    .query_opt(
                        "SELECT pg_get_viewdef(c.oid, true) \
                         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = $1::text AND c.relname = $2::text \
                           AND c.relkind = $3::text::\"char\"",
                        &[&schema, &name, &relkind],
                    )
                    .await
                    .map_err(db_error)?
                    .ok_or_else(missing)?
                    .get(0);
                let create = if kind == NodeKind::View {
                    "CREATE OR REPLACE VIEW"
                } else {
                    "CREATE MATERIALIZED VIEW"
                };
                let ident = |name| dfox_sql::quote_ident(name, dfox_sql::Dialect::Postgres);
                Ok(format!(
                    "{create} {}.{} AS\n{}\n",
                    ident(schema),
                    ident(name),
                    body.trim_end()
                ))
            }
            kind => Err(DbError::General(format!(
                "Postgres can't show the definition of a {}",
                kind.label()
            ))),
        }
    }

    /// Each column's type, to cast text parameters to.
    async fn types(&self, schema: &str, table: &str) -> Result<HashMap<String, String>, DbError> {
        Ok(self
            .columns(schema, table)
            .await?
            .into_iter()
            .map(|c| (c.name, c.type_name))
            .collect())
    }

    /// Applies `changes` in a transaction on the metadata connection.
    pub async fn apply(&self, changes: ChangeSet) -> Result<ApplyReport, DbError> {
        let (schema, table) = self.table(&changes.table)?;
        let types = self.types(schema, table).await?;
        let target = format!("{}.{}", quote(schema), quote(table));
        let cast = |column: &str, n: usize| -> Result<String, DbError> {
            let ty = types
                .get(column)
                .ok_or_else(|| DbError::General(format!("no column {column} in {table}")))?;
            Ok(format!("CAST(${n}::text AS {ty})"))
        };
        let key_clause = |first: usize| -> Result<String, DbError> {
            changes
                .key
                .iter()
                .enumerate()
                .map(|(i, k)| Ok(format!("{} = {}", quote(k), cast(k, first + i)?)))
                .collect::<Result<Vec<_>, DbError>>()
                .map(|parts| parts.join(" AND "))
        };

        let client = &self.client;
        client.batch_execute("BEGIN").await.map_err(db_error)?;
        let result = async {
            let mut report = ApplyReport::default();
            for change in &changes.changes {
                let (sql, values): (String, Vec<Option<String>>) = match change {
                    Change::Insert(values) => {
                        let columns: Vec<String> =
                            changes.columns.iter().map(|c| quote(c)).collect();
                        let casts = changes
                            .columns
                            .iter()
                            .enumerate()
                            .map(|(i, c)| cast(c, i + 1))
                            .collect::<Result<Vec<_>, _>>()?;
                        (
                            format!(
                                "INSERT INTO {target} ({}) VALUES ({})",
                                columns.join(", "),
                                casts.join(", ")
                            ),
                            values.iter().map(param).collect(),
                        )
                    }
                    Change::Update { key, set } => {
                        let assignments = set
                            .iter()
                            .enumerate()
                            .map(|(i, (c, _))| Ok(format!("{} = {}", quote(c), cast(c, i + 1)?)))
                            .collect::<Result<Vec<_>, DbError>>()?;
                        let mut values: Vec<Option<String>> =
                            set.iter().map(|(_, v)| param(v)).collect();
                        values.extend(key.iter().map(param));
                        (
                            format!(
                                "UPDATE {target} SET {} WHERE {}",
                                assignments.join(", "),
                                key_clause(set.len() + 1)?
                            ),
                            values,
                        )
                    }
                    Change::Delete(key) => (
                        format!("DELETE FROM {target} WHERE {}", key_clause(1)?),
                        key.iter().map(param).collect(),
                    ),
                };
                let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = values
                    .iter()
                    .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
                    .collect();
                let rows = client.execute(&sql, &params).await.map_err(db_error)?;
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
        client.batch_execute(end).await.map_err(db_error)?;
        result
    }
}

impl Catalog for PgCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        Box::pin(async move {
            match node.parts() {
                [] => self.databases().await,
                [database] if *database == self.database => self.schemas().await,
                [database] => Err(DbError::Config(format!(
                    "connect to {database} to browse it"
                ))),
                [database, schema] if *database == self.database => self.objects(schema).await,
                [_, schema, table] => {
                    self.table(node)?;
                    Ok(self
                        .columns(schema, table)
                        .await?
                        .into_iter()
                        .map(|c| {
                            Node::new(NodeKind::Column, c.name, false).with_detail(c.type_name)
                        })
                        .collect())
                }
                parts => Err(DbError::General(format!("nothing under {parts:?}"))),
            }
        })
    }

    fn describe<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<EntityInfo, DbError>> {
        Box::pin(async move {
            let (schema, table) = self.table(node)?;
            self.describe_table(schema, table).await
        })
    }

    fn definition<'a>(
        &'a mut self,
        node: &'a NodePath,
        kind: NodeKind,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move { PgCatalog::definition(self, node, kind).await })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        Box::pin(async move {
            let (schema, table) = self.table(node)?;
            let types = self.types(schema, table).await?;
            let mut params: Vec<Option<String>> = Vec::new();
            let mut conditions = Vec::new();
            for filter in &query.filters {
                let ty = types.get(&filter.column).ok_or_else(|| {
                    DbError::General(format!("no column {} in {table}", filter.column))
                })?;
                let column = quote(&filter.column);
                let mut placeholder = || {
                    params.push(param(&filter.value));
                    format!("${}", params.len())
                };
                conditions.push(match filter.op {
                    FilterOp::IsNull => format!("{column} IS NULL"),
                    FilterOp::NotNull => format!("{column} IS NOT NULL"),
                    FilterOp::Contains => format!(
                        "strpos(lower({column}::text), lower({}::text)) > 0",
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
                        format!("{column} {sign} CAST({}::text AS {ty})", placeholder())
                    }
                });
            }
            let mut sql = format!("SELECT * FROM {}.{}", quote(schema), quote(table));
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
            if let Some(limit) = query.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            if query.offset > 0 {
                sql.push_str(&format!(" OFFSET {}", query.offset));
            }
            let client = self.client.clone();
            Ok(spawn_run(move |tx| async move {
                execute(&client, &sql, &params, usize::MAX, &tx).await
            }))
        })
    }

    fn all_columns<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<Vec<TableColumn>, DbError>> {
        Box::pin(async move {
            let schema = match node.parts() {
                [database, schema] if *database == self.database => schema,
                parts => return Err(DbError::General(format!("not a schema: {parts:?}"))),
            };
            let rows = self
                .client
                .query(
                    "SELECT c.relname::text, a.attname::text, \
                            format_type(a.atttypid, a.atttypmod) \
                     FROM pg_attribute a \
                     JOIN pg_class c ON c.oid = a.attrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = $1::text AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                       AND a.attnum > 0 AND NOT a.attisdropped \
                     ORDER BY c.relname, a.attnum",
                    &[&schema],
                )
                .await
                .map_err(db_error)?;
            Ok(rows
                .iter()
                .map(|row| TableColumn {
                    table: row.get(0),
                    name: row.get(1),
                    data_type: row.get(2),
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_quoted_and_estimates_short() {
        assert_eq!(quote("Weird \"name\""), "\"Weird \"\"name\"\"\"");
        assert_eq!(estimate(999), "~999");
        assert_eq!(estimate(12_400), "~12.4k");
        assert_eq!(estimate(3_000_000), "~3.0M");
    }

    #[test]
    fn values_become_text_parameters() {
        assert_eq!(param(&OwnedCell::Null), None);
        assert_eq!(param(&OwnedCell::Int(7)).as_deref(), Some("7"));
        assert_eq!(
            param(&OwnedCell::Bytes(vec![0xde, 0xad].into())).as_deref(),
            Some("\\xdead")
        );
        assert_eq!(param(&OwnedCell::Bool(true)).as_deref(), Some("true"));
    }
}
