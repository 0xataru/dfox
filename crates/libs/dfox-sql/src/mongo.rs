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

/// Methods of a collection, for completion, with their arguments.
pub const METHODS: &[(&str, &str)] = &[
    ("aggregate", "pipeline, options"),
    ("countDocuments", "filter, options"),
    ("createIndex", "keys, options"),
    ("deleteMany", "filter, options"),
    ("deleteOne", "filter, options"),
    ("distinct", "field, filter"),
    ("drop", ""),
    ("dropIndex", "index"),
    ("estimatedDocumentCount", "options"),
    ("find", "filter, projection"),
    ("findOne", "filter, projection"),
    ("findOneAndDelete", "filter, options"),
    ("findOneAndReplace", "filter, replacement, options"),
    ("findOneAndUpdate", "filter, update, options"),
    ("getIndexes", ""),
    ("insertMany", "documents, options"),
    ("insertOne", "document, options"),
    ("replaceOne", "filter, replacement, options"),
    ("stats", ""),
    ("updateMany", "filter, update, options"),
    ("updateOne", "filter, update, options"),
];

/// An operator or key, with what goes with it.
pub type Operator = (&'static str, &'static str);

/// Keys a filter starts with besides fields.
pub const LOGICAL: &[Operator] = &[
    ("$and", "[filter, …]"),
    ("$or", "[filter, …]"),
    ("$nor", "[filter, …]"),
    ("$expr", "expression"),
];

/// Operators on a field's value in a filter.
pub const QUERY: &[Operator] = &[
    ("$eq", "value"),
    ("$ne", "value"),
    ("$gt", "value"),
    ("$gte", "value"),
    ("$lt", "value"),
    ("$lte", "value"),
    ("$in", "[value, …]"),
    ("$nin", "[value, …]"),
    ("$exists", "bool"),
    ("$type", "type"),
    ("$regex", "/pattern/"),
    ("$elemMatch", "{ filter }"),
    ("$size", "n"),
    ("$all", "[value, …]"),
    ("$not", "{ operator }"),
];

/// Update operators.
pub const UPDATE: &[Operator] = &[
    ("$set", "{ field: value }"),
    ("$unset", "{ field: \"\" }"),
    ("$inc", "{ field: n }"),
    ("$push", "{ field: value }"),
    ("$pull", "{ field: filter }"),
    ("$addToSet", "{ field: value }"),
    ("$rename", "{ field: name }"),
    ("$setOnInsert", "{ field: value }"),
    ("$min", "{ field: value }"),
    ("$max", "{ field: value }"),
    ("$mul", "{ field: n }"),
    ("$currentDate", "{ field: true }"),
];

/// Aggregation stages, with their shapes.
pub const STAGES: &[Operator] = &[
    ("$match", "{ filter }"),
    ("$group", "{ _id: expr, field: { $sum: expr } }"),
    ("$project", "{ field: 1 }"),
    ("$sort", "{ field: 1 | -1 }"),
    ("$limit", "n"),
    ("$skip", "n"),
    ("$lookup", "{ from, localField, foreignField, as }"),
    ("$unwind", "\"$field\""),
    ("$addFields", "{ field: expr }"),
    ("$set", "{ field: expr }"),
    ("$unset", "\"field\""),
    ("$count", "\"name\""),
    ("$facet", "{ name: [stage, …] }"),
    ("$replaceRoot", "{ newRoot: expr }"),
    ("$sample", "{ size: n }"),
];

/// Keys of a `$group` stage besides the outputs.
pub const GROUP: &[Operator] = &[("_id", "group key")];

/// Accumulators of a `$group` output.
pub const ACCUMULATORS: &[Operator] = &[
    ("$sum", "expr"),
    ("$avg", "expr"),
    ("$min", "expr"),
    ("$max", "expr"),
    ("$first", "expr"),
    ("$last", "expr"),
    ("$push", "expr"),
    ("$addToSet", "expr"),
    ("$count", "{}"),
];

/// Keys of a `$lookup` stage.
pub const LOOKUP: &[Operator] = &[
    ("from", "collection"),
    ("localField", "field"),
    ("foreignField", "field"),
    ("as", "name"),
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
    /// A key inside `{ … }`: fields of `fields`' collection, and the
    /// operators that fit there.
    Keys {
        fields: Option<String>,
        operators: &'static [Operator],
    },
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
    let words = words(before);
    let want = match words.as_slice() {
        [.., "db", "."] => Want::Collections,
        rest => match collection_before(rest) {
            Some(collection) => Want::Methods(collection),
            None => keys(&words),
        },
    };
    (want, start, prefix)
}

/// The method call the cursor is in, and which argument, from 0.
pub fn call(text: &str, cursor: Pos) -> Option<(String, usize)> {
    let all: Vec<Token> = tokens(text)
        .into_iter()
        .filter(|t| t.end <= cursor)
        .collect();
    let words = words(&all);
    frames(&words)
        .iter()
        .rev()
        .find_map(|f| Some((f.method.clone()?.1, f.args)))
}

fn words(tokens: &[Token]) -> Vec<&str> {
    tokens
        .iter()
        .filter(|t| !matches!(t.kind, Tok::Comment | Tok::Newline))
        .map(|t| t.text.as_str())
        .collect()
}

/// A bracket open before the cursor.
#[derive(Debug)]
struct Frame {
    open: char,
    /// The key whose value it is: `age` in `{ age: {`.
    key: Option<String>,
    /// For `db.name.method(`: the collection and the method.
    method: Option<(String, String)>,
    /// Commas seen at this level.
    args: usize,
}

fn unquote(word: &str) -> String {
    word.trim_matches(|c| c == '"' || c == '\'').to_string()
}

/// The brackets still open at the end of `words`, outermost first.
fn frames(words: &[&str]) -> Vec<Frame> {
    let mut open: Vec<Frame> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        match *word {
            "{" | "[" | "(" => {
                let key = (i >= 2 && words[i - 1] == ":").then(|| unquote(words[i - 2]));
                let method = (*word == "(" && i >= 1)
                    .then(|| {
                        collection_before(&words[..i - 1]).map(|c| (c, words[i - 1].to_string()))
                    })
                    .flatten();
                open.push(Frame {
                    open: word.chars().next().unwrap_or('{'),
                    key,
                    method,
                    args: 0,
                });
            }
            "}" | "]" | ")" => {
                open.pop();
            }
            "," => {
                if let Some(frame) = open.last_mut() {
                    frame.args += 1;
                }
            }
            _ => {}
        }
    }
    open
}

/// What a `{ … }` holds, from where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Filter,
    Update,
    Document,
    Fields,
    Stage,
    Group,
    Accumulator,
    Lookup,
    Query,
    Other,
}

fn role(frames: &[Frame], i: usize) -> Role {
    let frame = &frames[i];
    if frame.open != '{' {
        return Role::Other;
    }
    let parent = i.checked_sub(1).map(|p| &frames[p]);
    match frame.key.as_deref() {
        Some("$match" | "$elemMatch") => Role::Filter,
        Some("$group") => Role::Group,
        Some("$lookup") => Role::Lookup,
        Some(
            "$project" | "$sort" | "$set" | "$unset" | "$addFields" | "$inc" | "$push" | "$pull"
            | "$addToSet" | "$min" | "$max" | "$mul" | "$rename" | "$setOnInsert" | "$currentDate",
        ) => Role::Fields,
        Some(key) if key.starts_with('$') => Role::Other,
        // A field's value: operators in a filter, an accumulator in a group.
        Some(_) => match i.checked_sub(1).map(|p| role(frames, p)) {
            Some(Role::Filter) => Role::Query,
            Some(Role::Group) => Role::Accumulator,
            _ => Role::Other,
        },
        None => match parent {
            Some(Frame {
                open: '(',
                method: Some((_, method)),
                args,
                ..
            }) => match (method.as_str(), args) {
                ("insertOne", 0) | ("replaceOne" | "findOneAndReplace", 1) => Role::Document,
                ("updateOne" | "updateMany" | "findOneAndUpdate", 1) => Role::Update,
                ("find" | "findOne", 1) | ("createIndex", 0) => Role::Fields,
                ("distinct", 1) | (_, 0) if method != "aggregate" => Role::Filter,
                _ => Role::Other,
            },
            Some(Frame { open: '[', key, .. }) => match key.as_deref() {
                Some("$and" | "$or" | "$nor") => Role::Filter,
                _ => match i.checked_sub(2).map(|g| &frames[g]) {
                    Some(Frame {
                        method: Some((_, method)),
                        args: 0,
                        ..
                    }) if method == "aggregate" => Role::Stage,
                    Some(Frame {
                        method: Some((_, method)),
                        args: 0,
                        ..
                    }) if method == "insertMany" => Role::Document,
                    _ => Role::Other,
                },
            },
            _ => Role::Other,
        },
    }
}

/// The keys that fit at the end of `words`: right after `{` or `,` inside
/// an object.
fn keys(words: &[&str]) -> Want {
    if !matches!(words.last(), Some(&"{" | &",")) {
        return Want::Nothing;
    }
    let frames = frames(words);
    let Some(last) = frames.len().checked_sub(1) else {
        return Want::Nothing;
    };
    let collection = frames
        .iter()
        .rev()
        .find_map(|f| f.method.as_ref())
        .map(|(c, _)| c.clone());
    let (fields, operators): (bool, &'static [Operator]) = match role(&frames, last) {
        Role::Filter => (true, LOGICAL),
        Role::Update => (false, UPDATE),
        Role::Document | Role::Fields => (true, &[]),
        Role::Stage => (false, STAGES),
        Role::Group => (false, GROUP),
        Role::Accumulator => (false, ACCUMULATORS),
        Role::Lookup => (false, LOOKUP),
        Role::Query => (false, QUERY),
        Role::Other => return Want::Nothing,
    };
    Want::Keys {
        fields: collection.filter(|_| fields),
        operators,
    }
}

/// Width a document is kept on one line within.
const WIDTH: usize = 72;

/// A token, or a bracketed group of them.
enum Node<'a> {
    Leaf(&'a Token),
    Group(&'a Token, Vec<Node<'a>>, Option<&'a Token>),
}

impl Node<'_> {
    fn first(&self) -> &str {
        match self {
            Node::Leaf(t) | Node::Group(t, ..) => &t.text,
        }
    }

    fn last(&self) -> &str {
        match self {
            Node::Leaf(t) | Node::Group(_, _, Some(t)) => &t.text,
            Node::Group(t, _, None) => &t.text,
        }
    }
}

fn tree<'a>(tokens: &mut std::slice::Iter<'a, Token>) -> (Vec<Node<'a>>, Option<&'a Token>) {
    let mut nodes = Vec::new();
    while let Some(token) = tokens.next() {
        match token.text.as_str() {
            "(" | "[" | "{" if token.kind == Tok::Punct => {
                let (inner, close) = tree(tokens);
                nodes.push(Node::Group(token, inner, close));
            }
            ")" | "]" | "}" if token.kind == Tok::Punct => return (nodes, Some(token)),
            _ => nodes.push(Node::Leaf(token)),
        }
    }
    (nodes, None)
}

/// Whether a space goes between two neighbours on a line.
fn spaced(before: &Node, after: &Node, sign: bool) -> bool {
    let call = matches!(after, Node::Group(t, ..) if t.text == "(")
        && matches!(before, Node::Leaf(t) if t.kind == Tok::Word)
        || before.last() == ")" && after.first() == "(";
    !(sign || call || matches!(after.first(), "," | ":" | "." | ";") || before.last() == ".")
}

/// Nodes one after another, groups laid out at `indent`.
fn line(nodes: &[Node], indent: usize) -> String {
    let mut out = String::new();
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            // `-1` where a value starts.
            let sign = i >= 1
                && matches!(nodes[i - 1].first(), "-" | "+")
                && (i == 1 || matches!(nodes[i - 2].last(), ":" | "," | "("));
            if spaced(&nodes[i - 1], node, sign) {
                out.push(' ');
            }
        }
        match node {
            Node::Leaf(t) => out.push_str(&t.text),
            Node::Group(open, inner, close) => out.push_str(&group(open, inner, *close, indent)),
        }
    }
    out
}

fn group(open: &Token, inner: &[Node], close: Option<&Token>, indent: usize) -> String {
    let close = close.map_or("", |t| t.text.as_str());
    let flat = line(inner, indent);
    if open.text == "(" {
        return format!("({flat}{close}");
    }
    let padded = if open.text == "{" && !inner.is_empty() {
        format!("{{ {flat} {close}")
    } else {
        format!("{}{flat}{close}", open.text)
    };
    if !padded.contains('\n') && indent * 2 + padded.chars().count() <= WIDTH {
        return padded;
    }
    let pad = "  ".repeat(indent + 1);
    let items: Vec<String> = inner
        .split(|n| matches!(n, Node::Leaf(t) if t.text == ","))
        .filter(|item| !item.is_empty())
        .map(|item| format!("{pad}{}", line(item, indent + 1)))
        .collect();
    format!(
        "{}\n{}\n{}{close}",
        open.text,
        items.join(",\n"),
        "  ".repeat(indent)
    )
}

/// Shell commands laid out: documents and arrays that don't fit a line
/// get a line a field, indented. Only whitespace changes: when anything
/// else would, the text is left as it is.
pub fn format(text: &str) -> String {
    let all = tokens(text);
    // shortcut: text with comments is left as it is, lay them out too if
    // people ask for it.
    if all.iter().any(|t| t.kind == Tok::Comment) {
        return text.to_string();
    }
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let out: Vec<String> = statements(text)
        .iter()
        .map(|s| {
            let command = tokens(&slice(&lines, s.start, s.end));
            let kept: Vec<Token> = command
                .into_iter()
                .filter(|t| t.kind != Tok::Newline)
                .collect();
            let (nodes, _) = tree(&mut kept.iter());
            line(&nodes, 0)
        })
        .collect();
    let out = out.join("\n") + if text.ends_with('\n') { "\n" } else { "" };
    let words = |text: &str| -> Vec<(Tok, String)> {
        tokens(text)
            .into_iter()
            .filter(|t| t.kind != Tok::Newline)
            .map(|t| (t.kind, t.text))
            .collect()
    };
    if words(&out) == words(text) {
        out
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_that_dont_fit_get_a_line_a_field() {
        assert_eq!(
            format("db.users.find({age:{$gt:-1}},{name:1}).sort({ age : -1 }).limit(5)"),
            "db.users.find({ age: { $gt: -1 } }, { name: 1 }).sort({ age: -1 }).limit(5)"
        );
        assert_eq!(
            format("db.orders.aggregate([{$match:{status:'paid',total:{$gte:100}}},{$group:{_id:'$customer',sum:{$sum:'$total'}}}])"),
            "db.orders.aggregate([\n  { $match: { status: 'paid', total: { $gte: 100 } } },\n  { $group: { _id: '$customer', sum: { $sum: '$total' } } }\n])"
        );
    }

    #[test]
    fn formatting_never_changes_what_runs() {
        for text in [
            "db.a.find({x: /a,b/i, d: new Date('2024-01-01'), s: \"it's: {}\"})\nshow dbs",
            "db.a.insertMany([{a: 1}, {b: [1, 2, 3]}]);",
            "db.a.find({a: 1}) // why",
            "db.a.find({",
        ] {
            let formatted = format(text);
            let words = |t: &str| -> Vec<String> {
                tokens(t)
                    .into_iter()
                    .filter(|t| t.kind != Tok::Newline)
                    .map(|t| t.text)
                    .collect()
            };
            assert_eq!(words(&formatted), words(text), "{formatted}");
            assert_eq!(format(&formatted), formatted, "stable: {formatted}");
        }
    }

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
        let filter = Want::Keys {
            fields: Some("users".into()),
            operators: LOGICAL,
        };
        assert_eq!(
            at("db.users.find({ ag"),
            (filter.clone(), (0, 16), "ag".into())
        );
        assert_eq!(
            at("db.users.find({ age: 1, na"),
            (filter, (0, 24), "na".into())
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

    #[test]
    fn operators_stages_and_keys_by_where_the_object_sits() {
        let want = |text: &str| context(text, (0, text.chars().count())).0;
        let keys = |fields: Option<&str>, operators| Want::Keys {
            fields: fields.map(str::to_string),
            operators,
        };
        assert_eq!(want("db.users.find({ age: { $g"), keys(None, QUERY));
        assert_eq!(
            want("db.users.find({ $or: [{ "),
            keys(Some("users"), LOGICAL)
        );
        assert_eq!(want("db.users.updateOne({ a: 1 }, { $"), keys(None, UPDATE));
        assert_eq!(
            want("db.users.updateOne({ a: 1 }, { $set: { "),
            keys(Some("users"), &[])
        );
        assert_eq!(want("db.users.insertOne({ "), keys(Some("users"), &[]));
        assert_eq!(want("db.users.find({}, { "), keys(Some("users"), &[]));
        assert_eq!(want("db.users.aggregate([{ $m"), keys(None, STAGES));
        assert_eq!(
            want("db.users.aggregate([{ $match: { a: 1 } }, { $"),
            keys(None, STAGES)
        );
        assert_eq!(
            want("db.users.aggregate([{ $match: { "),
            keys(Some("users"), LOGICAL)
        );
        assert_eq!(want("db.users.aggregate([{ $group: { "), keys(None, GROUP));
        assert_eq!(
            want("db.users.aggregate([{ $group: { _id: 1, n: { $s"),
            keys(None, ACCUMULATORS)
        );
        assert_eq!(
            want("db.users.aggregate([{ $lookup: { "),
            keys(None, LOOKUP)
        );
        assert_eq!(want("db.users.aggregate({ "), Want::Nothing);
    }

    #[test]
    fn the_method_and_argument_at_the_cursor() {
        let at = |text: &str| call(text, (0, text.chars().count()));
        assert_eq!(at("db.users.find("), Some(("find".into(), 0)));
        assert_eq!(
            at("db.users.updateOne({ a: [1, 2] }, "),
            Some(("updateOne".into(), 1))
        );
        assert_eq!(at("db.users.find({}).limit(1)"), None);
    }
}
