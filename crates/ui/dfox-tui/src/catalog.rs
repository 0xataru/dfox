//! What autocomplete knows about the database: schemas, their tables, and
//! every table's columns. Filled in the background as the explorer loads
//! schemas.

use std::collections::{BTreeMap, HashMap};

use dfox_core::{errors::DbError, models::schema::TableColumn};

#[derive(Debug, Default)]
pub struct Catalog {
    /// In the server's order; the first is the default schema.
    schemas: Vec<String>,
    tables: BTreeMap<String, Vec<String>>,
    /// Per schema, per table, the columns in order.
    columns: HashMap<String, HashMap<String, Vec<TableColumn>>>,
}

impl Catalog {
    pub fn on_schemas(&mut self, schemas: &[String]) {
        self.schemas = schemas.to_vec();
    }

    /// Records the tables and views of `schema`.
    pub fn on_tables(&mut self, schema: &str, tables: Vec<String>) {
        self.tables.insert(schema.to_string(), tables);
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
