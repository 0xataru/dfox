//! Reading mongo shell commands without running them: highlighting,
//! splitting into commands, the command under the cursor, what to complete,
//! and the commands worth a second look.
//!
//! A light scanner rather than a parser: it knows strings, numbers,
//! comments, regular expressions and brackets, which is all these need.

use crate::{guard::Findings, slice, Kind, Piece, Pos, Stmt};

/// One token of shell text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    start: Pos,
    end: Pos,
    kind: Tok,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Word,
    String,
    Number,
    Comment,
    Regex,
    /// A bracket or other punctuation, one character.
    Punct,
    Newline,
}

/// Words coloured as keywords: the shell's own, and its value helpers.
const KEYWORDS: &[&str] = &[
    "BinData",
    "Date",
    "ISODate",
    "MaxKey",
    "MinKey",
    "NumberDecimal",
    "NumberInt",
    "NumberLong",
    "ObjectId",
    "RegExp",
    "Timestamp",
    "UUID",
    "db",
    "false",
    "new",
    "null",
    "show",
    "true",
    "undefined",
    "use",
];

/// Collection methods that change data or schema.
const WRITES: &[&str] = &[
    "insertOne",
    "insertMany",
    "insert",
    "updateOne",
    "updateMany",
    "update",
    "replaceOne",
    "deleteOne",
    "deleteMany",
    "remove",
    "drop",
    "createIndex",
    "createIndexes",
    "dropIndex",
    "dropIndexes",
    "renameCollection",
    "findOneAndUpdate",
    "findOneAndReplace",
    "findOneAndDelete",
    "bulkWrite",
];

/// Methods of a collection, for completion.
pub const METHODS: &[&str] = &[
    "aggregate",
    "countDocuments",
    "createIndex",
    "deleteMany",
    "deleteOne",
    "distinct",
    "drop",
    "dropIndex",
    "estimatedDocumentCount",
    "find",
    "findOne",
    "getIndexes",
    "insertMany",
    "insertOne",
    "replaceOne",
    "stats",
    "updateMany",
    "updateOne",
];

fn tokens(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let (mut i, mut line, mut col) = (0, 0, 0);
    // Advances past `n` characters, keeping the position.
    let advance = |i: &mut usize, line: &mut usize, col: &mut usize, n: usize| {
        for _ in 0..n {
            if chars.get(*i) == Some(&'\n') {
                *line += 1;
                *col = 0;
            } else {
                *col += 1;
            }
            *i += 1;
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let start = (line, col);
        let from = i;
        let kind = match c {
            '\n' => {
                advance(&mut i, &mut line, &mut col, 1);
                Tok::Newline
            }
            c if c.is_whitespace() => {
                advance(&mut i, &mut line, &mut col, 1);
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                let n = chars[i..].iter().take_while(|c| **c != '\n').count();
                advance(&mut i, &mut line, &mut col, n);
                Tok::Comment
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let mut n = 2;
                while i + n < chars.len()
                    && !(chars[i + n] == '*' && chars.get(i + n + 1) == Some(&'/'))
                {
                    n += 1;
                }
                let n = (n + 2).min(chars.len() - i);
                advance(&mut i, &mut line, &mut col, n);
                Tok::Comment
            }
            // A regular expression where a value starts: after `(`, `[`,
            // `{`, `,` or `:`.
            '/' if out
                .iter()
                .rev()
                .find(|t| t.kind != Tok::Comment && t.kind != Tok::Newline)
                .is_some_and(|t| t.kind == Tok::Punct && "([{,:".contains(t.text.as_str())) =>
            {
                let mut n = 1;
                let mut class = false;
                while i + n < chars.len() && chars[i + n] != '\n' {
                    match chars[i + n] {
                        '\\' => n += 1,
                        '[' => class = true,
                        ']' => class = false,
                        '/' if !class => break,
                        _ => {}
                    }
                    n += 1;
                }
                n += 1;
                while chars.get(i + n).is_some_and(|c| c.is_ascii_alphabetic()) {
                    n += 1;
                }
                let n = n.min(chars.len() - i);
                advance(&mut i, &mut line, &mut col, n);
                Tok::Regex
            }
            q @ ('"' | '\'' | '`') => {
                let mut n = 1;
                while i + n < chars.len() && chars[i + n] != q {
                    if chars[i + n] == '\\' {
                        n += 1;
                    }
                    n += 1;
                }
                let n = (n + 1).min(chars.len() - i);
                advance(&mut i, &mut line, &mut col, n);
                Tok::String
            }
            c if c.is_ascii_digit()
                || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) =>
            {
                let n = chars[i..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '.' || **c == '_')
                    .count();
                advance(&mut i, &mut line, &mut col, n);
                Tok::Number
            }
            c if c.is_alphanumeric() || c == '_' || c == '$' => {
                let n = chars[i..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_' || **c == '$')
                    .count();
                advance(&mut i, &mut line, &mut col, n);
                Tok::Word
            }
            _ => {
                advance(&mut i, &mut line, &mut col, 1);
                Tok::Punct
            }
        };
        out.push(Token {
            start,
            end: (line, col),
            kind,
            text: chars[from..i].iter().collect(),
        });
    }
    out
}

/// The coloured pieces of each line.
pub fn highlight(lines: &[String]) -> Vec<Vec<Piece>> {
    let text = lines.join("\n");
    let mut out = vec![Vec::new(); lines.len()];
    let all = tokens(&text);
    for (i, token) in all.iter().enumerate() {
        let kind = match token.kind {
            Tok::String | Tok::Regex => Kind::String,
            Tok::Number => Kind::Number,
            Tok::Comment => Kind::Comment,
            Tok::Word if token.text.starts_with('$') || KEYWORDS.contains(&token.text.as_str()) => {
                Kind::Keyword
            }
            // A method: a word called.
            Tok::Word if all.get(i + 1).is_some_and(|t| t.text == "(") => Kind::Function,
            _ => continue,
        };
        for line in token.start.0..=token.end.0.min(lines.len().saturating_sub(1)) {
            let start = if line == token.start.0 {
                token.start.1
            } else {
                0
            };
            let end = if line == token.end.0 {
                token.end.1
            } else {
                lines[line].chars().count()
            };
            if end > start {
                out[line].push(Piece { start, end, kind });
            }
        }
    }
    out
}

/// The commands of a script. A command ends at `;`, or at the end of a line
/// when its brackets are closed and the next line doesn't carry on with a
/// `.` method.
pub fn statements(text: &str) -> Vec<Stmt> {
    let all = tokens(text);
    let mut found = Vec::new();
    let mut current: Option<(Pos, Pos)> = None;
    let mut depth = 0usize;
    let finish = |current: &mut Option<(Pos, Pos)>, found: &mut Vec<Stmt>, zone_end: Pos| {
        if let Some((start, end)) = current.take() {
            found.push(Stmt {
                start,
                end,
                zone_end,
            });
        }
    };
    for (i, token) in all.iter().enumerate() {
        match token.kind {
            Tok::Comment => continue,
            Tok::Newline => {
                let continues = all[i + 1..]
                    .iter()
                    .find(|t| !matches!(t.kind, Tok::Newline | Tok::Comment))
                    .is_some_and(|t| {
                        t.text == "." || t.text == ")" || t.text == "]" || t.text == "}"
                    });
                if depth == 0 && !continues {
                    let end = current.map(|(_, end)| end);
                    if let Some(end) = end {
                        finish(&mut current, &mut found, end);
                    }
                }
                continue;
            }
            Tok::Punct if token.text == ";" && depth == 0 => {
                if let Some((start, _)) = current {
                    current = Some((start, token.end));
                }
                finish(&mut current, &mut found, token.end);
                continue;
            }
            Tok::Punct => match token.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            },
            _ => {}
        }
        let start = current.map_or(token.start, |(start, _)| start);
        current = Some((start, token.end));
    }
    let end = crate::end_of(text);
    finish(&mut current, &mut found, end);
    found
}

/// The command the cursor is in or just after.
pub fn statement_at(text: &str, cursor: Pos) -> Option<Stmt> {
    let all = statements(text);
    all.iter()
        .find(|s| s.zone_end >= cursor || s.end >= cursor)
        .or(all.last())
        .copied()
}

/// Commands that delete, change or drop everything they reach: a delete or
/// update without a filter, dropping a collection or a database.
pub fn check(text: &str) -> Findings {
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut findings = Findings::default();
    for statement in statements(text) {
        let command = slice(&lines, statement.start, statement.end);
        let all: Vec<Token> = tokens(&command)
            .into_iter()
            .filter(|t| !matches!(t.kind, Tok::Comment | Tok::Newline))
            .collect();
        let words: Vec<&str> = all.iter().map(|t| t.text.as_str()).collect();
        // `db . name . method (` — the collection is the words before the
        // method.
        for (i, token) in all.iter().enumerate() {
            if token.kind != Tok::Word || words.get(i + 1) != Some(&"(") {
                continue;
            }
            let method = token.text.as_str();
            let collection = collection_before(&words[..i]);
            if method == "dropDatabase" {
                findings.writes = true;
                findings.destructive.push("dropDatabase".to_string());
                continue;
            }
            if !WRITES.contains(&method) {
                continue;
            }
            findings.writes = true;
            let filter_empty = || {
                // `()` or `({})`, so every document matches.
                matches!(&words[i + 2..], [")", ..] | ["{", "}", ..])
            };
            let what = collection.as_deref().unwrap_or("a collection");
            match method {
                "drop" => findings.destructive.push(format!("drop {what}")),
                "deleteMany" | "remove" if filter_empty() => findings
                    .destructive
                    .push(format!("{method} without a filter on {what}")),
                "updateMany" | "update" if filter_empty() => findings
                    .destructive
                    .push(format!("{method} without a filter on {what}")),
                _ => {}
            }
        }
    }
    findings
}

/// The collection named by `db.name` or `db.getCollection("name")` at the
/// end of `words`.
fn collection_before(words: &[&str]) -> Option<String> {
    match words {
        [.., "db", ".", name, "."] => Some((*name).to_string()),
        [.., "getCollection", "(", quoted, ")", "."] => {
            Some(quoted.trim_matches(|c| c == '"' || c == '\'').to_string())
        }
        [.., "db", "[", quoted, "]", "."] => {
            Some(quoted.trim_matches(|c| c == '"' || c == '\'').to_string())
        }
        _ => None,
    }
}

/// What may be typed at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Nothing,
    /// A collection, after `db.`.
    Collections,
    /// A method of this collection, after `db.name.`.
    Methods(String),
    /// A field of this collection, as a key inside `{ … }`.
    Fields(String),
}

/// What to complete at the cursor, from where the typed word starts.
pub fn context(text: &str, cursor: Pos) -> (Want, Pos, String) {
    let all: Vec<Token> = tokens(text)
        .into_iter()
        .filter(|t| t.start < cursor)
        .collect();
    // Inside a string, comment or regex there is nothing to complete.
    if all.last().is_some_and(|t| {
        matches!(t.kind, Tok::String | Tok::Comment | Tok::Regex) && cursor <= t.end && {
            let closed = t.text.len() > 1
                && t.text.chars().last() == t.text.chars().next()
                && t.kind == Tok::String;
            !closed || cursor < t.end
        }
    }) {
        return (Want::Nothing, cursor, String::new());
    }
    let (prefix, start, before) = match all.last() {
        Some(t) if t.kind == Tok::Word && t.end == cursor => {
            (t.text.clone(), t.start, &all[..all.len() - 1])
        }
        _ => (String::new(), cursor, &all[..]),
    };
    let words: Vec<&str> = before
        .iter()
        .filter(|t| !matches!(t.kind, Tok::Comment | Tok::Newline))
        .map(|t| t.text.as_str())
        .collect();
    let want = match words.as_slice() {
        [.., "db", "."] => Want::Collections,
        rest => match collection_before(rest) {
            Some(collection) => Want::Methods(collection),
            None => field_context(&words).map_or(Want::Nothing, Want::Fields),
        },
    };
    (want, start, prefix)
}

/// The collection whose document is being written, when the cursor is
/// where a key goes: right after `{` or `,` in a document passed straight
/// to `db.name.method(`.
fn field_context(words: &[&str]) -> Option<String> {
    if !matches!(words.last(), Some(&"{" | &",")) {
        return None;
    }
    // The bracket a position is directly inside: the last one not closed
    // before it.
    let enclosing = |end: usize| {
        let mut depth = 0usize;
        for i in (0..end).rev() {
            match words[i] {
                "}" | ")" | "]" => depth += 1,
                "{" | "(" | "[" if depth == 0 => return Some(i),
                "{" | "(" | "[" => depth -= 1,
                _ => {}
            }
        }
        None
    };
    let brace = if words.last() == Some(&"{") {
        words.len() - 1
    } else {
        enclosing(words.len() - 1)?
    };
    if words[brace] != "{" {
        return None;
    }
    let paren = enclosing(brace)?;
    if words[paren] != "(" || paren == 0 {
        return None;
    }
    // `db . name . method (`
    collection_before(&words[..paren - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    fn split(text: &str) -> Vec<String> {
        statements(text)
            .iter()
            .map(|s| slice(&lines(text), s.start, s.end))
            .collect()
    }

    #[test]
    fn commands_end_at_semicolons_and_finished_lines() {
        let text = "show dbs\nuse shop\ndb.users.find({\n  age: { $gt: 30 }\n})\n  .sort({ age: -1 })\n  // a note\n  .limit(5)\n\ndb.users.countDocuments({}); db.users.distinct('tags')";
        assert_eq!(
            split(text),
            [
                "show dbs",
                "use shop",
                "db.users.find({\n  age: { $gt: 30 }\n})\n  .sort({ age: -1 })\n  // a note\n  .limit(5)",
                "db.users.countDocuments({});",
                "db.users.distinct('tags')",
            ]
        );
        // `;` and line breaks inside strings, regexes and brackets don't end
        // a command.
        assert_eq!(
            split("db.a.find({ s: 'x;\\'y', r: /a;b/ })\ndb.b.find()"),
            ["db.a.find({ s: 'x;\\'y', r: /a;b/ })", "db.b.find()"]
        );
        let at = statement_at(text, (4, 1)).expect("a command");
        assert_eq!(at.start, (2, 0));
        assert_eq!(statement_at(text, (9, 2)).expect("count").start, (9, 0));
    }

    #[test]
    fn words_strings_and_operators_are_coloured() {
        let pieces = highlight(&lines("db.users.find({ $gt: 'a', n: 1 }) // hi"));
        let kinds: Vec<(usize, usize, Kind)> =
            pieces[0].iter().map(|p| (p.start, p.end, p.kind)).collect();
        assert_eq!(
            kinds,
            [
                (0, 2, Kind::Keyword),
                (9, 13, Kind::Function),
                (16, 19, Kind::Keyword),
                (21, 24, Kind::String),
                (29, 30, Kind::Number),
                (34, 39, Kind::Comment),
            ]
        );
    }

    #[test]
    fn deletes_and_drops_of_everything_are_caught() {
        let destructive = |text: &str| check(text).destructive;
        assert_eq!(
            destructive("db.users.deleteMany({})"),
            ["deleteMany without a filter on users"]
        );
        assert_eq!(
            destructive("db.users.remove()"),
            ["remove without a filter on users"]
        );
        assert_eq!(
            destructive("db.getCollection('odd name').updateMany({}, { $set: { a: 1 } })"),
            ["updateMany without a filter on odd name"]
        );
        assert_eq!(
            destructive("db.users.drop()\ndb.dropDatabase()"),
            ["drop users", "dropDatabase"]
        );
        assert!(destructive("db.users.deleteMany({ age: 3 })").is_empty());
        assert!(destructive("db.users.find({})").is_empty());
        assert!(check("db.users.insertOne({ a: 1 })").writes);
        assert!(!check("db.users.find().sort({ a: 1 })").writes);
    }

    #[test]
    fn completion_knows_collections_methods_and_fields() {
        let at = |text: &str| {
            let end = (0, text.chars().count());
            context(text, end)
        };
        assert_eq!(at("db."), (Want::Collections, (0, 3), String::new()));
        assert_eq!(at("db.us"), (Want::Collections, (0, 3), "us".into()));
        assert_eq!(
            at("db.users.fi"),
            (Want::Methods("users".into()), (0, 9), "fi".into())
        );
        assert_eq!(
            at("db.users.find({ ag"),
            (Want::Fields("users".into()), (0, 16), "ag".into())
        );
        assert_eq!(
            at("db.users.find({ age: 1, na"),
            (Want::Fields("users".into()), (0, 24), "na".into())
        );
        assert_eq!(
            at("db.users.find({ age: 'a"),
            (Want::Nothing, (0, 23), String::new())
        );
        assert_eq!(
            at("db.users.find({ age: { $in: [ x"),
            (Want::Nothing, (0, 30), "x".into())
        );
    }
}
