//! The MySQL catalog as a tree: databases ▸ tables, views and routines ▸
//! columns. In MySQL a database is a schema, and one connection reaches all
//! of them, so every database can be browsed. One query per level, read from
//! `information_schema` on the session's metadata connection. Browsing and
//! edits run there too, with values always sent as parameters.

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

/// A histogram's values, as MySQL writes them in JSON: strings as
/// `base64:type254:…`.
fn histogram_value(value: &serde_json::Value) -> String {
    use base64::Engine;
    match value {
        serde_json::Value::String(text) => text
            .strip_prefix("base64:")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(_, data)| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .map_or_else(
                || text.clone(),
                |bytes| String::from_utf8_lossy(&bytes).into_owned(),
            ),
        other => other.to_string(),
    }
}

/// Column statistics from a MySQL 8 histogram: a singleton histogram holds
/// every value and how often it occurs, an equi-height one ranges.
fn histogram_stats(json: &str) -> Option<ColumnStats> {
    let histogram: serde_json::Value = serde_json::from_str(json).ok()?;
    let buckets = histogram.get("buckets")?.as_array()?;
    let singleton = histogram.get("histogram-type")?.as_str()? == "singleton";
    let mut stats = ColumnStats {
        null_fraction: histogram.get("null-values").and_then(|v| v.as_f64()),
        ..ColumnStats::default()
    };
    // Each bucket holds the fraction of rows up to and including it.
    let mut before = 0.0;
    let mut distinct = 0.0;
    for bucket in buckets {
        let bucket = bucket.as_array()?;
        if singleton {
            let [value, upto] = bucket.as_slice() else {
                return None;
            };
            let upto = upto.as_f64()?;
            stats.common.push((histogram_value(value), upto - before));
            before = upto;
            distinct += 1.0;
        } else {
            let [low, high, upto, values] = bucket.as_slice() else {
                return None;
            };
            let upto = upto.as_f64()?;
            stats.histogram.push(Bucket {
                low: histogram_value(low),
                high: histogram_value(high),
                fraction: upto - before,
            });
            before = upto;
            distinct += values.as_f64()?;
        }
    }
    stats
        .common
        .sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    stats.common.truncate(10);
    stats.distinct = Some(Distinct::Count(distinct));
    Some(stats)
}

/// `` `a`, `b` ``: names quoted and listed.
fn quote_all(names: &[String]) -> String {
    names
        .iter()
        .map(|n| quote(n))
        .collect::<Vec<_>>()
        .join(", ")
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
        let signatures = Self::signatures(&mut conn, database).await?;
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
                let detail = match kind {
                    NodeKind::Table => estimates.get(&name).map(|&rows| estimate_text(rows)),
                    NodeKind::Function | NodeKind::Procedure => Some(
                        signatures
                            .get(&(name.clone(), kind))
                            .cloned()
                            .unwrap_or_else(|| "()".to_string()),
                    ),
                    _ => None,
                };
                let node = Node::new(kind, name, kind.has_rows());
                match detail {
                    Some(detail) => node.with_detail(detail),
                    None => node,
                }
            })
            .collect())
    }

    /// `(a INT, OUT b TEXT) → int` for each routine with parameters or a
    /// result, by name and kind.
    async fn signatures(
        conn: &mut Conn,
        database: &str,
    ) -> Result<HashMap<(String, NodeKind), String>, DbError> {
        let rows: Vec<(String, String, Option<String>, Option<String>)> = conn
            .exec(
                "SELECT SPECIFIC_NAME, ROUTINE_TYPE, \
                        GROUP_CONCAT(CASE WHEN ORDINAL_POSITION > 0 THEN CONCAT_WS(' ', \
                            CASE WHEN PARAMETER_MODE <> 'IN' THEN PARAMETER_MODE END, \
                            PARAMETER_NAME, DTD_IDENTIFIER) END \
                            ORDER BY ORDINAL_POSITION SEPARATOR ', '), \
                        MAX(CASE WHEN ORDINAL_POSITION = 0 THEN DTD_IDENTIFIER END) \
                 FROM information_schema.PARAMETERS WHERE SPECIFIC_SCHEMA = ? \
                 GROUP BY SPECIFIC_NAME, ROUTINE_TYPE",
                (database,),
            )
            .await
            .map_err(db_error)?;
        Ok(rows
            .into_iter()
            .map(|(name, kind, arguments, result)| {
                let kind = if kind == "PROCEDURE" {
                    NodeKind::Procedure
                } else {
                    NodeKind::Function
                };
                let arguments = arguments.unwrap_or_default();
                let signature = match result {
                    Some(result) => format!("({arguments}) → {result}"),
                    None => format!("({arguments})"),
                };
                ((name, kind), signature)
            })
            .collect())
    }

    /// The SQL that creates a view or routine, as the server keeps it.
    async fn definition(&self, node: &NodePath, kind: NodeKind) -> Result<String, DbError> {
        let (database, name) = table(node)?;
        let (what, column) = match kind {
            NodeKind::Table => ("TABLE", "Create Table"),
            NodeKind::View => ("VIEW", "Create View"),
            NodeKind::Function => ("FUNCTION", "Create Function"),
            NodeKind::Procedure => ("PROCEDURE", "Create Procedure"),
            kind => {
                return Err(DbError::General(format!(
                    "MySQL can't show the definition of a {}",
                    kind.label()
                )))
            }
        };
        let sql = format!("SHOW CREATE {what} {}.{}", quote(database), quote(name));
        let row: Option<mysql_async::Row> = self
            .conn
            .lock()
            .await
            .query_first(sql)
            .await
            .map_err(db_error)?;
        let missing = || DbError::General(format!("no {} {database}.{name}", kind.label()));
        let text: Option<String> = row
            .ok_or_else(missing)?
            .get_opt(column)
            .and_then(Result::ok);
        // Without the right privileges the server hides the body.
        let text = text.ok_or_else(|| {
            DbError::General(format!(
                "the server didn't show the definition of {database}.{name}: \
                 it needs SHOW_ROUTINE or the routine's own privileges"
            ))
        })?;
        Ok(format!("{};\n", text.trim_end()))
    }

    async fn columns(&self, database: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
        type Row = (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
        );
        let rows: Vec<Row> = self
            .conn
            .lock()
            .await
            .exec(
                "SELECT c.COLUMN_NAME, c.COLUMN_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT, \
                        c.COLUMN_KEY, c.COLUMN_COMMENT, \
                        CASE WHEN c.COLLATION_NAME <> t.TABLE_COLLATION \
                             THEN c.COLLATION_NAME END, \
                        c.GENERATION_EXPRESSION \
                 FROM information_schema.COLUMNS c \
                 LEFT JOIN information_schema.TABLES t \
                   ON t.TABLE_SCHEMA = c.TABLE_SCHEMA AND t.TABLE_NAME = c.TABLE_NAME \
                 WHERE c.TABLE_SCHEMA = ? AND c.TABLE_NAME = ? ORDER BY c.ORDINAL_POSITION",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let given = |text: Option<String>| text.filter(|t| !t.is_empty());
        Ok(rows
            .into_iter()
            .map(
                |(name, type_name, nullable, default, key, comment, collation, generated)| {
                    ColumnInfo {
                        name,
                        type_name,
                        nullable: nullable == "YES",
                        default,
                        primary_key: key == "PRI",
                        comment: given(Some(comment)),
                        collation,
                        generated: given(generated),
                    }
                },
            )
            .collect())
    }

    async fn describe_table(&self, database: &str, table: &str) -> Result<EntityInfo, DbError> {
        let columns = self.columns(database, table).await?;
        if columns.is_empty() {
            return Err(DbError::General(format!("no table {database}.{table}")));
        }
        let mut conn = self.conn.lock().await;
        let parts: Vec<(String, i64, Option<String>, String)> = conn
            .exec(
                "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, INDEX_TYPE \
                 FROM information_schema.STATISTICS \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                 ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let target = quote(table);
        let mut indexes: Vec<IndexInfo> = Vec::new();
        let mut kinds: Vec<String> = Vec::new();
        for (name, non_unique, column, kind) in parts {
            // An index on an expression has no column name.
            let column = column.unwrap_or_else(|| "(expression)".into());
            match indexes.last_mut() {
                Some(index) if index.name == name => index.columns.push(column),
                _ => {
                    indexes.push(IndexInfo {
                        name,
                        columns: vec![column],
                        unique: non_unique == 0,
                        definition: String::new(),
                    });
                    kinds.push(kind);
                }
            }
        }
        for (index, kind) in indexes.iter_mut().zip(&kinds) {
            let columns = quote_all(&index.columns);
            index.definition = match (index.name.as_str(), kind.as_str()) {
                ("PRIMARY", _) => format!("ALTER TABLE {target} ADD PRIMARY KEY ({columns});\n"),
                (name, "FULLTEXT" | "SPATIAL") => format!(
                    "CREATE {kind} INDEX {} ON {target} ({columns});\n",
                    quote(name)
                ),
                (name, _) => format!(
                    "CREATE {}INDEX {} ON {target} ({columns});\n",
                    if index.unique { "UNIQUE " } else { "" },
                    quote(name)
                ),
            };
        }
        let table_row: Option<(Option<u64>, Option<String>, String)> = conn
            .exec_first(
                "SELECT TABLE_ROWS, TABLE_COMMENT, TABLE_TYPE FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let (row_estimate, comment) = match table_row {
            // A view's comment is always "VIEW".
            Some((rows, comment, kind)) => (
                rows,
                comment.filter(|c| !c.is_empty() && kind == "BASE TABLE"),
            ),
            None => (None, None),
        };
        let keys: Vec<(String, String, String, String, String)> = conn
            .exec(
                "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
                        REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                 FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                   AND REFERENCED_TABLE_NAME IS NOT NULL \
                 ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let mut foreign_keys: Vec<ForeignKey> = Vec::new();
        for (name, column, target_database, target_table, target_column) in keys {
            match foreign_keys.last_mut() {
                Some(key) if key.name == name => {
                    key.columns.push(column);
                    key.target_columns.push(target_column);
                }
                _ => foreign_keys.push(ForeignKey {
                    name,
                    columns: vec![column],
                    target: [target_database, target_table].into_iter().collect(),
                    target_columns: vec![target_column],
                }),
            }
        }
        let constraints =
            Self::constraints(&mut conn, database, table, &indexes, &foreign_keys).await?;
        let triggers: Vec<(String, String, String, String)> = conn
            .exec(
                "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION, ACTION_STATEMENT \
                 FROM information_schema.TRIGGERS \
                 WHERE EVENT_OBJECT_SCHEMA = ? AND EVENT_OBJECT_TABLE = ? \
                 ORDER BY TRIGGER_NAME",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let triggers = triggers
            .into_iter()
            .map(|(name, timing, event, statement)| RuleInfo {
                definition: format!(
                    "CREATE TRIGGER {} {timing} {event} ON {target}\n    FOR EACH ROW {};\n",
                    quote(&name),
                    statement.trim_end().trim_end_matches(';')
                ),
                detail: Some(format!(
                    "{} {}",
                    timing.to_lowercase(),
                    event.to_lowercase()
                )),
                name,
            })
            .collect();
        Ok(EntityInfo {
            columns,
            indexes,
            row_estimate,
            foreign_keys,
            constraints,
            triggers,
            policies: Vec::new(),
            comment,
        })
    }

    /// The table's keys and checks, with the SQL that adds each.
    async fn constraints(
        conn: &mut Conn,
        database: &str,
        table: &str,
        indexes: &[IndexInfo],
        foreign_keys: &[ForeignKey],
    ) -> Result<Vec<ConstraintInfo>, DbError> {
        let target = quote(table);
        let add = |name: &str, body: String| {
            format!(
                "ALTER TABLE {target}\n    ADD CONSTRAINT {} {body};\n",
                quote(name)
            )
        };
        let declared: Vec<(String, String)> = conn
            .exec(
                "SELECT CONSTRAINT_NAME, CONSTRAINT_TYPE FROM information_schema.TABLE_CONSTRAINTS \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                   AND CONSTRAINT_TYPE IN ('UNIQUE', 'CHECK')",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        let mut out = Vec::new();
        if let Some(primary) = indexes.iter().find(|i| i.name == "PRIMARY") {
            out.push(ConstraintInfo {
                name: "PRIMARY".into(),
                kind: ConstraintKind::PrimaryKey,
                definition: primary.definition.clone(),
            });
        }
        let rules: Vec<(String, String, String)> = conn
            .exec(
                "SELECT CONSTRAINT_NAME, UPDATE_RULE, DELETE_RULE \
                 FROM information_schema.REFERENTIAL_CONSTRAINTS \
                 WHERE CONSTRAINT_SCHEMA = ? AND TABLE_NAME = ?",
                (database, table),
            )
            .await
            .map_err(db_error)?;
        for key in foreign_keys {
            let mut body = format!(
                "FOREIGN KEY ({}) REFERENCES {} ({})",
                quote_all(&key.columns),
                key.target
                    .parts()
                    .iter()
                    .map(|p| quote(p))
                    .collect::<Vec<_>>()
                    .join("."),
                quote_all(&key.target_columns),
            );
            if let Some((_, update, delete)) = rules.iter().find(|(name, ..)| *name == key.name) {
                for (event, rule) in [("UPDATE", update), ("DELETE", delete)] {
                    if rule != "RESTRICT" && rule != "NO ACTION" {
                        body.push_str(&format!(" ON {event} {rule}"));
                    }
                }
            }
            out.push(ConstraintInfo {
                definition: add(&key.name, body),
                name: key.name.clone(),
                kind: ConstraintKind::ForeignKey,
            });
        }
        for (name, _) in declared.iter().filter(|(_, kind)| kind == "UNIQUE") {
            if let Some(index) = indexes.iter().find(|i| i.name == *name) {
                out.push(ConstraintInfo {
                    definition: add(name, format!("UNIQUE ({})", quote_all(&index.columns))),
                    name: name.clone(),
                    kind: ConstraintKind::Unique,
                });
            }
        }
        if declared.iter().any(|(_, kind)| kind == "CHECK") {
            let checks: Vec<(String, String)> = conn
                .exec(
                    "SELECT tc.CONSTRAINT_NAME, cc.CHECK_CLAUSE \
                     FROM information_schema.TABLE_CONSTRAINTS tc \
                     JOIN information_schema.CHECK_CONSTRAINTS cc \
                       ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
                      AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
                     WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? \
                       AND tc.CONSTRAINT_TYPE = 'CHECK' \
                     ORDER BY 1",
                    (database, table),
                )
                .await
                .map_err(db_error)?;
            for (name, clause) in checks {
                out.push(ConstraintInfo {
                    definition: add(&name, format!("CHECK {clause}")),
                    name,
                    kind: ConstraintKind::Check,
                });
            }
        }
        Ok(out)
    }

    /// A MySQL 8 histogram of a column, from `ANALYZE TABLE … UPDATE
    /// HISTOGRAM`. Servers without them have none.
    async fn histogram(
        &self,
        database: &str,
        table: &str,
        column: &str,
    ) -> Result<Option<ColumnStats>, DbError> {
        let found: Result<Option<String>, _> = self
            .conn
            .lock()
            .await
            .exec_first(
                "SELECT CAST(HISTOGRAM AS CHAR) FROM information_schema.COLUMN_STATISTICS \
                 WHERE SCHEMA_NAME = ? AND TABLE_NAME = ? AND COLUMN_NAME = ?",
                (database, table, column),
            )
            .await;
        match found {
            Ok(json) => Ok(json.and_then(|json| histogram_stats(&json))),
            // MariaDB and MySQL 5.7 have no COLUMN_STATISTICS.
            Err(err) => {
                log::debug!("no histograms: {err}");
                Ok(None)
            }
        }
    }

    /// Counts a column's values over the first rows of its table.
    async fn profile(
        &self,
        database: &str,
        table: &str,
        column: &str,
    ) -> Result<ColumnStats, DbError> {
        self.check_columns(database, table, [&column.to_string()])
            .await?;
        let sample = format!(
            "(SELECT {} AS c FROM {}.{} LIMIT {PROFILE_ROWS}) s",
            quote(column),
            quote(database),
            quote(table)
        );
        let mut conn = self.conn.lock().await;
        let (rows, values, distinct, min, max): (u64, u64, u64, Option<String>, Option<String>) =
            conn.query_first(format!(
                "SELECT COUNT(*), COUNT(c), COUNT(DISTINCT c), \
                        CAST(MIN(c) AS CHAR), CAST(MAX(c) AS CHAR) FROM {sample}"
            ))
            .await
            .map_err(db_error)?
            .unwrap_or_default();
        let common: Vec<(String, u64)> = conn
            .query(format!(
                "SELECT CAST(c AS CHAR) AS v, COUNT(*) AS n FROM {sample} \
                 WHERE c IS NOT NULL GROUP BY c HAVING n > 1 ORDER BY n DESC, v LIMIT 10"
            ))
            .await
            .map_err(db_error)?;
        Ok(ColumnStats {
            source: StatsSource::Profile,
            rows: Some(rows),
            null_fraction: (rows > 0).then(|| (rows - values) as f64 / rows as f64),
            distinct: Some(Distinct::Count(distinct as f64)),
            common: common
                .into_iter()
                .map(|(value, n)| (value, n as f64 / rows as f64))
                .collect(),
            histogram: Vec::new(),
            min,
            max,
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
    fn count_rows<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let (database, table) = table(node)?;
            let sql = format!("SELECT COUNT(*) FROM {}.{}", quote(database), quote(table));
            let count: Option<u64> = self
                .conn
                .lock()
                .await
                .query_first(sql)
                .await
                .map_err(db_error)?;
            Ok(count.unwrap_or(0))
        })
    }

    fn column_stats<'a>(
        &'a mut self,
        node: &'a NodePath,
        column: &'a str,
        profile: bool,
    ) -> BoxFuture<'a, Result<Option<ColumnStats>, DbError>> {
        Box::pin(async move {
            let (database, table) = table(node)?;
            if profile {
                self.profile(database, table, column).await.map(Some)
            } else {
                self.histogram(database, table, column).await
            }
        })
    }

    fn definition<'a>(
        &'a mut self,
        node: &'a NodePath,
        kind: NodeKind,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        Box::pin(async move { MyCatalog::definition(self, node, kind).await })
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
            let rows: Vec<(String, String, String)> = self
                .conn
                .lock()
                .await
                .exec(
                    "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE FROM information_schema.COLUMNS \
                     WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
                    (database,),
                )
                .await
                .map_err(db_error)?;
            Ok(rows
                .into_iter()
                .map(|(table, name, data_type)| TableColumn {
                    table,
                    name,
                    data_type,
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histograms_become_stats() {
        let equi_height = r#"{"buckets": [[1, 50, 0.25, 50], [51, 200, 0.995, 150]],
            "null-values": 0.005, "histogram-type": "equi-height"}"#;
        let stats = histogram_stats(equi_height).expect("stats");
        assert_eq!(stats.null_fraction, Some(0.005));
        assert_eq!(stats.distinct, Some(Distinct::Count(200.0)));
        assert_eq!(
            stats.histogram,
            [
                Bucket {
                    low: "1".into(),
                    high: "50".into(),
                    fraction: 0.25
                },
                Bucket {
                    low: "51".into(),
                    high: "200".into(),
                    fraction: 0.745
                }
            ]
        );

        let singleton = r#"{"buckets": [["base64:type254:ZXU=", 0.25], ["base64:type254:dXM=", 1.0]],
            "null-values": 0.0, "histogram-type": "singleton"}"#;
        let stats = histogram_stats(singleton).expect("stats");
        assert_eq!(
            stats.common,
            [("us".to_string(), 0.75), ("eu".to_string(), 0.25)]
        );
        assert_eq!(histogram_stats("not json"), None);
    }

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
