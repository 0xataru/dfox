//! What the cursor wants completed, read from the tokens around it: a table
//! after `FROM`, a column after `SELECT` or `WHERE`, a member after `x.`, and
//! which tables (and aliases) the statement reads.

use sqlparser::tokenizer::{Token, Whitespace};

use super::{lex, Dialect, Kind, Lexeme, Pos};

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
        };
    }

    let before: Vec<&&Lexeme> = tokens.iter().filter(|l| l.end <= start).collect();
    let want = match before.last().map(|l| &l.token) {
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
    Context {
        start,
        prefix,
        want,
        tables,
    }
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
}
