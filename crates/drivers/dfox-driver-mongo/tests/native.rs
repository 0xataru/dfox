//! The MongoDB session against a real server: the driver test suite with
//! documents, shell commands, columns learnt from documents, the catalog,
//! read-only connections and errors.

use dfox_core::{
    catalog::{BrowseQuery, NodeKind, NodePath},
    driver::Driver,
    models::connections::ConnectionParams,
    session::Session,
};
use dfox_driver_mongo::MongoDriver;
use dfox_testkit::{
    conformance::{check, Fixture},
    execute, only_row, params_from_url, rows, run, scalar, server_url, unique,
};

const URL: &str = "DFOX_TEST_MONGO_URL";

/// Connection details for a database of this run's own.
fn params(database: &str) -> Option<ConnectionParams> {
    let url = server_url(URL)?;
    Some(ConnectionParams {
        database: Some(database.to_string()),
        ..params_from_url(&url)
    })
}

async fn session(database: &str) -> Option<Box<dyn Session>> {
    let params = params(database)?;
    Some(MongoDriver.open(&params).await.expect("open a session"))
}

/// Drops the database when the test ends.
async fn drop_database(s: &mut dyn Session) {
    execute(s, "db.dropDatabase()").await;
}

/// `n` documents `{ n: i }` from no collection at all.
fn generate(n: usize) -> String {
    format!(
        "db.aggregate([{{ $documents: [{{}}] }}, {{ $project: {{ a: {{ $range: [0, 1000] }} }} }}, \
         {{ $unwind: '$a' }}, \
         {{ $project: {{ n: {{ $range: [{{ $multiply: ['$a', 1000] }}, \
                                    {{ $add: [{{ $multiply: ['$a', 1000] }}, 1000] }}] }} }} }}, \
         {{ $unwind: '$n' }}, {{ $limit: {n} }}])"
    )
}

#[tokio::test]
async fn passes_the_driver_suite() {
    let database = unique("dfox_it");
    let Some(mut s) = session(&database).await else {
        return;
    };
    execute(
        s.as_mut(),
        "db.people.insertMany([{ _id: 1, name: 'a' }, { _id: 2, name: 'b' }, { _id: 3, name: 'c' }])",
    )
    .await;
    let fixture = Fixture {
        table: [database.as_str(), "people"]
            .into_iter()
            .collect::<NodePath>(),
        key: "_id",
        select_all: "db.people.find().sort({ _id: 1 })".into(),
        count: "db.people.countDocuments({})".into(),
        generate,
        long_running: "db.people.find({ $where: 'sleep(20000) || true' })".into(),
        insert: |id, name| format!("db.people.insertOne({{ _id: {id}, name: '{name}' }})"),
        begin: String::new(),
        rollback: String::new(),
        types: (
            "db.aggregate([{ $documents: [{ i: 1, l: NumberLong('9007199254740993'), d: 1.5, \
             s: 'text', b: true, t: ISODate('2024-01-02T03:04:05Z'), \
             o: ObjectId('64b7f0c2a1b2c3d4e5f60718'), dec: NumberDecimal('1.10'), \
             obj: { a: 1, s: 'x' }, arr: [1, 'two'] }] }])"
                .into(),
            vec![
                "1",
                "9007199254740993",
                "1.5",
                "text",
                "true",
                "2024-01-02 03:04:05+00:00",
                "64b7f0c2a1b2c3d4e5f60718",
                "1.10",
                r#"{"a":1,"s":"x"}"#,
                r#"[1,"two"]"#,
            ],
        ),
        stream_rows: 1_000_000,
    };
    let params = params(&database).expect("params");
    check(&MongoDriver, &params, &fixture).await;
    drop_database(s.as_mut()).await;
}

#[tokio::test]
async fn shell_commands_read_and_write() {
    let database = unique("dfox_it_shell");
    let Some(mut s) = session(&database).await else {
        return;
    };
    let s = s.as_mut();
    let inserted = run(
        s,
        "db.users.insertMany([{ name: 'ada', age: 36, tags: ['math'] }, \
         { name: 'grace', age: 85 }, { name: 'edsger', age: 72, tags: [] }])",
        10,
    )
    .await
    .expect("insert");
    assert_eq!(inserted.rows_affected, 3);

    let found = run(
        s,
        "db.users.find({ age: { $gt: 40 } }, { _id: 0, name: 1, age: 1 })\n  .sort({ age: -1 })\n  .limit(5)",
        10,
    )
    .await
    .expect("find");
    assert_eq!(found.columns, ["name", "age"]);
    assert_eq!(
        rows(&found),
        [[Some("grace"), Some("85")], [Some("edsger"), Some("72")]]
    );

    let one = run(s, "db.users.findOne({ name: 'ada' }, { _id: 0 })", 10)
        .await
        .expect("findOne");
    assert_eq!(one.columns, ["name", "age", "tags"]);
    assert_eq!(
        only_row(&one),
        [Some("ada"), Some("36"), Some(r#"["math"]"#)]
    );

    let count = run(s, "db.users.countDocuments({ age: { $lt: 80 } })", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&count), Some("2"));
    let count = run(s, "db.users.find({ tags: { $exists: true } }).count()", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&count), Some("2"));

    let updated = run(
        s,
        "db.users.updateMany({ age: { $gt: 70 } }, { $inc: { age: 1 } })",
        10,
    )
    .await
    .expect("update");
    assert_eq!(updated.rows_affected, 2);
    let grouped = run(
        s,
        "db.users.aggregate([{ $group: { _id: null, oldest: { $max: '$age' } } }, { $project: { _id: 0 } }])",
        10,
    )
    .await
    .expect("aggregate");
    assert_eq!(scalar(&grouped), Some("86"));

    let distinct = run(s, "db.users.distinct('tags')", 10)
        .await
        .expect("distinct");
    assert_eq!(rows(&distinct), [[Some("math")]]);

    // Commands run in turn.
    let deleted = run(
        s,
        "db.users.deleteOne({ name: 'ada' }); db.users.deleteMany({ age: { $gt: 0 } })",
        10,
    )
    .await
    .expect("delete");
    assert_eq!(deleted.rows_affected, 3);

    let err = run(s, "db.users.mapReduce()", 10)
        .await
        .expect_err("unknown method");
    assert!(
        err.to_string().contains("doesn't run db.users.mapReduce()"),
        "{err}"
    );
    let err = run(s, "SELECT 1", 10).await.expect_err("not shell");
    assert!(
        err.to_string().contains("dfox runs shell commands"),
        "{err}"
    );
    drop_database(s).await;
}

#[tokio::test]
async fn use_switches_the_database_for_what_follows() {
    let database = unique("dfox_it_use");
    let other = unique("dfox_it_use_other");
    let Some(mut s) = session(&database).await else {
        return;
    };
    let s = s.as_mut();
    execute(s, &format!("use {other}\ndb.things.insertOne({{ x: 1 }})")).await;
    let here = run(s, "db.getName()", 10).await.expect("name");
    assert_eq!(scalar(&here), Some(other.as_str()));
    let dbs = run(s, "show dbs", 100).await.expect("show dbs");
    assert!(rows(&dbs).iter().any(|r| r[0] == Some(other.as_str())));
    let collections = run(s, "show collections", 10)
        .await
        .expect("show collections");
    assert_eq!(rows(&collections), [[Some("things")]]);
    drop_database(s).await;
}

#[tokio::test]
async fn late_fields_widen_the_result() {
    let database = unique("dfox_it_wide");
    let Some(mut s) = session(&database).await else {
        return;
    };
    let s = s.as_mut();
    // More documents than one batch holds, and the last brings a field.
    let docs: Vec<String> = (0..6000).map(|i| format!("{{ _id: {i} }}")).collect();
    execute(s, &format!("db.wide.insertMany([{}])", docs.join(", "))).await;
    execute(s, "db.wide.insertOne({ _id: 6000, late: 'here' })").await;
    let out = run(s, "db.wide.find().sort({ _id: 1 })", 100_000)
        .await
        .expect("find");
    assert_eq!(out.columns, ["_id", "late"]);
    assert_eq!(out.rows.len(), 6001);
    assert_eq!(out.rows[6000][1].as_deref(), Some("here"));
    assert_eq!(out.rows[0][1], None);
    drop_database(s).await;
}

#[tokio::test]
async fn the_catalog_learns_fields_and_shows_definitions() {
    let database = unique("dfox_it_catalog");
    let Some(mut s) = session(&database).await else {
        return;
    };
    execute(
        s.as_mut(),
        "db.orders.insertMany([{ _id: 1, total: 9.5, customer: 'a' }, { _id: 2, total: 3, note: null }]);\n\
         db.orders.createIndex({ customer: 1, total: -1 }, { unique: true, name: 'by_customer' });\n\
         db.createView('big', 'orders', [{ $match: { total: { $gt: 5 } } }]);\n\
         db.createCollection('capped', { capped: true, size: 4096 })",
    )
    .await;
    let db = NodePath::root().child(database.clone());
    let children: Vec<_> = s
        .catalog()
        .children(&db)
        .await
        .expect("collections")
        .into_iter()
        .map(|n| (n.name, n.kind, n.detail))
        .collect();
    assert_eq!(
        children,
        [
            ("big".to_string(), NodeKind::View, None),
            (
                "capped".to_string(),
                NodeKind::Collection,
                Some("~0".to_string())
            ),
            (
                "orders".to_string(),
                NodeKind::Collection,
                Some("~2".to_string())
            ),
        ]
    );

    let info = s
        .catalog()
        .describe(&db.child("orders"))
        .await
        .expect("describe");
    let columns: Vec<_> = info
        .columns
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.type_name.as_str(),
                c.nullable,
                c.primary_key,
            )
        })
        .collect();
    assert_eq!(
        columns,
        [
            ("_id", "int", false, true),
            ("total", "double", false, false),
            ("customer", "string", true, false),
            ("note", "null", true, false),
        ]
    );
    let index = info
        .indexes
        .iter()
        .find(|i| i.name == "by_customer")
        .expect("index");
    assert_eq!(index.columns, ["customer", "total"]);
    assert!(index.unique);
    assert!(
        index
            .definition
            .starts_with("db.orders.createIndex({\n  \"customer\": 1,\n  \"total\": -1\n}"),
        "{}",
        index.definition
    );
    assert_eq!(info.row_estimate, Some(2));

    let view = s
        .catalog()
        .definition(&db.child("big"), NodeKind::View)
        .await
        .expect("view");
    assert!(
        view.starts_with("db.createView(\"big\", \"orders\", ["),
        "{view}"
    );
    let capped = s
        .catalog()
        .definition(&db.child("capped"), NodeKind::Collection)
        .await
        .expect("collection");
    assert!(capped.contains("\"capped\": true"), "{capped}");
    assert_eq!(
        s.catalog()
            .definition(&db.child("orders"), NodeKind::Table)
            .await
            .expect("orders"),
        "db.createCollection(\"orders\");\n"
    );

    let count = s
        .catalog()
        .count_rows(&db.child("orders"))
        .await
        .expect("count");
    assert_eq!(count, 2);
    let stream = s
        .catalog()
        .browse(&db.child("orders"), BrowseQuery::default())
        .await
        .expect("browse");
    drop(stream);
    drop_database(s.as_mut()).await;
}

#[tokio::test]
async fn read_only_connections_refuse_writes() {
    let database = unique("dfox_it_ro");
    let Some(params) = params(&database) else {
        return;
    };
    let mut s = MongoDriver
        .open(&ConnectionParams {
            read_only: true,
            ..params
        })
        .await
        .expect("open");
    let err = run(s.as_mut(), "db.users.insertOne({ a: 1 })", 10)
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("read-only"), "{err}");
    let ok = run(s.as_mut(), "db.users.find()", 10).await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn a_wrong_password_fails_to_connect() {
    let Some(params) = params("admin") else {
        return;
    };
    let err = MongoDriver
        .open(&ConnectionParams {
            password: "wrong".into(),
            ..params
        })
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().starts_with("Connection error"), "{err}");
}

#[tokio::test]
async fn grid_edits_keep_each_fields_type() {
    use dfox_core::{
        result::OwnedCell,
        session::{Change, ChangeSet},
    };

    let database = unique("dfox_it");
    let Some(mut s) = session(&database).await else {
        return;
    };
    let hex = "64b7f0c2a1b2c3d4e5f60718";
    execute(
        s.as_mut(),
        &format!(
            "db.people.insertOne({{ _id: ObjectId('{hex}'), name: 'Ann', age: 30, code: '123', tags: ['a'] }})"
        ),
    )
    .await;
    let table: NodePath = [database.as_str(), "people"].into_iter().collect();
    let changes = |changes| ChangeSet {
        table: table.clone(),
        key: vec!["_id".into()],
        changes,
    };
    // As the grid sends them: text, and the ObjectId as its hex.
    let report = s
        .apply(changes(vec![
            Change::Update {
                key: vec![OwnedCell::Text(hex.into())],
                set: vec![
                    ("age".into(), OwnedCell::Text("31".into())),
                    ("code".into(), OwnedCell::Text("124".into())),
                    ("tags".into(), OwnedCell::Text("['b', 'c']".into())),
                    ("added".into(), OwnedCell::Text("42".into())),
                ],
            },
            Change::Insert(vec![("name".into(), OwnedCell::Text("Bob".into()))]),
        ]))
        .await
        .expect("apply");
    assert_eq!((report.updated, report.inserted), (1, 1));
    let types = run(
        s.as_mut(),
        "db.people.aggregate([{ $match: { name: 'Ann' } }, { $project: { _id: 0, \
         age: { $type: '$age' }, code: { $type: '$code' }, tags: '$tags', added: { $type: '$added' } } }])",
        10,
    )
    .await
    .expect("types");
    assert_eq!(
        only_row(&types),
        [
            Some("int"),
            Some("string"),
            Some("[\"b\",\"c\"]"),
            Some("int")
        ]
    );
    let bob = run(s.as_mut(), "db.people.countDocuments({ name: 'Bob' })", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&bob), Some("1"));

    // A value that doesn't fit the field is refused, naming it.
    let err = s
        .apply(changes(vec![Change::Update {
            key: vec![OwnedCell::Text(hex.into())],
            set: vec![("age".into(), OwnedCell::Text("old".into()))],
        }]))
        .await
        .expect_err("not a number");
    assert!(err.to_string().contains("age"), "{err}");

    let report = s
        .apply(changes(vec![Change::Delete(vec![OwnedCell::Text(
            hex.into(),
        )])]))
        .await
        .expect("delete");
    assert_eq!(report.deleted, 1);
    drop_database(s.as_mut()).await;

    // A read-only connection writes nothing.
    let Some(params) = params(&database) else {
        return;
    };
    let mut read_only = MongoDriver
        .open(&ConnectionParams {
            read_only: true,
            ..params
        })
        .await
        .expect("open");
    let err = read_only
        .apply(changes(vec![Change::Delete(vec![OwnedCell::Int(1)])]))
        .await
        .expect_err("read-only");
    assert!(err.to_string().contains("read-only"), "{err}");
}
