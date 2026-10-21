//! Browsing what a database holds as a tree of nodes, the same for every
//! kind of database: servers, databases, schemas, tables, collections, keys.

use futures_util::future::BoxFuture;

use crate::{arrow::ResultStream, errors::DbError, models::schema::TableColumn};

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
    /// A key and its value, as in Redis.
    Key,
    /// A column of a table.
    Column,
    /// A constraint of a table: a key or a check.
    Constraint,
    /// An index of a table.
    Index,
    /// A trigger on a table.
    Trigger,
    /// A row-level security policy on a table.
    Policy,
    /// A grouping node, such as "Tables" under a schema.
    Folder,
    /// Anything else the driver shows.
    Other,
}

impl NodeKind {
    /// What it is, in words.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Server => "server",
            NodeKind::Database => "database",
            NodeKind::Schema => "schema",
            NodeKind::Table => "table",
            NodeKind::View => "view",
            NodeKind::MaterializedView => "materialized view",
            NodeKind::ForeignTable => "foreign table",
            NodeKind::Function => "function",
            NodeKind::Procedure => "procedure",
            NodeKind::Collection => "collection",
            NodeKind::Keyspace => "keyspace",
            NodeKind::KeyPrefix => "key prefix",
            NodeKind::Key => "key",
            NodeKind::Column => "column",
            NodeKind::Constraint => "constraint",
            NodeKind::Index => "index",
            NodeKind::Trigger => "trigger",
            NodeKind::Policy => "policy",
            NodeKind::Folder => "folder",
            NodeKind::Other => "object",
        }
    }

    /// True for nodes whose rows can be browsed.
    pub fn has_rows(self) -> bool {
        matches!(
            self,
            NodeKind::Table
                | NodeKind::View
                | NodeKind::MaterializedView
                | NodeKind::ForeignTable
                | NodeKind::Collection
                | NodeKind::Key
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// What its comment says.
    pub comment: Option<String>,
    /// Its collation, when it isn't the one its type or table has.
    pub collation: Option<String>,
    /// For a generated column, the expression it is computed from.
    pub generated: Option<String>,
}

/// A foreign key: columns of one table that point at another's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    /// The constraint's name.
    pub name: String,
    /// The columns holding the reference, in order.
    pub columns: Vec<String>,
    /// The table it points at.
    pub target: NodePath,
    /// The columns there that `columns` match, in the same order.
    pub target_columns: Vec<String>,
}

/// An index of a table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexInfo {
    /// The index's name.
    pub name: String,
    /// The columns it covers, in order.
    pub columns: Vec<String>,
    /// True when it enforces uniqueness.
    pub unique: bool,
    /// The SQL that creates it.
    pub definition: String,
}

/// What kind of rule a [`ConstraintInfo`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// The table's primary key.
    PrimaryKey,
    /// A reference to another table.
    ForeignKey,
    /// Values that must differ between rows.
    Unique,
    /// A condition every row must meet.
    Check,
    /// A Postgres exclusion constraint.
    Exclusion,
}

impl ConstraintKind {
    /// What it is, in words.
    pub fn label(self) -> &'static str {
        match self {
            ConstraintKind::PrimaryKey => "primary key",
            ConstraintKind::ForeignKey => "foreign key",
            ConstraintKind::Unique => "unique",
            ConstraintKind::Check => "check",
            ConstraintKind::Exclusion => "exclusion",
        }
    }
}

/// A constraint of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintInfo {
    /// The constraint's name.
    pub name: String,
    /// What kind of rule it is.
    pub kind: ConstraintKind,
    /// The SQL that adds it to the table.
    pub definition: String,
}

/// A trigger or policy on a table: its name, a short note on when it
/// applies, and the SQL that creates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInfo {
    /// Its name.
    pub name: String,
    /// When it applies: `before insert`, `for select`.
    pub detail: Option<String>,
    /// The SQL that creates it.
    pub definition: String,
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
    /// Foreign keys from this table to others.
    pub foreign_keys: Vec<ForeignKey>,
    /// Keys and checks, foreign keys among them.
    pub constraints: Vec<ConstraintInfo>,
    /// Triggers on the table.
    pub triggers: Vec<RuleInfo>,
    /// Row-level security policies on the table.
    pub policies: Vec<RuleInfo>,
    /// What the table's comment says.
    pub comment: Option<String>,
}

/// How many different values a column holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Distinct {
    /// About this many.
    Count(f64),
    /// About this fraction of the rows, growing with the table.
    Fraction(f64),
}

/// A range of values and the share of rows in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// The smallest value in it, as text.
    pub low: String,
    /// The largest value in it, as text.
    pub high: String,
    /// The fraction of all rows that fall in it.
    pub fraction: f64,
}

/// Where [`ColumnStats`] come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatsSource {
    /// The planner's statistics, gathered by `ANALYZE`.
    #[default]
    Planner,
    /// Counted just now, over the first [`ColumnStats::rows`] rows.
    Profile,
}

/// What is known about the values of a column.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnStats {
    /// Where the numbers come from.
    pub source: StatsSource,
    /// For a profile, the rows it read.
    pub rows: Option<u64>,
    /// The fraction of rows that are NULL.
    pub null_fraction: Option<f64>,
    /// How many different values there are.
    pub distinct: Option<Distinct>,
    /// The most common values, as text, and the fraction of rows holding
    /// each, most common first.
    pub common: Vec<(String, f64)>,
    /// The other values, in ranges from smallest to largest.
    pub histogram: Vec<Bucket>,
    /// The smallest value, as text.
    pub min: Option<String>,
    /// The largest value, as text.
    pub max: Option<String>,
}

/// Rows a profile reads at most, so it stays quick on any table.
pub const PROFILE_ROWS: u64 = 100_000;

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

    /// The SQL that creates `node`, a table, view or routine of this `kind`,
    /// to read or change. Two kinds of object may share a name, hence the kind. A
    /// catalog that can't show it returns an error.
    fn definition<'a>(
        &'a mut self,
        node: &'a NodePath,
        kind: NodeKind,
    ) -> BoxFuture<'a, Result<String, DbError>> {
        let _ = node;
        Box::pin(async move {
            Err(DbError::General(format!(
                "this database can't show the definition of a {}",
                kind.label()
            )))
        })
    }

    /// How many rows the table at `node` holds, counted now.
    fn count_rows<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<u64, DbError>> {
        let _ = node;
        Box::pin(async { Err(DbError::General("this database can't count rows".into())) })
    }

    /// What is known about the values of `column` in the table at `node`:
    /// the planner's statistics, `None` when it has gathered none, or with
    /// `profile`, counted now over the first [`PROFILE_ROWS`] rows.
    fn column_stats<'a>(
        &'a mut self,
        node: &'a NodePath,
        column: &'a str,
        profile: bool,
    ) -> BoxFuture<'a, Result<Option<ColumnStats>, DbError>> {
        let _ = (node, column, profile);
        Box::pin(async { Ok(None) })
    }

    /// Every column of every table and view directly under `node` (a schema,
    /// or a database that has no schemas), by table and then position, in
    /// one go. For autocomplete; a catalog that can't list them cheaply
    /// returns none.
    fn all_columns<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<Vec<TableColumn>, DbError>> {
        let _ = node;
        Box::pin(async { Ok(Vec::new()) })
    }
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
