//! What the cursor wants completed, read from the tokens around it: a table
//! after `FROM`, a column after `SELECT` or `WHERE`, a member after `x.`, and
//! which tables (and aliases) the statement reads.

use sqlparser::tokenizer::{Token, Whitespace};

use super::{lex, rest_of, Dialect, Kind, Lexeme, Pos};

/// A table the statement names: `FROM shop.orders o` → schema `shop`, name
/// `orders`, alias `o`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Tables,
    Columns,
    /// A column type: after `::`, `CAST(x AS`, a column name in `CREATE
    /// TABLE`, `ADD COLUMN x` or `TYPE`.
    Types,
    /// After `x.`: a table of schema `x`, or a column of table or alias `x`.
    Member(String),
    /// Nothing obvious, such as an alias position.
    Unknown,
    /// Inside a string or a comment.
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// Where the word being typed starts; completing replaces from here to
    /// the cursor.
    pub start: Pos,
    pub prefix: String,
    pub want: Want,
    pub tables: Vec<TableRef>,
    /// Keywords that fit here, best first.
    pub keywords: Vec<&'static str>,
    /// The function call the cursor is in, and which argument, from 0.
    pub call: Option<(String, usize)>,
    /// The quote the word being typed opened (`` ` `` in MySQL, `"`
    /// elsewhere), to keep it.
    pub quote: Option<char>,
    /// The column compared with `=`, `<>` or `IN (` before the cursor, and
    /// its qualifier: an enum's values come next.
    pub compared: Option<(Option<String>, String)>,
}

/// Keywords after which a table name comes.
const BEFORE_TABLE: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE", "DESCRIBE"];
/// Keywords after which a column (or an expression) comes.
const BEFORE_COLUMN: &[&str] = &[
    "SELECT",
    "WHERE",
    "AND",
    "OR",
    "ON",
    "BY",
    "SET",
    "HAVING",
    "WHEN",
    "THEN",
    "ELSE",
    "DISTINCT",
    "NOT",
    "CASE",
    "RETURNING",
];
/// Clauses, to tell what a comma separates.
const CLAUSES: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "ORDER",
    "SET",
    "VALUES",
    "HAVING",
    "RETURNING",
    "ON",
    "INTO",
];
/// Words that end a table reference instead of naming its alias.
const NOT_ALIAS: &[&str] = &[
    "WHERE",
    "JOIN",
    "ON",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "FULL",
    "NATURAL",
    "GROUP",
    "ORDER",
    "LIMIT",
    "SET",
    "USING",
    "VALUES",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "HAVING",
    "WINDOW",
    "OFFSET",
    "FETCH",
    "FOR",
    "RETURNING",
    "SELECT",
    "AS",
    "DEFAULT",
    "LATERAL",
];

pub fn context(text: &str, cursor: Pos, dialect: Dialect) -> Context {
    let all = lex(text, dialect);
    // A quoted name still being typed: complete it as if it were closed.
    let quote = if dialect == Dialect::MySql { '`' } else { '"' };
    let open = all
        .last()
        .filter(|l| l.token == Token::EOF && l.end == cursor);
    let word = open.and_then(|l| rest_of(text, l.start).trim_start().strip_prefix(quote));
    // Closed, the text holds the quote, so this happens once.
    if word.is_some_and(|w| !w.contains([quote, '\n'])) {
        let closed = format!("{text}{quote}");
        let mut ctx = context(&closed, (cursor.0, cursor.1 + 1), dialect);
        ctx.quote = Some(quote);
        return ctx;
    }
    // Inside a string or a comment there is nothing to complete.
    let inside = all.iter().any(|l| {
        // An unclosed string or comment, or a line comment, also contains
        // the position right at its end.
        let open = l.token == Token::EOF
            || matches!(
                l.token,
                Token::Whitespace(Whitespace::SingleLineComment { .. })
            );
        matches!(l.kind, Kind::String | Kind::Comment)
            && l.start < cursor
            && (cursor < l.end || (open && cursor <= l.end))
    });
    // The statement around the cursor, without whitespace and comments.
    let begin = all
        .iter()
        .rposition(|l| l.semicolon && l.end <= cursor)
        .map_or(0, |i| i + 1);
    let finish = all
        .iter()
        .position(|l| l.semicolon && l.start >= cursor)
        .unwrap_or(all.len());
    let tokens: Vec<&Lexeme> = all[begin..finish.max(begin)]
        .iter()
        .filter(|l| !l.trivia)
        .collect();

    let typing = tokens
        .iter()
        .position(|l| l.end == cursor && l.start < cursor && matches!(l.token, Token::Word(_)));
    let (start, prefix) = match typing {
        Some(i) => (
            tokens[i].start,
            tokens[i].name().unwrap_or_default().to_string(),
        ),
        None => (cursor, String::new()),
    };
    let tables = table_refs(&tokens, typing);
    if inside {
        return Context {
            start,
            prefix,
            want: Want::Nothing,
            tables,
            keywords: Vec::new(),
            call: None,
            quote: None,
            compared: None,
        };
    }

    let before: Vec<&&Lexeme> = tokens.iter().filter(|l| l.end <= start).collect();
    let (call, wants_type) = parens(&before);
    let want = match before.last().map(|l| &l.token) {
        _ if wants_type => Want::Types,
        Some(Token::Period) => match before.len().checked_sub(2).map(|i| before[i]) {
            Some(l) => l
                .name()
                .map_or(Want::Unknown, |n| Want::Member(n.to_string())),
            None => Want::Unknown,
        },
        Some(Token::Comma) => {
            let clause = before
                .iter()
                .rev()
                .filter_map(|l| l.keyword())
                .find(|w| CLAUSES.contains(&w.as_str()));
            match clause.as_deref() {
                Some("FROM") => Want::Tables,
                Some("VALUES" | "INTO") => Want::Unknown,
                Some(_) => Want::Columns,
                None => Want::Unknown,
            }
        }
        Some(
            Token::LParen
            | Token::Eq
            | Token::Neq
            | Token::Lt
            | Token::Gt
            | Token::LtEq
            | Token::GtEq
            | Token::Plus
            | Token::Minus,
        ) => Want::Columns,
        Some(Token::Word(_)) => {
            let word = before.last().and_then(|l| l.keyword()).unwrap_or_default();
            if BEFORE_TABLE.contains(&word.as_str()) {
                Want::Tables
            } else if BEFORE_COLUMN.contains(&word.as_str()) {
                Want::Columns
            } else {
                Want::Unknown
            }
        }
        None => Want::Unknown,
        Some(_) => Want::Unknown,
    };
    let keywords = keywords(&before, &want, dialect);
    Context {
        start,
        prefix,
        want,
        tables,
        keywords,
        call,
        quote: None,
        compared: compared(&before),
    }
}

/// The column before `=`, `<>`, `!=` or `IN (`, ending `before`.
fn compared(before: &[&&Lexeme]) -> Option<(Option<String>, String)> {
    let n = before.len();
    let operator = match &before.last()?.token {
        Token::Eq | Token::Neq => n - 1,
        Token::LParen if n >= 2 && before[n - 2].keyword().as_deref() == Some("IN") => n - 2,
        _ => return None,
    };
    let column = before.get(operator.checked_sub(1)?)?;
    if column
        .keyword()
        .is_some_and(|k| LEADS.contains(&k.as_str()))
    {
        return None;
    }
    let name = column.name()?.to_string();
    let qualifier = operator
        .checked_sub(3)
        .filter(|&i| before[i + 1].token == Token::Period)
        .and_then(|i| before[i].name())
        .map(str::to_string);
    Some((qualifier, name))
}

/// Reads the open parentheses before the cursor: the innermost function call
/// and its argument, and whether a column type comes next.
fn parens(before: &[&&Lexeme]) -> (Option<(String, usize)>, bool) {
    // Per open parenthesis: the word before it, and the commas seen.
    let mut open: Vec<(Option<String>, usize)> = Vec::new();
    for (i, l) in before.iter().enumerate() {
        match l.token {
            Token::LParen => {
                let word = i
                    .checked_sub(1)
                    .and_then(|p| before[p].name())
                    .map(str::to_string);
                open.push((word, 0));
            }
            Token::RParen => {
                open.pop();
            }
            Token::Comma => {
                if let Some((_, commas)) = open.last_mut() {
                    *commas += 1;
                }
            }
            _ => {}
        }
    }
    let call = open
        .iter()
        .rev()
        .find_map(|(word, commas)| Some((word.clone()?, *commas)))
        .filter(|(word, _)| !CLAUSE_KEYWORDS.contains(&word.to_uppercase().as_str()));
    let keyword_at = |back: usize| {
        before
            .len()
            .checked_sub(back)
            .and_then(|i| before[i].keyword())
            .unwrap_or_default()
    };
    let first = before.first().and_then(|l| l.keyword()).unwrap_or_default();
    let in_cast = open
        .last()
        .and_then(|(word, _)| word.as_deref())
        .is_some_and(|w| w.eq_ignore_ascii_case("CAST"));
    let after_name = matches!(before.last().map(|l| &l.token), Some(Token::Word(w))
        if w.quote_style.is_some() || !LEADS.contains(&w.value.to_uppercase().as_str()));
    let before_name = before.len().checked_sub(2).map(|i| &before[i].token);
    let wants_type = match before.last().map(|l| &l.token) {
        Some(Token::DoubleColon) => true,
        Some(Token::Word(_)) if keyword_at(1) == "AS" => in_cast,
        Some(Token::Word(_)) if keyword_at(1) == "TYPE" => true,
        _ if after_name && first == "CREATE" => {
            open.len() == 1 && matches!(before_name, Some(Token::LParen | Token::Comma))
        }
        _ if after_name && first == "ALTER" => matches!(keyword_at(2).as_str(), "ADD" | "COLUMN"),
        _ => false,
    };
    (call, wants_type)
}

/// Words that lead somewhere: after one of them comes a name or an
/// expression, not another clause.
const LEADS: &[&str] = &[
    "SELECT",
    "FROM",
    "JOIN",
    "INTO",
    "UPDATE",
    "TABLE",
    "WHERE",
    "AND",
    "OR",
    "ON",
    "BY",
    "SET",
    "HAVING",
    "WHEN",
    "THEN",
    "ELSE",
    "DISTINCT",
    "NOT",
    "CASE",
    "RETURNING",
    "AS",
    "IS",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "LIMIT",
    "OFFSET",
    "VALUES",
    "GROUP",
    "ORDER",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "FULL",
    "USING",
    "DELETE",
    "INSERT",
    "WITH",
    "UNION",
    "ALL",
    "EXISTS",
    "NULLS",
    "ADD",
    "COLUMN",
    "ALTER",
    "CREATE",
    "TYPE",
    "CAST",
    "DROP",
];
/// Keywords that start a clause, to tell which one the cursor is in.
const CLAUSE_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "JOIN",
    "ON",
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "LIMIT",
    "SET",
    "VALUES",
    "INTO",
    "UPDATE",
    "RETURNING",
    "OFFSET",
];

/// SQLite's pragmas people look up most.
const PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "foreign_key_list",
    "foreign_keys",
    "foreign_key_check",
    "database_list",
    "integrity_check",
    "quick_check",
    "journal_mode",
    "synchronous",
    "busy_timeout",
    "user_version",
    "schema_version",
    "page_size",
    "page_count",
    "cache_size",
    "auto_vacuum",
    "encoding",
    "temp_store",
    "mmap_size",
    "wal_checkpoint",
    "optimize",
    "compile_options",
    "function_list",
    "pragma_list",
];

/// The keywords that fit after `before`, the statement's tokens up to the
/// word being typed.
fn keywords(before: &[&&Lexeme], want: &Want, dialect: Dialect) -> Vec<&'static str> {
    // CQL has its own words (A1.4).
    if dialect == Dialect::Cql || matches!(want, Want::Member(_) | Want::Nothing) {
        return Vec::new();
    }
    let pg = dialect == Dialect::Postgres;
    let mysql = dialect == Dialect::MySql;
    let sqlite = dialect == Dialect::Sqlite;
    let only = |on: bool, words: &[&'static str]| if on { words.to_vec() } else { Vec::new() };
    let Some(last) = before.last() else {
        let mut words = vec![
            "SELECT",
            "INSERT INTO",
            "UPDATE",
            "DELETE FROM",
            "WITH",
            "CREATE TABLE",
            "ALTER TABLE",
            "DROP TABLE",
            "EXPLAIN",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
        ];
        words.extend(only(pg, &["EXPLAIN ANALYZE", "TRUNCATE"]));
        words.extend(only(mysql, &["SHOW TABLES", "DESCRIBE", "TRUNCATE"]));
        words.extend(only(sqlite, &["PRAGMA", "VACUUM"]));
        return words;
    };
    if sqlite && before.len() == 1 && last.keyword().as_deref() == Some("PRAGMA") {
        return PRAGMAS.to_vec();
    }
    let after_value = match &last.token {
        Token::Word(word) => {
            word.quote_style.is_some() || !LEADS.contains(&word.value.to_uppercase().as_str())
        }
        Token::Number(..) | Token::SingleQuotedString(_) | Token::RParen | Token::Mul => true,
        _ => false,
    };
    if !after_value {
        return match want {
            Want::Columns => {
                let mut words = Vec::new();
                if last.keyword().as_deref() == Some("SELECT") {
                    words.push("DISTINCT");
                }
                words.extend(["CASE", "NOT", "NULL", "EXISTS"]);
                words
            }
            _ => Vec::new(),
        };
    }
    let first = before.first().and_then(|l| l.keyword()).unwrap_or_default();
    let changes = matches!(first.as_str(), "UPDATE" | "DELETE" | "INSERT");
    let clause = before
        .iter()
        .rev()
        .filter_map(|l| l.keyword())
        .find(|w| CLAUSE_KEYWORDS.contains(&w.as_str()))
        .unwrap_or_default();
    let tail = ["GROUP BY", "ORDER BY", "LIMIT"];
    let mut words: Vec<&'static str> = match clause.as_str() {
        "SELECT" => vec!["FROM", "AS"],
        "FROM" | "JOIN" if first == "DELETE" => vec!["WHERE"],
        "FROM" | "JOIN" => {
            let mut words = vec!["WHERE"];
            if clause == "JOIN" {
                words.extend(["ON", "USING"]);
            }
            words.extend([
                "JOIN",
                "LEFT JOIN",
                "INNER JOIN",
                "RIGHT JOIN",
                "CROSS JOIN",
            ]);
            words.extend(only(!mysql, &["FULL JOIN"]));
            words.extend(tail);
            words.extend(["AS", "UNION"]);
            words
        }
        "ON" | "WHERE" | "HAVING" => {
            let mut words = vec!["AND", "OR", "NOT", "IN", "LIKE"];
            words.extend(only(pg, &["ILIKE"]));
            words.extend(["BETWEEN", "IS NULL", "IS NOT NULL"]);
            if clause == "ON" {
                words.extend(["WHERE", "JOIN", "LEFT JOIN"]);
            }
            if !changes {
                words.extend(tail);
            }
            words
        }
        "GROUP" => vec!["HAVING", "ORDER BY", "LIMIT"],
        "ORDER" => {
            let mut words = vec!["ASC", "DESC"];
            words.extend(only(pg || sqlite, &["NULLS FIRST", "NULLS LAST"]));
            words.push("LIMIT");
            words
        }
        "LIMIT" => vec!["OFFSET"],
        "UPDATE" => vec!["SET"],
        "SET" => vec!["WHERE"],
        "INTO" => {
            let mut words = vec!["VALUES", "SELECT"];
            words.extend(only(pg || sqlite, &["DEFAULT VALUES"]));
            words
        }
        "VALUES" => {
            let mut words = only(pg || sqlite, &["ON CONFLICT"]);
            words.extend(only(mysql, &["ON DUPLICATE KEY UPDATE"]));
            words
        }
        _ => Vec::new(),
    };
    if changes && (pg || sqlite) && !matches!(clause.as_str(), "RETURNING" | "UPDATE" | "INTO" | "")
    {
        words.push("RETURNING");
    }
    words
}

/// The tables named after `FROM`, `JOIN`, `UPDATE` and `INTO`, with their
/// aliases. The word being typed (`skip`) is not a table yet.
fn table_refs(tokens: &[&Lexeme], skip: Option<usize>) -> Vec<TableRef> {
    // A name, not the word being typed nor a clause keyword.
    let word_at = |i: usize| -> Option<&str> {
        if Some(i) == skip {
            return None;
        }
        let token = tokens.get(i)?;
        if token
            .keyword()
            .is_some_and(|k| NOT_ALIAS.contains(&k.as_str()))
        {
            return None;
        }
        token.name()
    };
    let mut refs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let keyword = tokens[i].keyword().unwrap_or_default();
        let lists = keyword == "FROM";
        if !matches!(keyword.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            i += 1;
            continue;
        }
        i += 1;
        while let Some(first) = word_at(i) {
            let mut table = TableRef {
                schema: None,
                name: first.to_string(),
                alias: None,
            };
            i += 1;
            if tokens.get(i).is_some_and(|l| l.token == Token::Period) {
                match word_at(i + 1) {
                    Some(name) => {
                        table.schema = Some(table.name.clone());
                        table.name = name.to_string();
                        i += 2;
                    }
                    None => break,
                }
            }
            if tokens.get(i).and_then(|l| l.keyword()).as_deref() == Some("AS") {
                i += 1;
            }
            if let Some(alias) = word_at(i) {
                table.alias = Some(alias.to_string());
                i += 1;
            }
            refs.push(table);
            if lists && tokens.get(i).is_some_and(|l| l.token == Token::Comma) {
                i += 1;
                continue;
            }
            break;
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context at `|` in `text`.
    fn at(text: &str) -> Context {
        let (line, col) = text
            .split('\n')
            .enumerate()
            .find_map(|(i, l)| l.find('|').map(|c| (i, l[..c].chars().count())))
            .expect("a cursor");
        context(&text.replace('|', ""), (line, col), Dialect::Postgres)
    }

    fn table(schema: Option<&str>, name: &str, alias: Option<&str>) -> TableRef {
        TableRef {
            schema: schema.map(str::to_string),
            name: name.to_string(),
            alias: alias.map(str::to_string),
        }
    }

    #[test]
    fn tables_come_after_from_and_join() {
        let c = at("select * from cus|");
        assert_eq!(
            (c.want, c.prefix.as_str(), c.start),
            (Want::Tables, "cus", (0, 14))
        );
        assert_eq!(at("select * from |").want, Want::Tables);
        assert_eq!(at("select * from a join |").want, Want::Tables);
        assert_eq!(at("select * from a, |").want, Want::Tables);
        assert_eq!(at("insert into |").want, Want::Tables);
        assert_eq!(at("UPDATE o|").want, Want::Tables);
    }

    #[test]
    fn columns_come_after_select_where_and_operators() {
        for text in [
            "select |",
            "select id, |",
            "select * from t where |",
            "select * from t where a = 1 and na|",
            "select * from t where a = |",
            "select * from t order by |",
            "update t set |",
            "select count(|",
        ] {
            assert_eq!(at(text).want, Want::Columns, "{text}");
        }
    }

    #[test]
    fn a_dot_asks_for_members() {
        let c = at("select o.| from orders o");
        assert_eq!((c.want, c.prefix.as_str()), (Want::Member("o".into()), ""));
        assert_eq!(
            at("select * from public.ord|").want,
            Want::Member("public".into())
        );
        assert_eq!(at("select \"Weird\".|").want, Want::Member("Weird".into()));
    }

    #[test]
    fn strings_comments_and_aliases_complete_nothing() {
        assert_eq!(at("select 'fro|").want, Want::Nothing);
        assert_eq!(at("select 'a|b'").want, Want::Nothing);
        assert_eq!(at("select 1 -- from |").want, Want::Nothing);
        assert_eq!(at("select /* from | */ 1").want, Want::Nothing);
        assert_eq!(at("select * from orders |").want, Want::Unknown);
        assert_eq!(at("|").want, Want::Unknown);
        assert_eq!(at("insert into t values (1, |").want, Want::Unknown);
    }

    #[test]
    fn the_statement_names_its_tables_and_aliases() {
        let c = at("select | from shop.orders o join customers as c on c.id = o.cid");
        assert_eq!(
            c.tables,
            [
                table(Some("shop"), "orders", Some("o")),
                table(None, "customers", Some("c")),
            ]
        );
        assert_eq!(
            at("select | from a, b x where true").tables,
            [table(None, "a", None), table(None, "b", Some("x"))]
        );
        // Only the statement under the cursor counts; the word being typed
        // isn't a table yet.
        let c = at("select * from a; select * from b where |; select * from c");
        assert_eq!(c.tables, [table(None, "b", None)]);
        assert_eq!(at("select * from ord|").tables, []);
        assert_eq!(at("update t x set |").tables, [table(None, "t", Some("x"))]);
        assert_eq!(at("select * from (select 1) s where |").tables, []);
        assert_eq!(at("select * from a. where |").tables, []);
    }

    fn words(text: &str, dialect: Dialect) -> Vec<&'static str> {
        let (line, col) = text
            .lines()
            .enumerate()
            .find_map(|(i, l)| l.find('|').map(|c| (i, l[..c].chars().count())))
            .expect("a cursor");
        context(&text.replace('|', ""), (line, col), dialect).keywords
    }

    #[test]
    fn keywords_fit_the_clause() {
        let pg = |text| words(text, Dialect::Postgres);
        assert!(pg("|").contains(&"SELECT"));
        assert!(pg("select id |").starts_with(&["FROM", "AS"]));
        assert!(pg("select * from orders o |").contains(&"LEFT JOIN"));
        assert!(pg("select * from a join b |").starts_with(&["WHERE", "ON", "USING"]));
        assert!(pg("select * from t where a = 1 |").contains(&"AND"));
        assert!(pg("select * from t where a = 1 |").contains(&"ORDER BY"));
        assert_eq!(
            pg("select * from t group by a |"),
            ["HAVING", "ORDER BY", "LIMIT"]
        );
        assert!(pg("select * from t order by a |").starts_with(&["ASC", "DESC"]));
        assert_eq!(pg("select * from t limit 10 |"), ["OFFSET"]);
        assert_eq!(pg("update t |"), ["SET"]);
        assert_eq!(pg("update t set a = 1 |"), ["WHERE", "RETURNING"]);
        assert_eq!(pg("delete from t |"), ["WHERE", "RETURNING"]);
        assert!(pg("insert into t (a) |").starts_with(&["VALUES", "SELECT"]));
        // Where a name or an expression comes, only expression words.
        assert!(pg("select * from |").is_empty());
        assert_eq!(
            pg("select |"),
            ["DISTINCT", "CASE", "NOT", "NULL", "EXISTS"]
        );
        assert!(pg("select 'a|").is_empty());
        assert!(pg("select o.|").is_empty());
    }

    #[test]
    fn keywords_follow_the_dialect() {
        let where_ = "select * from t where a |";
        assert!(words(where_, Dialect::Postgres).contains(&"ILIKE"));
        assert!(!words(where_, Dialect::MySql).contains(&"ILIKE"));
        let values = "insert into t values (1) |";
        assert_eq!(
            words(values, Dialect::Postgres),
            ["ON CONFLICT", "RETURNING"]
        );
        assert_eq!(words(values, Dialect::MySql), ["ON DUPLICATE KEY UPDATE"]);
        assert!(!words("select * from a |", Dialect::MySql).contains(&"FULL JOIN"));
        assert!(words("|", Dialect::Sqlite).contains(&"PRAGMA"));
        assert!(words("select * from t |", Dialect::Cql).is_empty());
    }

    #[test]
    fn types_come_after_casts_and_in_column_definitions() {
        for text in [
            "select x::|",
            "select cast(x as |",
            "create table t (id |",
            "create table t (id int, name |",
            "alter table t add column name |",
            "alter table t alter column name type |",
        ] {
            assert_eq!(at(text).want, Want::Types, "{text}");
        }
        assert_ne!(at("create table t (|").want, Want::Types);
        assert_ne!(at("alter table t add column |").want, Want::Types);
        assert_ne!(at("select x as |").want, Want::Types);
    }

    #[test]
    fn the_call_and_argument_at_the_cursor() {
        assert_eq!(
            at("select date_trunc(|").call,
            Some(("date_trunc".into(), 0))
        );
        assert_eq!(
            at("select date_trunc('day', created|").call,
            Some(("date_trunc".into(), 1))
        );
        assert_eq!(
            at("select coalesce(lower(a), |").call,
            Some(("coalesce".into(), 1))
        );
        assert_eq!(at("select lower(a) |").call, None);
        assert_eq!(at("insert into t values (|").call, None);
    }

    fn in_dialect(text: &str, dialect: Dialect) -> Context {
        context(text, (0, text.chars().count()), dialect)
    }

    #[test]
    fn an_open_quote_still_completes_and_is_kept() {
        let c = in_dialect("select * from `ord", Dialect::MySql);
        assert_eq!(
            (c.want, c.prefix.as_str(), c.start, c.quote),
            (Want::Tables, "ord", (0, 14), Some('`'))
        );
        let c = in_dialect("select \"Na", Dialect::Postgres);
        assert_eq!((c.want, c.quote), (Want::Columns, Some('"')));
        // Strings stay strings.
        assert_eq!(
            in_dialect("select 'ord", Dialect::MySql).want,
            Want::Nothing
        );
        assert_eq!(
            in_dialect("select * from \"ord", Dialect::MySql).want,
            Want::Nothing
        );
    }

    #[test]
    fn the_column_compared_before_the_cursor() {
        assert_eq!(
            at("select * from t where status = |").compared,
            Some((None, "status".into()))
        );
        assert_eq!(
            at("select * from t o where o.status <> pe|").compared,
            Some((Some("o".into()), "status".into()))
        );
        assert_eq!(
            at("select * from t where kind in (|").compared,
            Some((None, "kind".into()))
        );
        assert_eq!(at("select * from t where |").compared, None);
        assert_eq!(at("update t set a = 1 where b = (|").compared, None);
    }

    #[test]
    fn pragmas_follow_pragma_in_sqlite() {
        assert!(in_dialect("pragma ", Dialect::Sqlite)
            .keywords
            .contains(&"table_info"));
        assert!(!in_dialect("pragma ", Dialect::Postgres)
            .keywords
            .contains(&"table_info"));
    }
}
