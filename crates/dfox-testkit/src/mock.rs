//! A driver for an in-memory database, so the session interfaces and the
//! driver test suite can be checked without a server.
//!
//! It holds one table, `mock.main.people (id INTEGER PRIMARY KEY, name TEXT)`
//! with the rows 1 `a`, 2 `b`, 3 `c`, and understands a few statements,
//! separated by `;`:
//!
//! - `SELECT * FROM people`: the table, by id
//! - `COUNT people`: how many rows it has
//! - `GENERATE n`: `n` rows of one integer column
//! - `TYPES`: one row with a value of every kind
//! - `SLEEP ms`: waits, or stops early when cancelled
//! - `INSERT id name`: adds a row
//! - `BEGIN`, `COMMIT`, `ROLLBACK`

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int64Array,
    NullArray, RecordBatch, StringArray,
};
use arrow_schema::{DataType, SchemaRef};
use dfox_core::{
    arrow::{field, json_field, schema, Frame, ResultStream},
    catalog::{
        BrowseQuery, Catalog, ColumnInfo, EntityInfo, FilterOp, Node, NodeKind, NodePath, Order,
    },
    driver::{Driver, DriverInfo},
    errors::DbError,
    models::{
        connections::{ConnectionParams, DbType},
        schema::TableColumn,
    },
    result::OwnedCell,
    session::{
        ApplyReport, CancelHandle, Capabilities, Change, ChangeSet, QueryLanguage, Request,
        Session, SqlDialect,
    },
};
use futures_util::{future::BoxFuture, stream, StreamExt};
use tokio::sync::Notify;

/// Rows per batch the mock sends.
const BATCH: usize = 8192;

/// Opens [`MockSession`]s on a fresh copy of the mock database.
#[derive(Debug, Clone, Copy, Default)]
pub struct MockDriver;

impl Driver for MockDriver {
    fn db_type(&self) -> DbType {
        DbType::Sqlite
    }

    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "mock",
            name: "Mock",
            default_port: None,
            url_schemes: &["mock"],
        }
    }

    fn open<'a>(
        &'a self,
        _params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(async { Ok(Box::new(MockSession::default()) as Box<dyn Session>) })
    }
}

type Row = (i64, String);

/// The data, shared by the session and its catalog.
#[derive(Debug)]
struct Data {
    people: Vec<Row>,
    /// The rows as they were at `BEGIN`.
    saved: Option<Vec<Row>>,
}

/// A session on the mock database.
#[derive(Debug)]
pub struct MockSession {
    data: Arc<Mutex<Data>>,
    catalog: MockCatalog,
    cancelled: Arc<AtomicBool>,
    wake: Arc<Notify>,
}

impl Default for MockSession {
    fn default() -> Self {
        let data = Arc::new(Mutex::new(Data {
            people: vec![(1, "a".into()), (2, "b".into()), (3, "c".into())],
            saved: None,
        }));
        Self {
            catalog: MockCatalog { data: data.clone() },
            data,
            cancelled: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Notify::new()),
        }
    }
}

fn lock(data: &Mutex<Data>) -> std::sync::MutexGuard<'_, Data> {
    data.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn people_schema() -> SchemaRef {
    schema(vec![
        field("id", DataType::Int64, "INTEGER"),
        field("name", DataType::Utf8, "TEXT"),
    ])
}

fn people_batch(rows: &[Row]) -> Result<RecordBatch, DbError> {
    let ids: ArrayRef = Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.0)));
    let names: ArrayRef = Arc::new(StringArray::from_iter_values(rows.iter().map(|r| &r.1)));
    batch(people_schema(), vec![ids, names])
}

fn batch(schema: SchemaRef, arrays: Vec<ArrayRef>) -> Result<RecordBatch, DbError> {
    RecordBatch::try_new(schema, arrays).map_err(|err| DbError::General(err.to_string()))
}

/// The frames for a set of rows of the people table.
fn people_frames(rows: Vec<Row>) -> Vec<Result<Frame, DbError>> {
    let mut frames = vec![Ok(Frame::Schema(people_schema()))];
    frames.extend(
        rows.chunks(BATCH)
            .map(|c| people_batch(c).map(Frame::Batch)),
    );
    frames
}

/// One row with a value of every kind the suite checks.
fn types_frames() -> Vec<Result<Frame, DbError>> {
    let schema = schema(vec![
        field("int", DataType::Int64, "INTEGER"),
        field("float", DataType::Float64, "REAL"),
        field("bool", DataType::Boolean, "BOOLEAN"),
        field("text", DataType::Utf8, "TEXT"),
        field("decimal", DataType::Decimal128(10, 2), "NUMERIC"),
        field("date", DataType::Date32, "DATE"),
        field("bytes", DataType::Binary, "BLOB"),
        json_field("json", "JSON"),
        field("null", DataType::Null, "NULL"),
    ]);
    let decimal = Decimal128Array::from(vec![12345])
        .with_precision_and_scale(10, 2)
        .map_err(|err| DbError::General(err.to_string()));
    let decimal = match decimal {
        Ok(d) => d,
        Err(err) => return vec![Err(err)],
    };
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![-42])),
        Arc::new(Float64Array::from(vec![2.5])),
        Arc::new(BooleanArray::from(vec![true])),
        Arc::new(StringArray::from(vec!["héllo"])),
        Arc::new(decimal),
        Arc::new(Date32Array::from(vec![19_000])),
        Arc::new(BinaryArray::from(vec![&b"\xde\xad"[..]])),
        Arc::new(StringArray::from(vec![r#"{"a":1}"#])),
        Arc::new(NullArray::new(1)),
    ];
    vec![
        Ok(Frame::Schema(schema.clone())),
        batch(schema, arrays).map(Frame::Batch),
    ]
}

/// `n` integers, made one batch at a time, stopping when cancelled.
fn generate(n: usize, row_limit: usize, cancelled: Arc<AtomicBool>) -> ResultStream {
    let schema = schema(vec![field("n", DataType::Int64, "INTEGER")]);
    let kept = n.min(row_limit);
    let head = stream::iter([Ok(Frame::Schema(schema.clone()))]);
    let batches = stream::iter((0..kept).step_by(BATCH)).then(move |start| {
        let schema = schema.clone();
        let cancelled = cancelled.clone();
        async move {
            tokio::task::yield_now().await;
            if cancelled.load(Ordering::Relaxed) {
                return Err(DbError::Cancelled);
            }
            let end = kept.min(start + BATCH) as i64;
            let array: ArrayRef = Arc::new(Int64Array::from_iter_values(start as i64..end));
            batch(schema, vec![array]).map(Frame::Batch)
        }
    });
    Box::pin(head.chain(batches))
}

impl MockSession {
    /// Runs one statement into frames, or a stream for the big ones.
    fn statement(
        &self,
        text: &str,
        row_limit: usize,
    ) -> Result<Vec<Result<Frame, DbError>>, ResultStream> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut data = lock(&self.data);
        let frames = match words.as_slice() {
            [] => Vec::new(),
            [select, "*", from, "people"]
                if select.eq_ignore_ascii_case("select") && from.eq_ignore_ascii_case("from") =>
            {
                let mut rows = data.people.clone();
                rows.sort();
                rows.truncate(row_limit);
                people_frames(rows)
            }
            ["COUNT", "people"] => {
                let schema = schema(vec![field("count", DataType::Int64, "INTEGER")]);
                let count: ArrayRef = Arc::new(Int64Array::from(vec![data.people.len() as i64]));
                vec![
                    Ok(Frame::Schema(schema.clone())),
                    batch(schema, vec![count]).map(Frame::Batch),
                ]
            }
            ["GENERATE", n] => match n.parse::<usize>() {
                Ok(n) => return Err(generate(n, row_limit, self.cancelled.clone())),
                Err(_) => vec![Err(DbError::General(format!("mock: bad count {n}")))],
            },
            ["TYPES"] => types_frames(),
            ["SLEEP", ms] => {
                let ms = ms.parse().unwrap_or(0);
                let cancelled = self.cancelled.clone();
                let wake = self.wake.clone();
                return Err(Box::pin(stream::once(async move {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(Frame::Affected(0)),
                        _ = wake.notified() => Err(DbError::Cancelled),
                    }
                    .and_then(|frame| {
                        if cancelled.load(Ordering::Relaxed) {
                            Err(DbError::Cancelled)
                        } else {
                            Ok(frame)
                        }
                    })
                })));
            }
            ["INSERT", id, name] => match id.parse() {
                Ok(id) => {
                    data.people.push((id, (*name).to_string()));
                    vec![Ok(Frame::Affected(1))]
                }
                Err(_) => vec![Err(DbError::General(format!("mock: bad id {id}")))],
            },
            ["BEGIN"] => {
                data.saved = Some(data.people.clone());
                Vec::new()
            }
            ["COMMIT"] => {
                data.saved = None;
                Vec::new()
            }
            ["ROLLBACK"] => {
                if let Some(saved) = data.saved.take() {
                    data.people = saved;
                }
                Vec::new()
            }
            _ => vec![Err(DbError::General(format!(
                "mock: unknown statement {text:?}"
            )))],
        };
        Ok(frames)
    }
}

impl Session for MockSession {
    fn caps(&self) -> Capabilities {
        Capabilities::TRANSACTIONS
            | Capabilities::SCHEMAS
            | Capabilities::CANCEL
            | Capabilities::EDIT_BY_PK
            | Capabilities::MULTI_STATEMENT
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::Sql(SqlDialect::Generic)
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        self.cancelled.store(false, Ordering::Relaxed);
        let row_limit = request.row_limit.unwrap_or(usize::MAX);
        let mut out: ResultStream = Box::pin(stream::empty());
        for statement in request.text.split(';') {
            let next: ResultStream = match self.statement(statement.trim(), row_limit) {
                Ok(frames) => Box::pin(stream::iter(frames)),
                Err(stream) => stream,
            };
            out = Box::pin(out.chain(next));
        }
        // Stop at the first error, as a server does.
        let mut failed = false;
        let out = out.take_while(move |frame| {
            let keep = !failed;
            failed |= frame.is_err();
            futures_util::future::ready(keep)
        });
        Box::pin(async move { Ok(Box::pin(out) as ResultStream) })
    }

    fn cancel_handle(&self) -> CancelHandle {
        let cancelled = self.cancelled.clone();
        let wake = self.wake.clone();
        CancelHandle::new(move || {
            cancelled.store(true, Ordering::Relaxed);
            wake.notify_waiters();
            Box::pin(async { Ok(true) })
        })
    }

    fn apply(&mut self, changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        let result = (|| {
            if changes.table.name() != Some("people") || changes.key != ["id"] {
                return Err(DbError::General(
                    "mock: only people(id) can be edited".into(),
                ));
            }
            let mut data = lock(&self.data);
            let mut report = ApplyReport::default();
            let id = |values: &[OwnedCell]| match values {
                [OwnedCell::Int(id)] => Ok(*id),
                _ => Err(DbError::General("mock: the key is one integer".into())),
            };
            for change in changes.changes {
                match change {
                    Change::Insert(values) => match values.as_slice() {
                        [OwnedCell::Int(id), OwnedCell::Text(name)] => {
                            data.people.push((*id, name.to_string()));
                            report.inserted += 1;
                        }
                        _ => return Err(DbError::General("mock: insert (id, name)".into())),
                    },
                    Change::Update { key, set } => {
                        let id = id(&key)?;
                        for row in data.people.iter_mut().filter(|r| r.0 == id) {
                            for (column, value) in &set {
                                if let ("name", OwnedCell::Text(name)) = (column.as_str(), value) {
                                    row.1 = name.to_string();
                                }
                            }
                            report.updated += 1;
                        }
                    }
                    Change::Delete(key) => {
                        let id = id(&key)?;
                        let before = data.people.len();
                        data.people.retain(|r| r.0 != id);
                        report.deleted += (before - data.people.len()) as u64;
                    }
                }
            }
            Ok(report)
        })();
        Box::pin(async move { result })
    }
}

/// The mock's tree: `mock` ▸ `main` ▸ `people` ▸ its columns.
#[derive(Debug)]
pub struct MockCatalog {
    data: Arc<Mutex<Data>>,
}

fn not_found(node: &NodePath) -> DbError {
    DbError::General(format!("mock: no such node {:?}", node.parts()))
}

impl Catalog for MockCatalog {
    fn children<'a>(&'a mut self, node: &'a NodePath) -> BoxFuture<'a, Result<Vec<Node>, DbError>> {
        let parts: Vec<&str> = node.parts().iter().map(String::as_str).collect();
        let rows = lock(&self.data).people.len();
        let result = match parts.as_slice() {
            [] => Ok(vec![Node::new(NodeKind::Database, "mock", true)]),
            ["mock"] => Ok(vec![Node::new(NodeKind::Schema, "main", true)]),
            ["mock", "main"] => Ok(vec![
                Node::new(NodeKind::Table, "people", true).with_detail(format!("{rows} rows"))
            ]),
            ["mock", "main", "people"] => Ok(vec![
                Node::new(NodeKind::Column, "id", false).with_detail("INTEGER"),
                Node::new(NodeKind::Column, "name", false).with_detail("TEXT"),
            ]),
            _ => Err(not_found(node)),
        };
        Box::pin(async move { result })
    }

    fn describe<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<EntityInfo, DbError>> {
        let result = if node.parts() == ["mock", "main", "people"] {
            let column = |name: &str, type_name: &str, primary_key: bool| ColumnInfo {
                name: name.into(),
                type_name: type_name.into(),
                nullable: !primary_key,
                default: None,
                primary_key,
            };
            Ok(EntityInfo {
                columns: vec![column("id", "INTEGER", true), column("name", "TEXT", false)],
                indexes: Vec::new(),
                row_estimate: Some(lock(&self.data).people.len() as u64),
            })
        } else {
            Err(not_found(node))
        };
        Box::pin(async move { result })
    }

    fn browse<'a>(
        &'a mut self,
        node: &'a NodePath,
        query: BrowseQuery,
    ) -> BoxFuture<'a, Result<ResultStream, DbError>> {
        if node.parts() != ["mock", "main", "people"] {
            let err = not_found(node);
            return Box::pin(async move { Err(err) });
        }
        let mut rows = lock(&self.data).people.clone();
        for filter in &query.filters {
            rows.retain(|(id, name)| {
                let value = match filter.column.as_str() {
                    "id" => OwnedCell::Int(*id),
                    _ => OwnedCell::Text(name.as_str().into()),
                };
                match (filter.op, &value, &filter.value) {
                    (FilterOp::Eq, a, b) => a == b,
                    (FilterOp::Ne, a, b) => a != b,
                    (FilterOp::Lt, OwnedCell::Int(a), OwnedCell::Int(b)) => a < b,
                    (FilterOp::Le, OwnedCell::Int(a), OwnedCell::Int(b)) => a <= b,
                    (FilterOp::Gt, OwnedCell::Int(a), OwnedCell::Int(b)) => a > b,
                    (FilterOp::Ge, OwnedCell::Int(a), OwnedCell::Int(b)) => a >= b,
                    (FilterOp::Contains, OwnedCell::Text(a), OwnedCell::Text(b)) => {
                        a.to_lowercase().contains(&b.to_lowercase())
                    }
                    (FilterOp::IsNull, _, _) => false,
                    (FilterOp::NotNull, _, _) => true,
                    _ => false,
                }
            });
        }
        rows.sort();
        for (column, order) in query.sort.iter().rev() {
            if column == "name" {
                rows.sort_by(|a, b| a.1.cmp(&b.1));
            } else {
                rows.sort_by_key(|r| r.0);
            }
            if *order == Order::Descending {
                rows.reverse();
            }
        }
        let rows: Vec<Row> = rows
            .into_iter()
            .skip(query.offset as usize)
            .take(query.limit.map_or(usize::MAX, |l| l as usize))
            .collect();
        let stream: ResultStream = Box::pin(stream::iter(people_frames(rows)));
        Box::pin(async move { Ok(stream) })
    }

    fn all_columns<'a>(
        &'a mut self,
        node: &'a NodePath,
    ) -> BoxFuture<'a, Result<Vec<TableColumn>, DbError>> {
        let result = if node.parts() == ["mock", "main"] {
            let column = |name: &str, data_type: &str| TableColumn {
                table: "people".into(),
                name: name.into(),
                data_type: data_type.into(),
            };
            Ok(vec![column("id", "INTEGER"), column("name", "TEXT")])
        } else {
            Err(not_found(node))
        };
        Box::pin(async move { result })
    }
}
