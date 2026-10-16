use super::*;

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_string).collect()
}

fn kinds(text: &str) -> Vec<(String, Kind)> {
    let lines = lines(text);
    highlight(&lines, Dialect::Postgres)
        .iter()
        .enumerate()
        .flat_map(|(i, pieces)| {
            let line: Vec<char> = lines[i].chars().collect();
            pieces
                .iter()
                .map(move |p| (line[p.start..p.end].iter().collect(), p.kind))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn keywords_strings_numbers_and_comments_are_coloured() {
    assert_eq!(
        kinds("select count(*), name from t -- all\nwhere id > 10 and x = 'a'"),
        [
            ("select".to_string(), Kind::Keyword),
            ("count".to_string(), Kind::Function),
            ("*".to_string(), Kind::Operator),
            ("from".to_string(), Kind::Keyword),
            ("-- all".to_string(), Kind::Comment),
            ("where".to_string(), Kind::Keyword),
            (">".to_string(), Kind::Operator),
            ("10".to_string(), Kind::Number),
            ("and".to_string(), Kind::Keyword),
            ("=".to_string(), Kind::Operator),
            ("'a'".to_string(), Kind::String),
        ]
    );
}

#[test]
fn tokens_spanning_lines_are_split_per_line() {
    assert_eq!(
        kinds("/* one\ntwo */ select"),
        [
            ("/* one".to_string(), Kind::Comment),
            ("two */".to_string(), Kind::Comment),
            ("select".to_string(), Kind::Keyword),
        ]
    );
}

#[test]
fn an_unclosed_string_runs_to_the_end() {
    assert_eq!(
        kinds("select 'abc\ndef"),
        [
            ("select".to_string(), Kind::Keyword),
            ("'abc".to_string(), Kind::String),
            ("def".to_string(), Kind::String),
        ]
    );
}

#[test]
fn wide_characters_count_as_one_column() {
    assert_eq!(
        kinds("select 'é😀' as x"),
        [
            ("select".to_string(), Kind::Keyword),
            ("'é😀'".to_string(), Kind::String),
            ("as".to_string(), Kind::Keyword),
        ]
    );
}

#[test]
fn scripts_split_on_semicolons_outside_strings_and_bodies() {
    let text =
        "select ';';\n\ncreate function f() returns int as $$ begin; end $$;\n-- done\nselect 2";
    let found: Vec<String> = statements(text, Dialect::Postgres)
        .iter()
        .map(|s| slice(&lines(text), s.start, s.end))
        .collect();
    assert_eq!(
        found,
        [
            "select ';';",
            "create function f() returns int as $$ begin; end $$;",
            "select 2"
        ]
    );
}

#[test]
fn the_statement_under_the_cursor() {
    let text = "select 1;\nselect 2;  \n\nselect 3";
    let at = |cursor| {
        statement_at(text, cursor, Dialect::Postgres).map(|s| slice(&lines(text), s.start, s.end))
    };
    assert_eq!(at((0, 0)).as_deref(), Some("select 1;"));
    // Just after the semicolon still runs that statement.
    assert_eq!(at((0, 9)).as_deref(), Some("select 1;"));
    assert_eq!(at((1, 3)).as_deref(), Some("select 2;"));
    // In the blank space after it, the next one.
    assert_eq!(at((1, 11)).as_deref(), Some("select 3"));
    assert_eq!(at((3, 8)).as_deref(), Some("select 3"));
    assert_eq!(
        statement_at("  -- nothing", (0, 3), Dialect::Postgres),
        None
    );
}

#[test]
fn trailing_space_after_the_last_statement_runs_it() {
    let text = "select 1;\n\n";
    let s = statement_at(text, (2, 0), Dialect::Sqlite).expect("a statement");
    assert_eq!(slice(&lines(text), s.start, s.end), "select 1;");
}

#[test]
fn single_table_queries_name_their_table() {
    let table = |sql| single_table(sql, Dialect::Postgres);
    assert_eq!(
        table("select * from public.users where id = 1").as_deref(),
        Some("public.users")
    );
    assert_eq!(table("select * from a join b on a.id = b.id"), None);
    assert_eq!(table("select 1"), None);
    assert_eq!(table("delete from t"), None);
    assert_eq!(table("select * from"), None);
}

#[test]
fn every_dialect_reads_scripts() {
    for dialect in [Dialect::Postgres, Dialect::MySql, Dialect::Sqlite] {
        assert_eq!(statements("select 1; select 2", dialect).len(), 2);
    }
}

#[test]
fn the_keyword_list_is_sorted() {
    assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
    assert!(RESERVED.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn names_are_quoted_only_when_they_must_be() {
    let cases = [
        ("orders", Dialect::Postgres, "orders"),
        ("order_items", Dialect::MySql, "order_items"),
        ("order", Dialect::Postgres, "\"order\""),
        ("user", Dialect::MySql, "`user`"),
        ("key", Dialect::Sqlite, "\"key\""),
        ("Orders", Dialect::Postgres, "\"Orders\""),
        ("my table", Dialect::MySql, "`my table`"),
        ("a`b", Dialect::MySql, "`a``b`"),
        ("1st", Dialect::Postgres, "\"1st\""),
    ];
    for (name, dialect, quoted) in cases {
        assert_eq!(quote_ident(name, dialect), quoted, "{name}");
    }
}
