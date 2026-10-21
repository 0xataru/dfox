//! The keys of a Redis server as a tree: databases, then keys grouped by
//! `:` into prefixes. Keys are found with `SCAN`, never `KEYS`, which would
//! stop the server while it walks every key.
//!
//! A path is the database number, then the parts of a key: prefixes end
//! with `:` (`user:`), so `["0", "user:", "orders:", "7"]` is the key
//! `user:orders:7` in database 0, and a key named `user` can sit beside the
//! prefix `user:`.

use std::collections::BTreeMap;

use dfox_core::{
    arrow::ResultStream,
    catalog::{BrowseQuery, Catalog, EntityInfo, Node, NodeKind, NodePath},
    errors::DbError,
};
use futures_util::future::BoxFuture;
use redis::{aio::MultiplexedConnection, Value};

use super::{convert, query, send_table, spawn_run};

/// Keys read under one node, at most: a bigger database shows the first
/// ones and says so.
const SCAN_LIMIT: usize = 10_000;
/// Keys whose type is asked for under one node, at most.
const TYPE_LIMIT: usize = 1_000;

pub struct RedisCatalog {
    conn: MultiplexedConnection,
    /// The session's database, always listed.
    database: i64,
    /// The database this connection is in.
    selected: i64,
}

impl RedisCatalog {
    pub fn new(conn: MultiplexedConnection, database: i64) -> Self {
        Self {
            conn,
            database,
            selected: database,
        }
    }

    async fn run(&mut self, words: &[&str]) -> Result<Value, DbError> {
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        query(&mut self.conn, &words).await
    }

    /// Moves the connection to the database at the start of `node`, and
    /// returns the key the rest of it names.
    async fn enter(&mut self, node: &NodePath) -> Result<String, DbError> {
        let (database, key) = node
            .parts()
            .split_first()
            .ok_or_else(|| DbError::General("no database".into()))?;
        let n: i64 = database
            .parse()
            .map_err(|_| DbError::General(format!("no database {database}")))?;
        if n != self.selected {
            self.run(&["SELECT", database]).await?;
            self.selected = n;
        }
        Ok(key.concat())
    }

    /// The databases holding keys, and the session's own, with how many.
    async fn databases(&mut self) -> Result<Vec<Node>, DbError> {
        let info = convert::text(&self.run(&["INFO", "keyspace"]).await?);
        let mut keys = keyspace(&info);
        keys.entry(self.database).or_insert(0);
        Ok(keys
            .into_iter()
            .map(|(n, count)| {
                Node::new(NodeKind::Database, n.to_string(), true)
                    .with_detail(format!("{count} keys"))
            })
            .collect())
    }

    /// The prefixes and keys right under the key prefix `prefix`.
    async fn keys(&mut self, prefix: &str) -> Result<Vec<Node>, DbError> {
        let pattern = format!("{}*", glob_escape(prefix));
        let mut cursor = "0".to_string();
        let mut keys = Vec::new();
        loop {
            let reply = self
                .run(&["SCAN", &cursor, "MATCH", &pattern, "COUNT", "1000"])
                .await?;
            let Value::Array(parts) = reply else {
                return Err(DbError::General("SCAN answered oddly".into()));
            };
            let [next, Value::Array(found)] = parts.as_slice() else {
                return Err(DbError::General("SCAN answered oddly".into()));
            };
            keys.extend(found.iter().map(convert::text));
            cursor = convert::text(next);
            if cursor == "0" || keys.len() >= SCAN_LIMIT {
                break;
            }
        }
        let more = cursor != "0";
        let (prefixes, leaves) = group(prefix, &keys);
        let mut types = Vec::new();
        if !leaves.is_empty() {
            let mut pipe = redis::pipe();
            for leaf in leaves.iter().take(TYPE_LIMIT) {
                pipe.cmd("TYPE").arg(format!("{prefix}{leaf}"));
            }
            types = pipe
                .query_async::<Vec<String>>(&mut self.conn)
                .await
                .map_err(super::db_error)?;
        }
        let plus = if more { "+" } else { "" };
        let mut nodes: Vec<Node> = prefixes
            .into_iter()
            .map(|(name, count)| {
                Node::new(NodeKind::KeyPrefix, name, true)
                    .with_detail(format!("{count}{plus} keys"))
            })
            .collect();
        nodes.extend(leaves.into_iter().enumerate().map(|(i, name)| {
            let node = Node::new(NodeKind::Key, name, false);
            match types.get(i) {
                Some(t) => node.with_detail(t.clone()),
                None => node,
            }
        }));
        if more {
            nodes.push(
                Node::new(NodeKind::Other, "…", false)
                    .with_detail(format!("only the first {SCAN_LIMIT} keys are shown")),
            );
        }
        Ok(nodes)
    }

    /// The type of `key`, as `TYPE` names it.
    async fn key_type(&mut self, key: &str) -> Result<String, DbError> {
        let t = convert::text(&self.run(&["TYPE", key]).await?);
        if t == "none" {
            return Err(DbError::General(format!("no key {key}")));
        }
        Ok(t)
    }

    /// How many items `key` of `key_type` holds; bytes for a string.
    async fn length(&mut self, key: &str, key_type: &str) -> Result<Option<u64>, DbError> {
        let command = match key_type {
            "string" => "STRLEN",
            "hash" => "HLEN",
            "list" => "LLEN",
            "set" => "SCARD",
            "zset" => "ZCARD",
            "stream" => "XLEN",
            _ => return Ok(None),
        };
        match self.run(&[command, key]).await? {
            Value::Int(n) => Ok(u64::try_from(n).ok()),
            _ => Ok(None),
        }
    }
}

/// `key: count` for each database `INFO keyspace` lists.
fn keyspace(info: &str) -> BTreeMap<i64, u64> {
    info.lines()
        .filter_map(|line| {
            let (db, rest) = line.strip_prefix("db")?.split_once(':')?;
            let keys = rest.split(',').find_map(|kv| kv.strip_prefix("keys="))?;
            Some((db.parse().ok()?, keys.trim().parse().ok()?))
        })
        .collect()
}

/// The prefixes, with how many keys each holds, and the keys right under
/// `prefix` among `keys`.
fn group(prefix: &str, keys: &[String]) -> (BTreeMap<String, usize>, Vec<String>) {
    let mut prefixes = BTreeMap::new();
    let mut leaves = Vec::new();
    for key in keys {
        let Some(rest) = key.strip_prefix(prefix) else {
            continue;
        };
        match rest.find(':') {
            Some(i) => *prefixes.entry(rest[..=i].to_string()).or_insert(0) += 1,
            None => leaves.push(rest.to_string()),
        }
    }
    leaves.sort();
    (prefixes, leaves)
}

/// `text` matched literally in a `SCAN` pattern.
fn glob_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `PTTL`'s answer in words.
fn expiry(ms: i64) -> String {
    match ms {
        -1 => "no expiry".to_string(),
        ms if ms < 0 => "gone".to_string(),
        ms => {
            let s = ms / 1000;
            let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
            let left = match (d, h, m) {
                (0, 0, 0) => format!("{}s", s % 60),
                (0, 0, m) => format!("{m}m {}s", s % 60),
                (0, h, m) => format!("{h}h {m}m"),
                (d, h, _) => format!("{d}d {h}h"),
            };
            format!("expires in {left}")
        }
    }
}

impl Catalog for RedisCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        Box::pin(async move {
            if node.parts().is_empty() {
                return self.databases().await;
            }
            let prefix = self.enter(node).await?;
            self.keys(&prefix).await
        })
    }

    fn describe<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<EntityInfo, DbError>> {
        Box::pin(async move {
            let key = self.enter(node).await?;
            let key_type = self.key_type(&key).await?;
            let length = self.length(&key, &key_type).await?;
            let ttl = match self.run(&["PTTL", &key]).await? {
                Value::Int(ms) => expiry(ms),
                _ => String::new(),
            };
            let size = match (key_type.as_str(), length) {
                ("string", Some(n)) => format!(", {n} bytes"),
                (_, Some(n)) => format!(", {n} items"),
                _ => String::new(),
            };
            Ok(EntityInfo {
                row_estimate: length.filter(|_| key_type != "string"),
                comment: Some(format!("{key_type}{size}, {ttl}")),
                ..Default::default()
            })
        })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        Box::pin(async move {
            let key = self.enter(node).await?;
            let key_type = self.key_type(&key).await?;
            let limit = query.limit.map(|n| n as usize);
            let text = dfox_sql::redis::open_command(&key_type, &key, limit);
            let command = dfox_sql::redis::words(&text).map_err(DbError::General)?;
            let mut conn = self.conn.clone();
            Ok(spawn_run(move |tx| async move {
                let value = super::query(&mut conn, &command).await?;
                send_table(
                    convert::table(&command, value),
                    limit.unwrap_or(usize::MAX),
                    &tx,
                )
                .await
            }))
        })
    }

    fn count_rows<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let key = self.enter(node).await?;
            let key_type = self.key_type(&key).await?;
            Ok(self.length(&key, &key_type).await?.unwrap_or(1))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_group_by_their_next_prefix() {
        let keys: Vec<String> = ["user:1", "user:2", "user:orders:7", "user", "user:"]
            .iter()
            .map(|k| format!("app:{k}"))
            .collect();
        let (prefixes, leaves) = group("app:user:", &keys);
        assert_eq!(
            prefixes.into_iter().collect::<Vec<_>>(),
            [("orders:".to_string(), 1)]
        );
        assert_eq!(leaves, ["", "1", "2"]);
        let (prefixes, leaves) = group("app:", &keys);
        assert_eq!(prefixes.get("user:"), Some(&4));
        assert_eq!(leaves, ["user"]);
    }

    #[test]
    fn prefixes_match_literally() {
        assert_eq!(glob_escape("a*b?[c]\\"), "a\\*b\\?\\[c\\]\\\\");
    }

    #[test]
    fn keyspace_lists_databases_with_keys() {
        let info =
            "# Keyspace\r\ndb0:keys=12,expires=1,avg_ttl=0\r\ndb3:keys=1,expires=0,avg_ttl=0\r\n";
        assert_eq!(
            keyspace(info).into_iter().collect::<Vec<_>>(),
            [(0, 12), (3, 1)]
        );
    }

    #[test]
    fn expiry_reads_naturally() {
        assert_eq!(expiry(-1), "no expiry");
        assert_eq!(expiry(42_000), "expires in 42s");
        assert_eq!(expiry(125_000), "expires in 2m 5s");
        assert_eq!(expiry(90_061_000), "expires in 1d 1h");
    }
}
