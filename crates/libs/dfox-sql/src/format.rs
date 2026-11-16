//! SQL and CQL laid out: each clause on a line of its own, subqueries
//! indented, keywords upper-case. Only whitespace and the case of keywords
//! change: when anything else would, the text is left as it is, so
//! formatting never changes what runs.

use sqlparser::tokenizer::{Token, Whitespace};

use super::{lex, slice, without_delimiters, Dialect, Kind, Lexeme};

/// Words that start a clause on a line of its own.
const CLAUSES: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "VALUES",
    "SET",
    "RETURNING",
    "WINDOW",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "FULL",
    "CROSS",
    "NATURAL",
    "INSERT",
    "UPDATE",
    "DELETE",
];

/// Words a clause's first word follows on the same line: `LEFT JOIN`,
/// `DELETE FROM`, `ON CONFLICT DO UPDATE`, `FOR UPDATE`.
const JOINED: &[&str] = &[
    "LEFT", "RIGHT", "INNER", "FULL", "CROSS", "NATURAL", "OUTER", "DELETE", "DO", "FOR",
];

/// Words a table's name follows: its case may matter (MySQL on Linux), so
/// a name that is also a keyword is left as it is.
const NAMES: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];

const INDENT: &str = "  ";

pub fn format(text: &str, dialect: Dialect) -> String {
    if without_delimiters(text, dialect).is_some() {
        return text.to_string();
    }
    let lexemes = lex(text, dialect);
    if lexemes.iter().any(|l| l.token == Token::EOF) {
        return text.to_string();
    }
    let lines: Vec<String> = crate::lines(text);
    let tokens: Vec<&Lexeme> = lexemes.iter().filter(|l| !l.trivia || comment(l)).collect();
    let mut out = String::new();
    // Each open bracket, and whether it holds a subquery.
    let mut brackets: Vec<bool> = Vec::new();
    let mut clause = String::new();
    let mut between = false;
    let mut unary = false;
    let mut previous: Option<&Lexeme> = None;
    for (i, lexeme) in tokens.iter().enumerate() {
        let next = tokens[i + 1..].iter().find(|l| !comment(l));
        let word = lexeme.keyword();
        let word = word.as_deref().unwrap_or_default();
        let level = brackets.last().is_none_or(|subquery| *subquery);
        let depth = brackets.iter().filter(|s| **s).count();
        let after = previous.and_then(|p| p.keyword()).unwrap_or_default();
        let breaks = level
            && previous.is_some_and(|p| !p.semicolon)
            && ((CLAUSES.contains(&word) && !JOINED.contains(&after.as_str()))
                || (matches!(word, "AND" | "OR")
                    && !between
                    && matches!(clause.as_str(), "WHERE" | "HAVING")));
        if word == "BETWEEN" {
            between = true;
        } else if word == "AND" {
            between = false;
        }
        if level && CLAUSES.contains(&word) {
            clause = word.to_string();
        }
        let name = (NAMES.contains(&after.as_str()) && !CLAUSES.contains(&word))
            || next.is_some_and(|n| n.token == Token::Period)
            || previous.is_some_and(|p| p.token == Token::Period)
            || (clause == "FROM" && previous.is_some_and(|p| p.token == Token::Comma));
        let original = slice(&lines, lexeme.start, lexeme.end);
        let text = if lexeme.kind == Kind::Keyword && !name {
            word.to_string()
        } else {
            original.trim_end_matches('\n').to_string()
        };
        // A subquery's closing bracket goes under its opening line.
        let closes = lexeme.token == Token::RParen && brackets.last() == Some(&true);
        let depth = depth - usize::from(closes);
        if previous.is_some_and(|p| p.semicolon) {
            out.push_str("\n\n");
        } else if (breaks || closes) && !out.ends_with('\n') {
            out.push('\n');
        }
        if let Some(previous) = previous {
            if out.ends_with('\n') {
                let extra = usize::from(breaks && matches!(word, "AND" | "OR"));
                out.push_str(&INDENT.repeat(depth + extra));
            } else if !unary && spaced(previous, lexeme) {
                out.push(' ');
            }
        }
        out.push_str(&text);
        // A sign, not a subtraction: `= -1`, `(-x)`.
        unary = matches!(lexeme.token, Token::Minus | Token::Plus)
            && previous.is_none_or(|p| !value(p));
        match lexeme.token {
            Token::LParen => {
                let subquery = next.is_some_and(|n| {
                    matches!(n.keyword().as_deref(), Some("SELECT" | "WITH" | "VALUES"))
                });
                brackets.push(subquery);
                if subquery {
                    out.push('\n');
                }
            }
            Token::RParen => {
                brackets.pop();
            }
            _ => {}
        }
        if matches!(
            lexeme.token,
            Token::Whitespace(Whitespace::SingleLineComment { .. })
        ) {
            out.push('\n');
        }
        previous = Some(lexeme);
    }
    let out = out.trim_end().to_string() + if text.ends_with('\n') { "\n" } else { "" };
    if same(&lexemes, &lex(&out, dialect)) {
        out
    } else {
        text.to_string()
    }
}

fn comment(lexeme: &Lexeme) -> bool {
    matches!(
        lexeme.token,
        Token::Whitespace(Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_))
    )
}

/// Whether a token ends a value, so a `-` after it subtracts.
fn value(lexeme: &Lexeme) -> bool {
    matches!(
        lexeme.token,
        Token::RParen | Token::RBracket | Token::Number(..) | Token::Placeholder(_)
    ) || lexeme.kind == Kind::String
        || matches!(lexeme.token, Token::Word(_)) && lexeme.kind != Kind::Keyword
}

/// Whether a space goes between two tokens on a line.
fn spaced(previous: &Lexeme, token: &Lexeme) -> bool {
    let tight_after = matches!(
        previous.token,
        Token::LParen | Token::LBracket | Token::Period | Token::DoubleColon
    );
    let tight_before = matches!(
        token.token,
        Token::RParen
            | Token::RBracket
            | Token::Comma
            | Token::SemiColon
            | Token::Period
            | Token::DoubleColon
            | Token::LBracket
    );
    // A call or a column list: `count(*)`, `t(a, b)`, `"Users"(id)`.
    let call = token.token == Token::LParen
        && matches!(previous.token, Token::Word(_))
        && previous.kind != Kind::Keyword;
    !(tight_after || tight_before || call)
}

/// Whether two texts hold the same tokens and comments, unquoted words in
/// any case.
fn same(a: &[Lexeme], b: &[Lexeme]) -> bool {
    let kept = |lexemes: &[Lexeme]| -> Vec<Token> {
        lexemes
            .iter()
            .filter(|l| !l.trivia || comment(l))
            .map(|l| match &l.token {
                Token::Word(word) if word.quote_style.is_none() => {
                    let mut word = word.clone();
                    word.value = word.value.to_uppercase();
                    word.keyword = sqlparser::keywords::Keyword::NoKeyword;
                    Token::Word(word)
                }
                Token::Whitespace(Whitespace::SingleLineComment { comment, prefix }) => {
                    Token::Whitespace(Whitespace::SingleLineComment {
                        comment: comment.trim_end().to_string(),
                        prefix: prefix.clone(),
                    })
                }
                other => other.clone(),
            })
            .collect()
    };
    kept(a) == kept(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clauses_go_on_lines_of_their_own() {
        let sql = "select u.id, count(*) as n from users u left join orders o on o.user_id = u.id where u.active and o.total between 1 and 9 or u.vip group by u.id having count(*) > 1 order by n desc limit 10;\n-- next\nselect 1";
        assert_eq!(
            format(sql, Dialect::Postgres),
            "SELECT u.id, count(*) AS n\nFROM users u\nLEFT JOIN orders o ON o.user_id = u.id\nWHERE u.active\n  AND o.total BETWEEN 1 AND 9\n  OR u.vip\nGROUP BY u.id\nHAVING count(*) > 1\nORDER BY n DESC\nLIMIT 10;\n\n-- next\nSELECT 1"
        );
    }

    #[test]
    fn subqueries_are_indented_and_windows_left_alone() {
        let sql = "select * from (select a, row_number() over (partition by b order by c) from t where x in (1, 2)) s where s.a::int > 0";
        assert_eq!(
            format(sql, Dialect::Postgres),
            "SELECT *\nFROM (\n  SELECT a, row_number() OVER (PARTITION BY b ORDER BY c)\n  FROM t\n  WHERE x IN (1, 2)\n) s\nWHERE s.a::INT > 0"
        );
    }

    #[test]
    fn what_runs_never_changes() {
        for (sql, dialect) in [
            ("insert into t (a, b) values (1, 'x;y') on conflict (a) do update set b = excluded.b returning *", Dialect::Postgres),
            ("select data->>'name', $1, e'\\n' from \"Users\" where tags @> array['a']", Dialect::Postgres),
            ("delete from `user` where id = ? /* why */ limit 1", Dialect::MySql),
            ("create function f() returns int as $$ select 1 $$ language sql", Dialect::Postgres),
            ("select * from ks.t where id = 1 allow filtering", Dialect::Cql),
            ("select 'unterminated", Dialect::Postgres),
            ("DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END $$\nDELIMITER ;", Dialect::MySql),
        ] {
            let formatted = format(sql, dialect);
            assert!(same(&lex(sql, dialect), &lex(&formatted, dialect)), "{sql}\n{formatted}");
            assert_eq!(format(&formatted, dialect), formatted, "stable: {formatted}");
        }
    }
}
