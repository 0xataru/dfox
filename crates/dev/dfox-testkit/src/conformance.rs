//! The test suite every driver's [`Session`] must pass, against its own
//! database: connecting, walking the catalog, listing every column for
//! autocomplete, browsing, every kind of value, streaming a million rows, row
//! limits, cancelling, transactions that stay on the session, and edits from
//! the grid.
//!
//! A driver's tests describe its database in a [`Fixture`] and call
//! [`check`]. Checks for capabilities the session doesn't claim are skipped.

use std::time::{Duration, Instant};

use dfox_core::{
    arrow::{FetchStatus, Fetcher, Overflow, DEFAULT_BUDGET},
    catalog::{BrowseQuery, Filter, FilterOp, NodePath, Order},
    driver::Driver,
    models::connections::ConnectionParams,
    result::{Cell, OwnedCell, ResultSet},
    session::{Capabilities, Change, ChangeSet, Request, Session},
};
use futures_util::StreamExt;

/// What the suite needs to know about a driver's database. The table must
/// exist before the suite runs, as `people (id integer primary key, name
/// text)` holding `(1, 'a'), (2, 'b'), (3, 'c')`; the suite leaves it that
/// way. A document database names its key as it must, such as `_id`.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Where the table is in the catalog tree.
    pub table: NodePath,
    /// The key column's name: `id`, or `_id` in MongoDB.
    pub key: &'static str,
    /// Returns the table's rows, ordered by id.
    pub select_all: String,
    /// Returns one row and one column: how many rows the table has.
    pub count: String,
    /// Returns `n` rows of one integer column.
    pub generate: fn(usize) -> String,
    /// Runs for at least ten seconds unless cancelled.
    pub long_running: String,
    /// Adds the row `(id, name)` to the table.
    pub insert: fn(i64, &str) -> String,
    /// Starts a transaction.
    pub begin: String,
    /// Rolls back the transaction.
    pub rollback: String,
    /// Returns one row with a value of every kind the driver maps, with the
    /// text each must show as.
    pub types: (String, Vec<&'static str>),
    /// Rows streamed by the streaming check: a million by default.
    pub stream_rows: usize,
}

/// Runs every check, panicking with the first failure.
pub async fn check(driver: &dyn Driver, params: &ConnectionParams, fixture: &Fixture) {
    let mut session = driver
        .open(params)
        .await
        .expect("the driver opens a session");
    let name = driver.info().name;
    let session = session.as_mut();
    catalog_walk(session, fixture).await;
    browse(session, fixture).await;
    select(session, fixture).await;
    types(session, fixture).await;
    row_limit(session, fixture).await;
    streaming(session, fixture).await;
    if session.caps().contains(Capabilities::CANCEL) {
        cancel(session, fixture).await;
    }
    if session.caps().contains(Capabilities::TRANSACTIONS) {
        transactions(session, fixture).await;
    }
    if session.caps().contains(Capabilities::EDIT_BY_PK) {
        edits(session, fixture).await;
    }
    eprintln!("conformance: {name} passed");
}

/// Runs `sql` to the end and returns the last result's cells as text.
async fn rows(session: &mut dyn Session, request: Request) -> Vec<Vec<String>> {
    let text = request.text.clone();
    let stream = session
        .run(request)
        .await
        .unwrap_or_else(|err| panic!("{text}: {err}"));
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let status = fetcher
        .fetch()
        .await
        .unwrap_or_else(|err| panic!("{text}: {err}"));
    assert_eq!(status, FetchStatus::Done, "{text}");
    let Some(result) = fetcher.result() else {
        return Vec::new();
    };
    (0..result.row_count())
        .map(|row| {
            (0..result.columns().len())
                .map(|col| {
                    result
                        .cell(row, col)
                        .map(|c| c.to_string())
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect()
}

async fn count(session: &mut dyn Session, fixture: &Fixture) -> String {
    let rows = rows(session, Request::new(&fixture.count)).await;
    rows[0][0].clone()
}

async fn catalog_walk(session: &mut dyn Session, fixture: &Fixture) {
    let mut path = NodePath::root();
    for name in fixture.table.parts() {
        let children = session
            .catalog()
            .children(&path)
            .await
            .unwrap_or_else(|err| panic!("children of {:?}: {err}", path.parts()));
        assert!(
            children.iter().any(|c| &c.name == name),
            "{name:?} is not under {:?}: {children:?}",
            path.parts()
        );
        path = path.child(name.clone());
    }
    let info = session
        .catalog()
        .describe(&fixture.table)
        .await
        .expect("describe the table");
    let names: Vec<&str> = info.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, [fixture.key, "name"]);
    assert!(info.columns[0].primary_key, "the key column is the key");

    // Autocomplete reads every column under the table's parent at once.
    let parts = fixture.table.parts();
    let (table, parent) = parts.split_last().expect("a table path");
    let parent: NodePath = parent.iter().cloned().collect();
    let columns = session
        .catalog()
        .all_columns(&parent)
        .await
        .unwrap_or_else(|err| panic!("all columns of {:?}: {err}", parent.parts()));
    let of_table: Vec<&str> = columns
        .iter()
        .filter(|c| &c.table == table)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(of_table, [fixture.key, "name"], "all columns, in order");
}

async fn browse(session: &mut dyn Session, fixture: &Fixture) {
    let query = BrowseQuery {
        sort: vec![(fixture.key.into(), Order::Descending)],
        limit: Some(2),
        ..Default::default()
    };
    let ids = browse_ids(session, fixture, query).await;
    assert_eq!(ids, ["3", "2"], "browse sorts and limits");

    let query = BrowseQuery {
        filters: vec![Filter {
            column: "name".into(),
            op: FilterOp::Eq,
            value: OwnedCell::Text("b".into()),
        }],
        ..Default::default()
    };
    assert_eq!(
        browse_ids(session, fixture, query).await,
        ["2"],
        "browse filters"
    );

    let query = BrowseQuery {
        sort: vec![(fixture.key.into(), Order::Ascending)],
        offset: 1,
        ..Default::default()
    };
    assert_eq!(
        browse_ids(session, fixture, query).await,
        ["2", "3"],
        "browse pages"
    );
}

async fn browse_ids(
    session: &mut dyn Session,
    fixture: &Fixture,
    query: BrowseQuery,
) -> Vec<String> {
    let stream = session
        .catalog()
        .browse(&fixture.table, query)
        .await
        .expect("browse");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect("browse rows");
    let result = fetcher.result().expect("a result");
    (0..result.row_count())
        .map(|row| {
            result
                .cell(row, 0)
                .map(|c| c.to_string())
                .unwrap_or_default()
        })
        .collect()
}

async fn select(session: &mut dyn Session, fixture: &Fixture) {
    let rows = rows(session, Request::new(&fixture.select_all)).await;
    assert_eq!(rows, [["1", "a"], ["2", "b"], ["3", "c"]]);
}

async fn types(session: &mut dyn Session, fixture: &Fixture) {
    let (sql, expected) = &fixture.types;
    let rows = rows(session, Request::new(sql)).await;
    assert_eq!(rows.len(), 1, "the types query returns one row");
    assert_eq!(&rows[0], expected, "values show as expected");
}

async fn row_limit(session: &mut dyn Session, fixture: &Fixture) {
    let rows = rows(
        session,
        Request::new((fixture.generate)(100)).with_row_limit(10),
    )
    .await;
    assert_eq!(rows.len(), 10, "the row limit keeps 10 of 100 rows");
}

async fn streaming(session: &mut dyn Session, fixture: &Fixture) {
    let n = fixture.stream_rows;
    let stream = session
        .run(Request::new((fixture.generate)(n)))
        .await
        .expect("generate rows");
    let started = Instant::now();
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    assert_eq!(fetcher.fetch().await.expect("stream"), FetchStatus::Done);
    let result = fetcher.result().expect("a result");
    assert_eq!(result.row_count(), n, "every row arrives");
    assert!(
        result.batches().len() > 1 || n < 10_000,
        "rows come in batches"
    );
    assert!(
        matches!(result.cell(n - 1, 0), Some(Cell::Int(_))),
        "integers stay integers"
    );
    eprintln!(
        "conformance: {n} rows in {:?}, {} batches, {} bytes",
        started.elapsed(),
        result.batches().len(),
        result.memory_bytes()
    );
}

async fn cancel(session: &mut dyn Session, fixture: &Fixture) {
    let handle = session.cancel_handle();
    let mut stream = session
        .run(Request::new(&fixture.long_running))
        .await
        .expect("start the long query");
    let started = Instant::now();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.cancel().await
    });
    let mut failed = false;
    while let Some(frame) = stream.next().await {
        failed |= frame.is_err();
    }
    assert!(failed, "a cancelled query ends with an error");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancelling took {:?}",
        started.elapsed()
    );
    assert!(
        canceller.await.expect("join").expect("cancel"),
        "the cancel was sent"
    );
    // The session still works.
    assert_eq!(count(session, fixture).await, "3");
}

async fn transactions(session: &mut dyn Session, fixture: &Fixture) {
    rows(session, Request::new(&fixture.begin)).await;
    rows(session, Request::new((fixture.insert)(99, "z"))).await;
    assert_eq!(
        count(session, fixture).await,
        "4",
        "the insert is seen inside the transaction"
    );
    rows(session, Request::new(&fixture.rollback)).await;
    assert_eq!(count(session, fixture).await, "3", "the rollback undid it");
}

async fn edits(session: &mut dyn Session, fixture: &Fixture) {
    let changes = |changes| ChangeSet {
        table: fixture.table.clone(),
        key: vec!["id".into()],
        changes,
    };
    let report = session
        .apply(changes(vec![
            Change::Insert(vec![
                ("id".into(), OwnedCell::Int(10)),
                ("name".into(), OwnedCell::Text("x".into())),
            ]),
            Change::Update {
                key: vec![OwnedCell::Int(10)],
                set: vec![("name".into(), OwnedCell::Text("y".into()))],
            },
        ]))
        .await
        .expect("insert and update");
    assert_eq!((report.inserted, report.updated), (1, 1));
    let query = BrowseQuery {
        filters: vec![Filter {
            column: "name".into(),
            op: FilterOp::Eq,
            value: OwnedCell::Text("y".into()),
        }],
        ..Default::default()
    };
    assert_eq!(
        browse_ids(session, fixture, query).await,
        ["10"],
        "the edit is there"
    );
    let report = session
        .apply(changes(vec![Change::Delete(vec![OwnedCell::Int(10)])]))
        .await
        .expect("delete");
    assert_eq!(report.deleted, 1);
    assert_eq!(count(session, fixture).await, "3", "the table is as it was");
}
