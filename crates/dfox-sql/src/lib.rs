//! Reading SQL without running it: highlighting, splitting a script into
//! statements, the statement under the cursor, autocomplete context, the
//! destructive-statement guard, and the table a query reads from. Built on
//! the `sqlparser` tokenizer, in the dialect of the connected database.

pub mod complete;
pub mod guard;

use sqlparser::{
    ast::{SetExpr, Statement, TableFactor},
    dialect::{Dialect as SqlParserDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect},
    parser::Parser,
    tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace},
};

/// A position in the text: line and column, both from 0, columns in
/// characters.
pub type Pos = (usize, usize);

/// How a piece of SQL is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keyword,
    Function,
    String,
    Number,
    Comment,
    Operator,
    Plain,
}

/// A coloured piece of one line: columns `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

/// A statement of a script: from its first token to the end of its last one,
/// and up to where the next statement's text starts (after the `;`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stmt {
    pub start: Pos,
    pub end: Pos,
    pub zone_end: Pos,
}

/// The flavour of SQL to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    Postgres,
    MySql,
    Sqlite,
}

/// The `sqlparser` dialect for `dialect`.
fn parser_dialect(dialect: Dialect) -> Box<dyn SqlParserDialect> {
    match dialect {
        Dialect::Postgres => Box::new(PostgreSqlDialect {}),
        Dialect::MySql => Box::new(MySqlDialect {}),
        Dialect::Sqlite => Box::new(SQLiteDialect {}),
    }
}

/// A token with its kind and where it is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lexeme {
    pub start: Pos,
    pub end: Pos,
    pub kind: Kind,
    /// Whitespace and comments.
    pub trivia: bool,
    pub semicolon: bool,
    /// The token; `EOF` for text the tokenizer gave up on.
    pub token: Token,
}

impl Lexeme {
    /// The upper-cased word, when the token is an unquoted word.
    pub fn keyword(&self) -> Option<String> {
        match &self.token {
            Token::Word(word) if word.quote_style.is_none() => Some(word.value.to_uppercase()),
            _ => None,
        }
    }

    /// The name a word stands for, quoted or not.
    pub fn name(&self) -> Option<&str> {
        match &self.token {
            Token::Word(word) => Some(&word.value),
            _ => None,
        }
    }
}

/// Tokenizes `text`. Text the tokenizer gives up on, such as a string that
/// isn't closed yet, runs to the end as one piece.
pub(crate) fn lex(text: &str, dialect: Dialect) -> Vec<Lexeme> {
    let dialect = parser_dialect(dialect);
    let mut tokens: Vec<TokenWithSpan> = Vec::new();
    let failed = Tokenizer::new(dialect.as_ref(), text)
        .tokenize_with_location_into_buf(&mut tokens)
        .is_err();

    let pos = |line: u64, column: u64| {
        (
            (line as usize).saturating_sub(1),
            (column as usize).saturating_sub(1),
        )
    };
    let mut lexemes: Vec<Lexeme> = Vec::with_capacity(tokens.len());
    for (i, t) in tokens.iter().enumerate() {
        let (kind, trivia) = match &t.token {
            Token::Whitespace(Whitespace::SingleLineComment { .. })
            | Token::Whitespace(Whitespace::MultiLineComment(_)) => (Kind::Comment, true),
            Token::Whitespace(_) => (Kind::Plain, true),
            Token::Word(word) if word.quote_style.is_some() => (Kind::Plain, false),
            Token::Word(word) if is_keyword(&word.value) => (Kind::Keyword, false),
            Token::Word(_) => {
                let call = tokens[i + 1..]
                    .iter()
                    .find(|t| !matches!(t.token, Token::Whitespace(_)))
                    .is_some_and(|t| t.token == Token::LParen);
                (if call { Kind::Function } else { Kind::Plain }, false)
            }
            Token::Number(..) | Token::Placeholder(_) => (Kind::Number, false),
            Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::DollarQuotedString(_)
            | Token::EscapedStringLiteral(_)
            | Token::NationalStringLiteral(_)
            | Token::UnicodeStringLiteral(_)
            | Token::HexStringLiteral(_)
            | Token::SingleQuotedByteStringLiteral(_)
            | Token::DoubleQuotedByteStringLiteral(_)
            | Token::TripleSingleQuotedString(_)
            | Token::TripleDoubleQuotedString(_) => (Kind::String, false),
            Token::Comma
            | Token::LParen
            | Token::RParen
            | Token::Period
            | Token::SemiColon
            | Token::LBracket
            | Token::RBracket => (Kind::Plain, false),
            Token::EOF => continue,
            _ => (Kind::Operator, false),
        };
        lexemes.push(Lexeme {
            start: pos(t.span.start.line, t.span.start.column),
            end: pos(t.span.end.line, t.span.end.column),
            kind,
            trivia,
            semicolon: t.token == Token::SemiColon,
            token: t.token.clone(),
        });
    }

    if failed {
        let start = lexemes.last().map_or((0, 0), |l| l.end);
        let rest = rest_of(text, start);
        let trimmed = rest.trim_start();
        let kind = if trimmed.starts_with("/*") || trimmed.starts_with("--") {
            Kind::Comment
        } else if trimmed.starts_with(['\'', '"', '`', '$']) {
            Kind::String
        } else {
            Kind::Plain
        };
        lexemes.push(Lexeme {
            start,
            end: end_of(text),
            kind,
            trivia: kind == Kind::Comment,
            semicolon: false,
            token: Token::EOF,
        });
    }
    lexemes
}

/// The text from `start` to the end.
fn rest_of(text: &str, (line, col): Pos) -> &str {
    let mut offset = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i == line {
            let byte = l.char_indices().nth(col).map_or(l.len(), |(b, _)| b);
            return &text[offset + byte..];
        }
        offset += l.len() + 1;
    }
    ""
}

/// The position just after the last character.
fn end_of(text: &str) -> Pos {
    let line = text.split('\n').count() - 1;
    let col = text.rsplit('\n').next().map_or(0, |l| l.chars().count());
    (line, col)
}

/// The coloured pieces of each line of `lines`, in order. Uncoloured text
/// isn't listed.
pub fn highlight(lines: &[String], dialect: Dialect) -> Vec<Vec<Piece>> {
    let text = lines.join("\n");
    let mut out = vec![Vec::new(); lines.len()];
    for lexeme in lex(&text, dialect) {
        if lexeme.kind == Kind::Plain {
            continue;
        }
        // A token can span lines: a block comment or a multi-line string.
        for line in lexeme.start.0..=lexeme.end.0.min(lines.len().saturating_sub(1)) {
            let start = if line == lexeme.start.0 {
                lexeme.start.1
            } else {
                0
            };
            let end = if line == lexeme.end.0 {
                lexeme.end.1
            } else {
                lines[line].chars().count()
            };
            if end > start {
                out[line].push(Piece {
                    start,
                    end,
                    kind: lexeme.kind,
                });
            }
        }
    }
    out
}

/// The statements of a script, skipping empty ones.
pub fn statements(text: &str, dialect: Dialect) -> Vec<Stmt> {
    let mut found = Vec::new();
    let mut current: Option<(Pos, Pos)> = None;
    for lexeme in lex(text, dialect) {
        if lexeme.semicolon {
            if let Some((start, _)) = current.take() {
                found.push(Stmt {
                    start,
                    end: lexeme.end,
                    zone_end: lexeme.end,
                });
            } else if let Some(last) = found.last_mut() {
                // `;;`: the empty statement belongs to the one before.
                last.zone_end = lexeme.end;
            }
        } else if !lexeme.trivia {
            let start = current.map_or(lexeme.start, |(start, _)| start);
            current = Some((start, lexeme.end));
        }
    }
    if let Some((start, end)) = current {
        found.push(Stmt {
            start,
            end,
            zone_end: end_of(text),
        });
    }
    found
}

/// The statement the cursor is in or just after: the one whose text, up to
/// and including its `;`, reaches the cursor. In trailing blank space, the
/// last statement.
pub fn statement_at(text: &str, cursor: Pos, dialect: Dialect) -> Option<Stmt> {
    let all = statements(text, dialect);
    all.iter()
        .find(|s| s.zone_end >= cursor)
        .or(all.last())
        .copied()
}

/// The text between two positions.
pub fn slice(lines: &[String], start: Pos, end: Pos) -> String {
    let mut out = String::new();
    let last = end.0.min(lines.len().saturating_sub(1));
    for (line, text) in lines.iter().enumerate().take(last + 1).skip(start.0) {
        let chars = text.chars();
        let from = if line == start.0 { start.1 } else { 0 };
        if line == end.0 {
            out.extend(chars.skip(from).take(end.1.saturating_sub(from)));
        } else {
            out.extend(chars.skip(from));
            out.push('\n');
        }
    }
    out
}

/// A name as SQL: as it is when it is a plain lower-case name that isn't a
/// reserved word, otherwise quoted for the dialect.
pub fn quote_ident(name: &str, dialect: Dialect) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && RESERVED.binary_search(&name).is_err();
    if plain {
        return name.to_string();
    }
    let quote = if dialect == Dialect::MySql { '`' } else { '"' };
    let doubled = format!("{quote}{quote}");
    format!("{quote}{}{quote}", name.replace(quote, &doubled))
}

/// Words that can't be a bare name in Postgres, MySQL or SQLite.
const RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "between",
    "binary",
    "both",
    "by",
    "case",
    "cast",
    "change",
    "check",
    "collate",
    "collation",
    "column",
    "condition",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "database",
    "databases",
    "default",
    "deferrable",
    "delete",
    "desc",
    "describe",
    "distinct",
    "div",
    "do",
    "drop",
    "else",
    "end",
    "except",
    "exists",
    "explain",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "groups",
    "having",
    "ilike",
    "in",
    "index",
    "initially",
    "inner",
    "insert",
    "intersect",
    "interval",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "keys",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "load",
    "localtime",
    "localtimestamp",
    "lock",
    "match",
    "mod",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "option",
    "or",
    "order",
    "outer",
    "over",
    "overlaps",
    "placing",
    "primary",
    "range",
    "rank",
    "read",
    "references",
    "release",
    "rename",
    "replace",
    "return",
    "returning",
    "revoke",
    "right",
    "row",
    "rows",
    "schema",
    "select",
    "session_user",
    "set",
    "show",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "update",
    "usage",
    "user",
    "using",
    "values",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
    "write",
];

/// The table a query reads, when it reads exactly one: `SELECT … FROM t`.
pub fn single_table(sql: &str, dialect: Dialect) -> Option<String> {
    let dialect = parser_dialect(dialect);
    let statements = Parser::parse_sql(dialect.as_ref(), sql).ok()?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let [from] = select.from.as_slice() else {
        return None;
    };
    if !from.joins.is_empty() {
        return None;
    }
    match &from.relation {
        TableFactor::Table { name, .. } => Some(name.to_string()),
        _ => None,
    }
}

/// Words coloured as keywords. Many more words are keywords to the parser
/// (`name`, `user`, …), but colouring those would paint column names.
const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "AUTO_INCREMENT",
    "BEGIN",
    "BETWEEN",
    "BIGINT",
    "BOOLEAN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHAR",
    "CHECK",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "DATE",
    "DECIMAL",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DO",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FILTER",
    "FIRST",
    "FLOAT",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INT",
    "INTEGER",
    "INTERSECT",
    "INTERVAL",
    "INTO",
    "IS",
    "JOIN",
    "JSON",
    "JSONB",
    "KEY",
    "LATERAL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "MATERIALIZED",
    "NATURAL",
    "NOT",
    "NULL",
    "NULLS",
    "NUMERIC",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "PROCEDURE",
    "REAL",
    "RECURSIVE",
    "REFERENCES",
    "REPLACE",
    "RETURNING",
    "RETURNS",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SCHEMA",
    "SELECT",
    "SERIAL",
    "SET",
    "SHOW",
    "SMALLINT",
    "TABLE",
    "TEMP",
    "TEMPORARY",
    "TEXT",
    "THEN",
    "TIMESTAMP",
    "TIMESTAMPTZ",
    "TO",
    "TRANSACTION",
    "TRIGGER",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "UUID",
    "VACUUM",
    "VALUES",
    "VARCHAR",
    "VIEW",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
];

fn is_keyword(word: &str) -> bool {
    word.len() <= 17
        && KEYWORDS
            .binary_search(&word.to_ascii_uppercase().as_str())
            .is_ok()
}

#[cfg(test)]
mod tests;
