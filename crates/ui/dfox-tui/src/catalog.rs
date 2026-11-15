//! What autocomplete knows about the database: schemas, their tables, and
//! every table's columns. Filled in the background as the explorer loads
//! schemas.

use std::collections::{BTreeMap, HashMap};

use dfox_core::{
    catalog::{Node, NodeKind, NodePath},
    errors::DbError,
    models::schema::TableColumn,
};

#[derive(Debug, Default)]
pub struct Catalog {
    /// In the server's order; the first is the default schema.
    schemas: Vec<String>,
    tables: BTreeMap<String, Vec<String>>,
    /// Per schema, per table, the columns in order.
    columns: HashMap<String, HashMap<String, Vec<TableColumn>>>,
    /// The server's databases (keyspaces in Cassandra).
    databases: Vec<String>,
    /// Redis keys seen in the explorer's listings, with their type.
    keys: BTreeMap<String, Option<String>>,
    /// Redis key prefixes (`user:`), where they are listed, and whether
    /// their keys have been asked for.
    prefixes: BTreeMap<String, (NodePath, bool)>,
    /// Per schema, the database's own functions: name, and arguments when
    /// the server told them.
    functions: BTreeMap<String, Vec<(String, Option<String>)>>,
}

impl Catalog {
    pub fn on_schemas(&mut self, schemas: &[String]) {
        self.schemas = schemas.to_vec();
    }

    /// Records the tables and views of `schema`.
    pub fn on_tables(&mut self, schema: &str, tables: Vec<String>) {
        self.tables.insert(schema.to_string(), tables);
    }

    /// Records the Redis keys and prefixes listed under `path`, a database
    /// or a prefix in it.
    pub fn on_keys(&mut self, path: &NodePath, nodes: &[Node]) {
        let Some((_, prefix)) = path.parts().split_first() else {
            return;
        };
        let prefix = prefix.concat();
        if let Some((_, asked)) = self.prefixes.get_mut(&prefix) {
            *asked = true;
        }
        for node in nodes {
            let name = format!("{prefix}{}", node.name);
            match node.kind {
                NodeKind::Key => {
                    self.keys.insert(name, node.detail.clone());
                }
                NodeKind::KeyPrefix => {
                    self.prefixes
                        .entry(name)
                        .or_insert_with(|| (path.child(node.name.clone()), false));
                }
                _ => {}
            }
        }
    }

    /// Known keys and prefixes: `(name, type)`, a prefix's type `None` and
    /// its name ending in its separator.
    pub fn keys(&self) -> impl Iterator<Item = (&str, Option<&str>, bool)> {
        let prefixes = self.prefixes.keys().map(|p| (p.as_str(), None, true));
        let keys = self
            .keys
            .iter()
            .map(|(k, t)| (k.as_str(), t.as_deref(), false));
        prefixes.chain(keys)
    }

    /// The longest known prefix of `typed` whose keys haven't been asked
    /// for yet: where to list them. It counts as asked from now on.
    pub fn ask_prefix(&mut self, typed: &str) -> Option<NodePath> {
        let (_, (path, asked)) = self
            .prefixes
            .iter_mut()
            .filter(|(p, (_, asked))| !asked && typed.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())?;
        *asked = true;
        Some(path.clone())
    }

    pub fn on_databases(&mut self, databases: Vec<String>) {
        self.databases = databases;
    }

    pub fn databases(&self) -> &[String] {
        &self.databases
    }

    pub fn on_functions(&mut self, schema: &str, functions: Vec<(String, Option<String>)>) {
        self.functions.insert(schema.to_string(), functions);
    }

    /// The functions of the default schema.
    pub fn functions(&self) -> &[(String, Option<String>)] {
        self.schemas
            .first()
            .and_then(|s| self.functions.get(s))
            .map_or(&[], Vec::as_slice)
    }

    pub fn on_columns(&mut self, schema: &str, result: Result<Vec<TableColumn>, DbError>) {
        // Without columns, tables still complete; nothing to report.
        let Ok(columns) = result else { return };
        let mut by_table: HashMap<String, Vec<TableColumn>> = HashMap::new();
        for column in columns {
            by_table
                .entry(column.table.clone())
                .or_default()
                .push(column);
        }
        self.columns.insert(schema.to_string(), by_table);
    }

    /// Schemas in their order.
    pub fn schemas(&self) -> &[String] {
        &self.schemas
    }

    /// The schema called `name`, ignoring case.
    pub fn schema(&self, name: &str) -> Option<&str> {
        self.schemas
            .iter()
            .chain(self.tables.keys())
            .find(|s| s.eq_ignore_ascii_case(name))
            .map(String::as_str)
    }

    /// Tables of `schema`, or of every loaded schema, the default one first.
    pub fn tables(&self, schema: Option<&str>) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        for (name, tables) in self.ordered() {
            if schema.is_none_or(|s| s.eq_ignore_ascii_case(name)) {
                out.extend(tables.iter().map(|t| (name, t.as_str())));
            }
        }
        out
    }

    /// Loaded schemas, the default first.
    fn ordered(&self) -> Vec<(&str, &Vec<String>)> {
        let rank = |s: &str| {
            self.schemas
                .iter()
                .position(|x| x == s)
                .unwrap_or(usize::MAX)
        };
        let mut all: Vec<(&str, &Vec<String>)> =
            self.tables.iter().map(|(s, t)| (s.as_str(), t)).collect();
        all.sort_by_key(|(s, _)| rank(s));
        all
    }

    /// The columns of `table`, in `schema` or else the first schema that has
    /// it. Names match ignoring case.
    pub fn columns(&self, schema: Option<&str>, table: &str) -> &[TableColumn] {
        let find = |schema: &str| {
            self.columns
                .iter()
                .find(|(s, _)| s.eq_ignore_ascii_case(schema))
                .and_then(|(_, tables)| {
                    tables
                        .iter()
                        .find(|(t, _)| t.eq_ignore_ascii_case(table))
                        .map(|(_, c)| c.as_slice())
                })
        };
        match schema {
            Some(schema) => find(schema),
            None => self.ordered().into_iter().find_map(|(s, _)| find(s)),
        }
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_catalog as sample;

    #[test]
    fn tables_list_the_default_schema_first() {
        let catalog = sample();
        assert_eq!(
            catalog.tables(None),
            [
                ("public", "customers"),
                ("public", "orders"),
                ("audit", "log"),
                ("audit", "orders")
            ]
        );
        assert_eq!(catalog.tables(Some("AUDIT")).len(), 2);
        assert_eq!(catalog.schema("Public"), Some("public"));
        assert_eq!(catalog.schemas(), ["public", "audit"]);
    }

    #[test]
    fn columns_are_found_by_table_and_schema() {
        let catalog = sample();
        let names = |schema, table| -> Vec<String> {
            catalog
                .columns(schema, table)
                .iter()
                .map(|c| c.name.clone())
                .collect()
        };
        assert_eq!(names(None, "Orders"), ["id", "customer_id"]);
        assert_eq!(names(Some("audit"), "orders"), ["at"]);
        assert!(names(None, "nope").is_empty());
    }

    #[test]
    fn a_failed_load_keeps_what_was_known() {
        let mut catalog = sample();
        catalog.on_columns("public", Err(DbError::General("x".into())));
        assert_eq!(catalog.columns(None, "customers").len(), 2);
    }
}
