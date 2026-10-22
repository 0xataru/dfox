//! The Postgres catalog as a tree: databases ▸ schemas ▸ tables, views and
//! routines ▸ columns. One query per level, read from `pg_catalog`, on the
//! session's metadata connection. Browsing and edits run there too, with
//! values always sent as parameters.

use std::{collections::HashMap, sync::Arc};

use dfox_core::{
    arrow::ResultStream,
    catalog::{
        BrowseQuery, Bucket, Catalog, ColumnInfo, ColumnStats, ConstraintInfo, ConstraintKind,
        Distinct, EntityInfo, FilterOp, ForeignKey, IndexInfo, Node, NodeKind, NodePath, Order,
        RuleInfo, StatsSource, PROFILE_ROWS,
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

/// `inserts`, a run of inserts naming the same columns, sent as one COPY in
/// text format: the server casts each column from text, as the single
/// inserts' casts do. Returns how many rows went in.
async fn copy_in(
    client: &tokio_postgres::Client,
    target: &str,
    inserts: &[Change],
) -> Result<u64, DbError> {
    use futures_util::SinkExt;

    let Some(Change::Insert(first)) = inserts.first() else {
        return Ok(0);
    };
    let columns: Vec<String> = first.iter().map(|(c, _)| quote(c)).collect();
    let mut text = String::new();
    for change in inserts {
        let Change::Insert(values) = change else {
            continue;
        };
        for (i, (_, value)) in values.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            match param(value) {
                None => text.push_str("\\N"),
                Some(value) => copy_escape(&value, &mut text),
            }
        }
        text.push('\n');
    }
    let sink = client
        .copy_in::<_, bytes::Bytes>(&format!(
            "COPY {target} ({}) FROM STDIN",
            columns.join(", ")
        ))
        .await
        .map_err(db_error)?;
    futures_util::pin_mut!(sink);
    sink.send(bytes::Bytes::from(text))
        .await
        .map_err(db_error)?;
    sink.finish().await.map_err(db_error)
}

/// `value` as COPY's text format writes it: backslash, tab and line breaks
/// escaped.
fn copy_escape(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
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

/// The items of an array as Postgres writes it, `{a,"b c",NULL}`; nested
/// arrays stay whole.
fn array_items(text: &str) -> Vec<Option<String>> {
    let inner = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .unwrap_or(text);
    let mut items = Vec::new();
    let mut chars = inner.chars().peekable();
    while chars.peek().is_some() {
        let mut item = String::new();
        let mut quoted = false;
        let mut depth = 0;
        while let Some(c) = chars.next() {
            match c {
                '"' if depth == 0 => {
                    quoted = true;
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => item.extend(chars.next()),
                            '"' => break,
                            c => item.push(c),
                        }
                    }
                }
                '{' => {
                    depth += 1;
                    item.push(c);
                }
                '}' => {
                    depth -= 1;
                    item.push(c);
                }
                ',' if depth == 0 => break,
                c => item.push(c),
            }
        }
        items.push((quoted || item != "NULL").then_some(item));
    }
    items
}

/// A name as Postgres would write it: quoted only when it must be.
fn ident(name: &str) -> String {
    dfox_sql::quote_ident(name, dfox_sql::Dialect::Postgres)
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
                        NOT a.attnotnull, \
                        CASE WHEN a.attgenerated = '' THEN pg_get_expr(d.adbin, d.adrelid) END, \
                        COALESCE(a.attnum = ANY(i.indkey), false), \
                        col_description(c.oid, a.attnum), \
                        CASE WHEN a.attcollation <> t.typcollation THEN co.collname::text END, \
                        CASE WHEN a.attgenerated <> '' THEN pg_get_expr(d.adbin, d.adrelid) END \
                 FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_type t ON t.oid = a.atttypid \
                 LEFT JOIN pg_collation co ON co.oid = a.attcollation \
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
                comment: row.get(5),
                collation: row.get(6),
                generated: row.get(7),
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
                              ORDER BY k.ord), \
                        pg_get_indexdef(i.indexrelid) \
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
                definition: format!("{};\n", row.get::<_, String>(3)),
            })
            .collect();
        let table_row = self
            .client
            .query_opt(
                "SELECT CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END, \
                        obj_description(c.oid, 'pg_class') \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1::text AND c.relname = $2::text",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?;
        let estimate = table_row
            .as_ref()
            .and_then(|row| row.get::<_, Option<i64>>(0))
            .map(|n| n as u64);
        let comment = table_row.and_then(|row| row.get(1));
        Ok(EntityInfo {
            columns,
            indexes,
            row_estimate: estimate,
            foreign_keys: self.foreign_keys(schema, table).await?,
            constraints: self.constraints(schema, table).await?,
            triggers: self.triggers(schema, table).await?,
            policies: self.policies(schema, table).await?,
            comment,
        })
    }

    async fn constraints(&self, schema: &str, table: &str) -> Result<Vec<ConstraintInfo>, DbError> {
        let rows = self
            .client
            .query(
                "SELECT con.conname::text, con.contype::text, pg_get_constraintdef(con.oid, true) \
                 FROM pg_constraint con \
                 JOIN pg_class c ON c.oid = con.conrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1::text AND c.relname = $2::text \
                   AND con.contype IN ('p', 'f', 'u', 'c', 'x') \
                 ORDER BY position(con.contype IN 'pfucx'), 1",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?;
        let target = format!("{}.{}", ident(schema), ident(table));
        Ok(rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let kind = match row.get::<_, String>(1).as_str() {
                    "p" => ConstraintKind::PrimaryKey,
                    "f" => ConstraintKind::ForeignKey,
                    "u" => ConstraintKind::Unique,
                    "x" => ConstraintKind::Exclusion,
                    _ => ConstraintKind::Check,
                };
                let definition = format!(
                    "ALTER TABLE {target}\n    ADD CONSTRAINT {} {};\n",
                    ident(&name),
                    row.get::<_, String>(2)
                );
                ConstraintInfo {
                    name,
                    kind,
                    definition,
                }
            })
            .collect())
    }

    async fn triggers(&self, schema: &str, table: &str) -> Result<Vec<RuleInfo>, DbError> {
        let rows = self
            .client
            .query(
                "SELECT t.tgname::text, pg_get_triggerdef(t.oid, true), \
                        CASE WHEN t.tgtype & 2 <> 0 THEN 'before' \
                             WHEN t.tgtype & 64 <> 0 THEN 'instead of' ELSE 'after' END \
                        || ' ' || concat_ws(' or ', \
                             CASE WHEN t.tgtype & 4 <> 0 THEN 'insert' END, \
                             CASE WHEN t.tgtype & 16 <> 0 THEN 'update' END, \
                             CASE WHEN t.tgtype & 8 <> 0 THEN 'delete' END, \
                             CASE WHEN t.tgtype & 32 <> 0 THEN 'truncate' END) \
                 FROM pg_trigger t \
                 JOIN pg_class c ON c.oid = t.tgrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1::text AND c.relname = $2::text AND NOT t.tgisinternal \
                 ORDER BY 1",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| RuleInfo {
                name: row.get(0),
                definition: format!("{};\n", row.get::<_, String>(1)),
                detail: row.get(2),
            })
            .collect())
    }

    async fn policies(&self, schema: &str, table: &str) -> Result<Vec<RuleInfo>, DbError> {
        let rows = self
            .client
            .query(
                "SELECT policyname::text, permissive, cmd, roles::text[], qual, with_check \
                 FROM pg_policies WHERE schemaname = $1::text AND tablename = $2::text \
                 ORDER BY 1",
                &[&schema, &table],
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let command: String = row.get(2);
                let roles: Vec<String> = row.get(3);
                let mut definition = format!(
                    "CREATE POLICY {} ON {}.{}\n    AS {} FOR {command} TO {}",
                    ident(&name),
                    ident(schema),
                    ident(table),
                    row.get::<_, String>(1),
                    roles.join(", "),
                );
                if let Some(using) = row.get::<_, Option<String>>(4) {
                    definition.push_str(&format!("\n    USING ({using})"));
                }
                if let Some(check) = row.get::<_, Option<String>>(5) {
                    definition.push_str(&format!("\n    WITH CHECK ({check})"));
                }
                definition.push_str(";\n");
                RuleInfo {
                    name,
                    detail: Some(format!("for {}", command.to_lowercase())),
                    definition,
                }
            })
            .collect())
    }

    async fn foreign_keys(&self, schema: &str, table: &str) -> Result<Vec<ForeignKey>, DbError> {
        let names = |key: &str, table: &str| {
            format!(
                "ARRAY(SELECT a.attname::text \
                       FROM unnest(con.{key}) WITH ORDINALITY AS k(num, ord) \
                       JOIN pg_attribute a ON a.attrelid = con.{table} AND a.attnum = k.num \
                       ORDER BY k.ord)"
            )
        };
        let sql = format!(
            "SELECT con.conname::text, rn.nspname::text, rc.relname::text, {}, {} \
             FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_class rc ON rc.oid = con.confrelid \
             JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
             WHERE con.contype = 'f' AND n.nspname = $1::text AND c.relname = $2::text \
             ORDER BY 1",
            names("conkey", "conrelid"),
            names("confkey", "confrelid"),
        );
        let rows = self
            .client
            .query(&sql, &[&schema, &table])
            .await
            .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| ForeignKey {
                name: row.get(0),
                target: [self.database.clone(), row.get(1), row.get(2)]
                    .into_iter()
                    .collect(),
                columns: row.get(3),
                target_columns: row.get(4),
            })
            .collect())
    }

    /// The planner's statistics for a column, from `pg_stats`.
    async fn planner_stats(
        &self,
        schema: &str,
        table: &str,
        column: &str,
    ) -> Result<Option<ColumnStats>, DbError> {
        let Some(row) = self
            .client
            .query_opt(
                "SELECT null_frac::float8, n_distinct::float8, most_common_vals::text, \
                        most_common_freqs::float8[], histogram_bounds::text \
                 FROM pg_stats \
                 WHERE schemaname = $1::text AND tablename = $2::text AND attname = $3::text \
                 ORDER BY inherited LIMIT 1",
                &[&schema, &table, &column],
            )
            .await
            .map_err(db_error)?
        else {
            return Ok(None);
        };
        let null_fraction: Option<f64> = row.get(0);
        let distinct = match row.get::<_, Option<f64>>(1) {
            Some(n) if n > 0.0 => Some(Distinct::Count(n)),
            Some(n) if n < 0.0 => Some(Distinct::Fraction(-n)),
            _ => None,
        };
        let values = row
            .get::<_, Option<String>>(2)
            .map(|text| array_items(&text))
            .unwrap_or_default();
        let frequencies: Vec<f64> = row.get::<_, Option<Vec<f64>>>(3).unwrap_or_default();
        let common: Vec<(String, f64)> = values
            .into_iter()
            .zip(frequencies)
            .map(|(value, f)| (value.unwrap_or_else(|| "NULL".into()), f))
            .collect();
        let bounds: Vec<String> = row
            .get::<_, Option<String>>(4)
            .map(|text| array_items(&text).into_iter().flatten().collect())
            .unwrap_or_default();
        // The bounds split the values that aren't common into ranges of
        // equal size.
        let rest = 1.0 - null_fraction.unwrap_or(0.0) - common.iter().map(|(_, f)| f).sum::<f64>();
        let ranges = bounds.len().saturating_sub(1);
        let histogram = bounds
            .windows(2)
            .map(|pair| Bucket {
                low: pair[0].clone(),
                high: pair[1].clone(),
                fraction: rest.max(0.0) / ranges as f64,
            })
            .collect();
        Ok(Some(ColumnStats {
            source: StatsSource::Planner,
            null_fraction,
            distinct,
            common,
            histogram,
            ..ColumnStats::default()
        }))
    }

    /// Counts a column's values over the first rows of its table.
    async fn profile(
        &self,
        schema: &str,
        table: &str,
        column: &str,
    ) -> Result<ColumnStats, DbError> {
        if !self
            .columns(schema, table)
            .await?
            .iter()
            .any(|c| c.name == column)
        {
            return Err(DbError::General(format!("no column {column} in {table}")));
        }
        let sample = format!(
            "(SELECT {} AS c FROM {}.{} LIMIT {PROFILE_ROWS}) s",
            quote(column),
            quote(schema),
            quote(table)
        );
        let counts = |range: &str| {
            format!("SELECT count(*), count(c), count(DISTINCT c::text), {range} FROM {sample}")
        };
        // Some types can't be ordered, so they have no smallest or largest.
        let row = match self
            .client
            .query_one(&counts("min(c)::text, max(c)::text"), &[])
            .await
        {
            Ok(row) => row,
            Err(_) => self
                .client
                .query_one(&counts("NULL::text, NULL::text"), &[])
                .await
                .map_err(db_error)?,
        };
        let (rows, values, distinct): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
        let common = self
            .client
            .query(
                &format!(
                    "SELECT c::text, count(*) FROM {sample} WHERE c IS NOT NULL \
                     GROUP BY 1 HAVING count(*) > 1 ORDER BY 2 DESC, 1 LIMIT 10"
                ),
                &[],
            )
            .await
            .map_err(db_error)?
            .iter()
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    row.get::<_, i64>(1) as f64 / rows as f64,
                )
            })
            .collect();
        Ok(ColumnStats {
            source: StatsSource::Profile,
            rows: Some(rows as u64),
            null_fraction: (rows > 0).then(|| (rows - values) as f64 / rows as f64),
            distinct: Some(Distinct::Count(distinct as f64)),
            common,
            histogram: Vec::new(),
            min: row.get(3),
            max: row.get(4),
        })
    }

    /// A `CREATE TABLE` put together from the catalog, as Postgres keeps
    /// none: columns, constraints, then indexes, triggers and comments.
    async fn table_definition(&self, schema: &str, table: &str) -> Result<String, DbError> {
        let info = self.describe_table(schema, table).await?;
        let target = format!("{}.{}", ident(schema), ident(table));
        let mut lines: Vec<String> = info
            .columns
            .iter()
            .map(|c| {
                let mut line = format!("    {} {}", ident(&c.name), c.type_name);
                if let Some(collation) = &c.collation {
                    line.push_str(&format!(" COLLATE {}", quote(collation)));
                }
                if let Some(expression) = &c.generated {
                    line.push_str(&format!(" GENERATED ALWAYS AS ({expression}) STORED"));
                }
                if !c.nullable {
                    line.push_str(" NOT NULL");
                }
                if let Some(default) = &c.default {
                    line.push_str(&format!(" DEFAULT {default}"));
                }
                line
            })
            .collect();
        // Each constraint is kept as `ALTER TABLE … ADD CONSTRAINT name …;`.
        lines.extend(info.constraints.iter().filter_map(|c| {
            let (_, body) = c.definition.split_once("ADD CONSTRAINT ")?;
            Some(format!(
                "    CONSTRAINT {}",
                body.trim_end().trim_end_matches(';')
            ))
        }));
        let mut out = format!("CREATE TABLE {target} (\n{}\n);\n", lines.join(",\n"));
        let literal = |text: &str| format!("'{}'", text.replace('\'', "''"));
        let backing: Vec<&str> = info.constraints.iter().map(|c| c.name.as_str()).collect();
        let extra = info
            .indexes
            .iter()
            .filter(|i| !backing.contains(&i.name.as_str()))
            .map(|i| i.definition.clone())
            .chain(info.triggers.iter().map(|t| t.definition.clone()));
        let comments = info
            .comment
            .iter()
            .map(|c| format!("COMMENT ON TABLE {target} IS {};\n", literal(c)))
            .chain(info.columns.iter().filter_map(|col| {
                let comment = col.comment.as_ref()?;
                Some(format!(
                    "COMMENT ON COLUMN {target}.{} IS {};\n",
                    ident(&col.name),
                    literal(comment)
                ))
            }));
        for statement in extra.chain(comments) {
            out.push('\n');
            out.push_str(&statement);
        }
        Ok(out)
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
                Ok(format!(
                    "{create} {}.{} AS\n{}\n",
                    ident(schema),
                    ident(name),
                    body.trim_end()
                ))
            }
            NodeKind::Table => self.table_definition(schema, name).await,
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
            let all = &changes.changes;
            let mut next = 0;
            while next < all.len() {
                // A run of inserts naming the same columns goes in by COPY.
                let run = Change::insert_run(&all[next..]);
                if run > 1 {
                    report.inserted += copy_in(client, &target, &all[next..next + run]).await?;
                    next += run;
                    continue;
                }
                let change = &all[next];
                next += 1;
                let (sql, values): (String, Vec<Option<String>>) = match change {
                    Change::Insert(values) if values.is_empty() => {
                        (format!("INSERT INTO {target} DEFAULT VALUES"), Vec::new())
                    }
                    Change::Insert(values) => {
                        let columns: Vec<String> = values.iter().map(|(c, _)| quote(c)).collect();
                        let casts = values
                            .iter()
                            .enumerate()
                            .map(|(i, (c, _))| cast(c, i + 1))
                            .collect::<Result<Vec<_>, _>>()?;
                        (
                            format!(
                                "INSERT INTO {target} ({}) VALUES ({})",
                                columns.join(", "),
                                casts.join(", ")
                            ),
                            values.iter().map(|(_, v)| param(v)).collect(),
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

    fn count_rows<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let (schema, table) = self.table(node)?;
            let sql = format!("SELECT count(*) FROM {}.{}", quote(schema), quote(table));
            let row = self.client.query_one(&sql, &[]).await.map_err(db_error)?;
            Ok(row.get::<_, i64>(0) as u64)
        })
    }

    fn column_stats<'a>(
        &'a mut self,
        node: &'a NodePath,
        column: &'a str,
        profile: bool,
    ) -> BoxFuture<'a, Result<Option<ColumnStats>, DbError>> {
        Box::pin(async move {
            let (schema, table) = self.table(node)?;
            if profile {
                self.profile(schema, table, column).await.map(Some)
            } else {
                self.planner_stats(schema, table, column).await
            }
        })
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
    fn array_text_splits_into_items() {
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            array_items(r#"{a,"b, c",NULL,"NULL","say \"hi\"",{1,2}}"#),
            [
                some("a"),
                some("b, c"),
                None,
                some("NULL"),
                some("say \"hi\""),
                some("{1,2}")
            ]
        );
        assert_eq!(array_items("{}"), []);
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
