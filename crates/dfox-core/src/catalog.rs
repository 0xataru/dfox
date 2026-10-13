//! Browsing what a database holds as a tree of nodes, the same for every
//! kind of database: servers, databases, schemas, tables, collections, keys.

use futures_util::future::BoxFuture;

use crate::{arrow::ResultStream, errors::DbError};

/// What a node in the tree is. The explorer picks icons and actions from it
/// without knowing the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A server, the root of a connection.
    Server,
    /// A database on a server.
    Database,
    /// A schema in a database.
    Schema,
    /// A table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A table backed by another server.
    ForeignTable,
    /// A function.
    Function,
    /// A stored procedure.
    Procedure,
    /// A document collection.
    Collection,
    /// A keyspace, as in Cassandra.
    Keyspace,
    /// A group of keys sharing a prefix, as in Redis.
    KeyPrefix,
    /// A column of a table.
    Column,
    /// A grouping node, such as "Tables" under a schema.
    Folder,
    /// Anything else the driver shows.
    Other,
}

impl NodeKind {
    /// True for nodes whose rows can be browsed.
    pub fn has_rows(self) -> bool {
        matches!(
            self,
            NodeKind::Table
                | NodeKind::View
                | NodeKind::MaterializedView
                | NodeKind::ForeignTable
                | NodeKind::Collection
        )
    }
}

/// One entry in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// What it is.
    pub kind: NodeKind,
    /// Its name, unique among its siblings.
    pub name: String,
    /// True when expanding it can list more nodes.
    pub has_children: bool,
    /// A short note shown beside it: a row estimate, a column type.
    pub detail: Option<String>,
}

impl Node {
    /// A node with no detail.
    pub fn new(kind: NodeKind, name: impl Into<String>, has_children: bool) -> Self {
        Self {
            kind,
            name: name.into(),
            has_children,
            detail: None,
        }
    }

    /// The node with a note beside it.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Where a node is: the names from the root down to it. The root itself is
/// the empty path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NodePath(pub Vec<String>);

impl NodePath {
    /// The root of the tree.
    pub fn root() -> Self {
        Self::default()
    }

    /// The path of a child named `name`.
    pub fn child(&self, name: impl Into<String>) -> Self {
        let mut path = self.0.clone();
        path.push(name.into());
        Self(path)
    }

    /// The node's own name; `None` for the root.
    pub fn name(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }

    /// The names, from the root down.
    pub fn parts(&self) -> &[String] {
        &self.0
    }
}

impl<S: Into<String>> FromIterator<S> for NodePath {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        Self(iter.into_iter().map(Into::into).collect())
    }
}

/// A column of a table, as [`Catalog::describe`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// The column's name.
    pub name: String,
    /// The database's name for its type.
    pub type_name: String,
    /// True when it may hold NULL.
    pub nullable: bool,
    /// Its default, as the database writes it.
    pub default: Option<String>,
    /// True when it is part of the primary key.
    pub primary_key: bool,
}

/// An index of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    /// The index's name.
    pub name: String,
    /// The columns it covers, in order.
    pub columns: Vec<String>,
    /// True when it enforces uniqueness.
    pub unique: bool,
}

/// What a node is made of: columns, keys, indexes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityInfo {
    /// Columns in order.
    pub columns: Vec<ColumnInfo>,
    /// Indexes.
    pub indexes: Vec<IndexInfo>,
    /// The planner's row estimate, when the database keeps one.
    pub row_estimate: Option<u64>,
}

/// How to compare in a [`Filter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
    /// Contains the text, ignoring case.
    Contains,
    /// Is NULL; the value is ignored.
    IsNull,
    /// Is not NULL; the value is ignored.
    NotNull,
}

/// One condition on a column. The driver writes it in its own query language,
/// with the value as a parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// The column.
    pub column: String,
    /// The comparison.
    pub op: FilterOp,
    /// What to compare with.
    pub value: crate::result::OwnedCell,
}

/// Which way to sort a column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// Smallest first.
    #[default]
    Ascending,
    /// Largest first.
    Descending,
}

/// Paging, sorting and filtering a table's rows without writing a query. The
/// driver turns it into SQL, MQL, CQL or whatever its database speaks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrowseQuery {
    /// All of these must hold.
    pub filters: Vec<Filter>,
    /// Sort by these columns, in order.
    pub sort: Vec<(String, Order)>,
    /// At most this many rows.
    pub limit: Option<u64>,
    /// Skip this many rows first.
    pub offset: u64,
}

/// Lists and opens the things a database holds.
pub trait Catalog: Send {
    /// The nodes under `node`, loaded when it is expanded.
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>>;

    /// The columns, keys and indexes of `node`.
    fn describe<'a>(&'a mut self, node: &'a NodePath)
        -> BoxFuture<'a, Result<EntityInfo, DbError>>;

    /// The rows of `node`, filtered, sorted and paged by `query`.
    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_grow_from_the_root() {
        let root = NodePath::root();
        assert_eq!(root.name(), None);
        let table = root.child("shop").child("public").child("orders");
        assert_eq!(table.name(), Some("orders"));
        assert_eq!(table.parts(), ["shop", "public", "orders"]);
        assert_eq!(
            ["shop", "public", "orders"]
                .into_iter()
                .collect::<NodePath>(),
            table
        );
    }

    #[test]
    fn rows_live_in_tables_and_collections() {
        assert!(NodeKind::Table.has_rows());
        assert!(NodeKind::Collection.has_rows());
        assert!(!NodeKind::Schema.has_rows());
        let node = Node::new(NodeKind::Table, "orders", true).with_detail("~12k");
        assert_eq!(node.detail.as_deref(), Some("~12k"));
    }
}
