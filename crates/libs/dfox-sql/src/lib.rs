//! Reading SQL without running it: highlighting, splitting a script into
//! statements, the statement under the cursor, autocomplete context, the
//! destructive-statement guard, and the table a query reads from. Built on
//! the `sqlparser` tokenizer, in the dialect of the connected database.

pub mod complete;
pub mod guard;
pub mod mongo;
pub mod redis;

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
    /// Cassandra's CQL: Postgres's quotes and comments, its own words.
    Cql,
}

/// The `sqlparser` dialect for `dialect`.
fn parser_dialect(dialect: Dialect) -> Box<dyn SqlParserDialect> {
    match dialect {
        Dialect::Postgres | Dialect::Cql => Box::new(PostgreSqlDialect {}),
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
pub(crate) fn lex(text: &str, language: Dialect) -> Vec<Lexeme> {
    let dialect = parser_dialect(language);
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
            Token::Word(word) if is_keyword(&word.value, language) => (Kind::Keyword, false),
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
pub(crate) fn end_of(text: &str) -> Pos {
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

/// The statements of a script, skipping empty ones. A statement ends at a
/// `;` outside strings, comments and the `BEGIN … END` body of a procedure,
/// function, trigger or event. On MySQL, `DELIMITER $$` lines change what
/// ends a statement, as in the mysql client; they aren't statements, and a
/// statement's text leaves its delimiter out.
pub fn statements(text: &str, dialect: Dialect) -> Vec<Stmt> {
    let lexemes = lex(text, dialect);
    let directives = if dialect == Dialect::MySql {
        delimiter_lines(&lexemes, text)
    } else {
        Vec::new()
    };
    if directives.is_empty() {
        return split_on_semicolons(&lexemes, end_of(text));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut found = Vec::new();
    let mut delimiter = ";".to_string();
    let mut first = 0;
    // Each run of lines between directives, with the delimiter in force.
    for (line, next) in directives
        .iter()
        .map(|(line, d)| (*line, Some(d)))
        .chain([(lines.len(), None)])
    {
        if line > first {
            let last = line - 1;
            let segment_end = (last, lines[last].chars().count());
            if delimiter == ";" {
                let inside: Vec<Lexeme> = lexemes
                    .iter()
                    .filter(|l| l.start.0 >= first && l.start.0 <= last)
                    .cloned()
                    .collect();
                found.extend(split_on_semicolons(&inside, segment_end));
            } else {
                found.extend(split_on(&lines, first..=last, &delimiter, &lexemes));
            }
        }
        if let Some(next) = next {
            delimiter = next.clone();
        }
        first = line + 1;
    }
    found
}

/// The lines that are `DELIMITER x` directives, and the delimiter each sets.
fn delimiter_lines(lexemes: &[Lexeme], text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut seen_line = None;
    for lexeme in lexemes {
        if lexeme.trivia || seen_line == Some(lexeme.start.0) {
            continue;
        }
        // Only the first token of a line can start a directive.
        seen_line = Some(lexeme.start.0);
        if lexeme.keyword().as_deref() != Some("DELIMITER") {
            continue;
        }
        let line = lines[lexeme.start.0];
        let rest: String = line.chars().skip(lexeme.end.1).collect();
        if let Some(delimiter) = rest.split_whitespace().next() {
            out.push((lexeme.start.0, delimiter.to_string()));
        }
    }
    out
}

/// Splits on `;` the statements made of `lexemes`, keeping procedure
/// bodies whole; the last statement's zone runs to `end`.
fn split_on_semicolons(lexemes: &[Lexeme], end: Pos) -> Vec<Stmt> {
    let mut found = Vec::new();
    let mut current: Option<(Pos, Pos)> = None;
    let mut body = Body::default();
    for lexeme in lexemes {
        if lexeme.semicolon && body.depth == 0 {
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
            body = Body::default();
        } else if !lexeme.trivia {
            body.step(current.is_none(), lexeme);
            let start = current.map_or(lexeme.start, |(start, _)| start);
            current = Some((start, lexeme.end));
        }
    }
    if let Some((start, last)) = current {
        found.push(Stmt {
            start,
            end: last,
            zone_end: end,
        });
    }
    found
}

/// How deep in a routine's `BEGIN … END` a statement is: its `;` don't end
/// it there.
#[derive(Debug, Default)]
struct Body {
    /// The statement starts with CREATE.
    create: bool,
    /// It creates a procedure, function, trigger or event, so its BEGIN
    /// starts a body rather than a transaction.
    routine: bool,
    depth: usize,
    /// The word before was END, which `END IF` and the like don't close.
    after_end: bool,
    /// The statement starts with BEGIN: a transaction, or a CQL batch.
    begin: bool,
    /// The word before was APPLY, which `APPLY BATCH` ends a batch with.
    after_apply: bool,
}

impl Body {
    fn step(&mut self, first: bool, lexeme: &Lexeme) {
        let word = lexeme.keyword();
        let word = word.as_deref().unwrap_or("");
        if first {
            self.create = word == "CREATE";
            self.begin = word == "BEGIN";
        }
        // A CQL batch, `BEGIN [UNLOGGED | COUNTER] BATCH … APPLY BATCH`,
        // holds statements of its own.
        if std::mem::take(&mut self.after_apply) && word == "BATCH" {
            self.depth = self.depth.saturating_sub(1);
            return;
        }
        if self.begin && word == "BATCH" && self.depth == 0 {
            self.depth = 1;
            return;
        }
        if self.begin && word == "APPLY" {
            self.after_apply = true;
            return;
        }
        if std::mem::take(&mut self.after_end) {
            match word {
                // These blocks opened without being counted.
                "IF" | "LOOP" | "WHILE" | "REPEAT" => self.depth += 1,
                // `END CASE` closes the CASE already counted.
                "CASE" => return,
                _ => {}
            }
        }
        match word {
            "PROCEDURE" | "FUNCTION" | "TRIGGER" | "EVENT" if self.create => self.routine = true,
            "BEGIN" | "CASE" if self.routine => self.depth += 1,
            "END" if self.routine => {
                self.depth = self.depth.saturating_sub(1);
                self.after_end = true;
            }
            _ => {}
        }
    }
}

/// Splits `rows` of `lines` on `delimiter` outside strings and comments,
/// leaving the delimiter out of each statement.
fn split_on(
    lines: &[&str],
    rows: std::ops::RangeInclusive<usize>,
    delimiter: &str,
    lexemes: &[Lexeme],
) -> Vec<Stmt> {
    // Where strings and comments are: no delimiter ends a statement there.
    let shielded: Vec<&Lexeme> = lexemes
        .iter()
        .filter(|l| matches!(l.kind, Kind::String | Kind::Comment))
        .collect();
    let within = |at: Pos| shielded.iter().find(|l| l.start <= at && at < l.end);
    let width = delimiter.chars().count();
    let mut found = Vec::new();
    let mut current: Option<(Pos, Pos)> = None;
    let last_row = *rows.end();
    for row in rows {
        let chars: Vec<char> = lines[row].chars().collect();
        let mut col = 0;
        while col < chars.len() {
            let at = (row, col);
            let shield = within(at);
            let rest: String = chars[col..].iter().take(width).collect();
            if shield.is_none() && rest == delimiter {
                if let Some((start, end)) = current.take() {
                    found.push(Stmt {
                        start,
                        end,
                        zone_end: (row, col + width),
                    });
                }
                col += width;
                continue;
            }
            let comment = shield.is_some_and(|l| l.kind == Kind::Comment);
            if !comment && !chars[col].is_whitespace() {
                let start = current.map_or(at, |(start, _)| start);
                current = Some((start, (row, col + 1)));
            }
            col += 1;
        }
    }
    if let Some((start, end)) = current {
        found.push(Stmt {
            start,
            end,
            zone_end: (last_row, lines[last_row].chars().count()),
        });
    }
    found
}

/// A MySQL script without its `DELIMITER` lines, each statement ending in
/// `;` as the server reads it; `None` when it has none to take out.
pub fn without_delimiters(text: &str, dialect: Dialect) -> Option<String> {
    if dialect != Dialect::MySql || delimiter_lines(&lex(text, dialect), text).is_empty() {
        return None;
    }
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let statements: Vec<String> = statements(text, dialect)
        .iter()
        .map(|s| {
            let text = slice(&lines, s.start, s.end);
            text.trim_end().trim_end_matches(';').to_string()
        })
        .collect();
    Some(format!("{};", statements.join(";\n")))
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

/// Words CQL adds, coloured only there.
const CQL_KEYWORDS: &[&str] = &[
    "ALLOW",
    "APPLY",
    "ASCII",
    "BATCH",
    "BLOB",
    "CLUSTERING",
    "COMPACT",
    "CONTAINS",
    "COUNTER",
    "DURATION",
    "ENTRIES",
    "EXISTS",
    "FILTERING",
    "FROZEN",
    "INET",
    "KEYS",
    "KEYSPACE",
    "KEYSPACES",
    "LIST",
    "LOGGED",
    "MAP",
    "PER",
    "SET",
    "STATIC",
    "TIMEUUID",
    "TOKEN",
    "TTL",
    "TUPLE",
    "TYPE",
    "UNLOGGED",
    "USE",
    "VARINT",
    "WRITETIME",
];

fn is_keyword(word: &str, dialect: Dialect) -> bool {
    if word.len() > 17 {
        return false;
    }
    let upper = word.to_ascii_uppercase();
    KEYWORDS.binary_search(&upper.as_str()).is_ok()
        || (dialect == Dialect::Cql && CQL_KEYWORDS.binary_search(&upper.as_str()).is_ok())
}

#[cfg(test)]
mod tests;
