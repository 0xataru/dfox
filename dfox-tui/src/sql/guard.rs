//! Spotting statements worth a second look before they run: ones that remove
//! data wholesale, and ones that write at all (for read-only connections).
//!
//! Works on tokens rather than a full parse, so it copes with every dialect
//! and with text that doesn't parse. Words inside strings, comments and
//! function bodies are never mistaken for statements.

use dfox_core::models::connections::DbType;
use sqlparser::tokenizer::Token;

use super::{lex, Lexeme};

/// What the guard found in a script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    /// Why each destructive statement is destructive: `DELETE without WHERE
    /// on orders`, `DROP TABLE users`, ….
    pub destructive: Vec<String>,
    /// Some statement changes data or schema.
    pub writes: bool,
}

/// Verbs that change data or schema when they start a statement.
const WRITES: &[&str] = &[
    "ALTER", "CREATE", "DELETE", "DROP", "GRANT", "INSERT", "MERGE", "RENAME", "REPLACE", "REVOKE",
    "TRUNCATE", "UPDATE", "UPSERT",
];

pub fn check(sql: &str, driver: DbType) -> Findings {
    let mut findings = Findings::default();
    let tokens: Vec<Lexeme> = lex(sql, driver).into_iter().filter(|l| !l.trivia).collect();
    for statement in tokens.split(|l| l.semicolon) {
        check_statement(statement, &mut findings);
    }
    findings
}

/// A statement's tokens, each with its depth in parentheses.
fn depths(tokens: &[Lexeme]) -> Vec<usize> {
    let mut depth = 0usize;
    tokens
        .iter()
        .map(|t| match t.token {
            Token::LParen => {
                depth += 1;
                depth - 1
            }
            Token::RParen => {
                depth = depth.saturating_sub(1);
                depth
            }
            _ => depth,
        })
        .collect()
}

fn check_statement(tokens: &[Lexeme], findings: &mut Findings) {
    let depths = depths(tokens);
    let words: Vec<Option<String>> = tokens.iter().map(Lexeme::keyword).collect();
    let word = |i: usize| words.get(i).and_then(|w| w.as_deref());

    for i in 0..tokens.len() {
        let Some(verb) = word(i) else { continue };
        // A verb starts the statement, or a sub-statement in parentheses
        // (`WITH gone AS (DELETE …)`). Elsewhere `ON DELETE`, `FOR UPDATE`
        // and the like are clauses, not statements.
        let starts = i == 0 || tokens[i - 1].token == Token::LParen;
        let call = tokens.get(i + 1).is_some_and(|t| t.token == Token::LParen);
        // In parentheses only data changes are statements: `(REPLACE(…))` is
        // a function.
        let nested_dml = matches!(verb, "DELETE" | "UPDATE" | "INSERT" | "MERGE");
        if !starts || call || (i > 0 && !nested_dml) {
            continue;
        }
        if WRITES.contains(&verb) {
            findings.writes = true;
        }
        let depth = depths[i];
        // The tokens of this (sub-)statement: until its parenthesis closes.
        let end = (i + 1..tokens.len())
            .find(|&j| {
                depths[j] < depth || (depths[j] == depth && tokens[j].token == Token::RParen)
            })
            .unwrap_or(tokens.len());
        let has = |kw: &str| (i + 1..end).any(|j| depths[j] == depth && word(j) == Some(kw));
        let target = |from: usize| -> String {
            (from..end)
                .filter(|&j| depths[j] == depth)
                .take_while(|&j| word(j) != Some("DROP"))
                .map_while(|j| match &tokens[j].token {
                    Token::Word(w) => Some(w.value.clone()),
                    Token::Period => Some(".".to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
                .replace(" . ", ".")
        };
        match verb {
            "DELETE" | "UPDATE" if !has("WHERE") => {
                let table = if verb == "DELETE" {
                    // DELETE FROM t
                    let from = (i + 1..end).find(|&j| word(j) == Some("FROM"));
                    from.map(|j| target(j + 1)).unwrap_or_default()
                } else {
                    target(i + 1)
                };
                let table = first_name(&table);
                findings
                    .destructive
                    .push(format!("{verb} without WHERE on {table}"));
            }
            "DROP" => findings.destructive.push(format!("DROP {}", target(i + 1))),
            "TRUNCATE" => {
                let table = target(i + 1);
                let table = match table.split_once(' ') {
                    Some((first, rest)) if first.eq_ignore_ascii_case("TABLE") => rest,
                    _ => &table,
                };
                findings.destructive.push(format!("TRUNCATE {table}"));
            }
            "ALTER" if has("DROP") => findings
                .destructive
                .push(format!("ALTER {} … DROP", target(i + 1))),
            _ => {}
        }
    }
}

/// `ONLY public.orders` → `public.orders`: the table name in a target,
/// without modifiers.
fn first_name(target: &str) -> &str {
    target
        .split(' ')
        .find(|w| {
            !matches!(
                w.to_uppercase().as_str(),
                "ONLY" | "TABLE" | "LOW_PRIORITY" | "IGNORE" | "QUICK" | "FROM"
            )
        })
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destructive(sql: &str) -> Vec<String> {
        check(sql, DbType::Postgres).destructive
    }

    #[test]
    fn wholesale_changes_are_caught() {
        let cases = [
            ("DELETE FROM orders", "DELETE without WHERE on orders"),
            (
                "delete from public.orders;",
                "DELETE without WHERE on public.orders",
            ),
            (
                "UPDATE users SET admin = true",
                "UPDATE without WHERE on users",
            ),
            (
                "update only users set x = 1",
                "UPDATE without WHERE on users",
            ),
            ("DROP TABLE users", "DROP TABLE users"),
            (
                "drop table if exists a.b cascade",
                "DROP table if exists a.b cascade",
            ),
            ("DROP DATABASE shop", "DROP DATABASE shop"),
            ("TRUNCATE orders", "TRUNCATE orders"),
            ("truncate table orders", "TRUNCATE orders"),
            (
                "ALTER TABLE users DROP COLUMN email",
                "ALTER TABLE users … DROP",
            ),
            (
                "WITH gone AS (DELETE FROM t RETURNING *) SELECT * FROM gone",
                "DELETE without WHERE on t",
            ),
            (
                "DELETE FROM t USING (SELECT id FROM s WHERE s.x) AS s",
                "DELETE without WHERE on t",
            ),
        ];
        for (sql, reason) in cases {
            assert_eq!(destructive(sql), [reason], "{sql}");
        }
    }

    #[test]
    fn targeted_and_harmless_statements_pass() {
        let cases = [
            "DELETE FROM orders WHERE id = 1",
            "update users set admin = false where id in (select id from bad)",
            "DELETE FROM t WHERE id IN (SELECT id FROM s)",
            "SELECT * FROM orders -- DELETE FROM orders",
            "SELECT 'DROP TABLE users; DELETE FROM x'",
            "/* TRUNCATE orders */ SELECT 1",
            "CREATE TABLE a (b_id int REFERENCES b ON DELETE CASCADE ON UPDATE CASCADE)",
            "SELECT * FROM t FOR UPDATE",
            "INSERT INTO t VALUES (1) ON DUPLICATE KEY UPDATE x = 1",
            "INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET x = 1 WHERE t.x < 1",
            "CREATE FUNCTION f() RETURNS void AS $$ DELETE FROM t $$ LANGUAGE sql",
            "ALTER TABLE users ADD COLUMN email text",
            "CREATE TRIGGER t BEFORE DELETE ON x FOR EACH ROW EXECUTE FUNCTION f()",
            "",
            "   ",
        ];
        for sql in cases {
            assert!(destructive(sql).is_empty(), "{sql}: {:?}", destructive(sql));
        }
    }

    #[test]
    fn each_statement_of_a_script_is_checked() {
        assert_eq!(
            destructive("SELECT 1; DELETE FROM a; DELETE FROM b WHERE x; DROP VIEW v"),
            ["DELETE without WHERE on a", "DROP VIEW v"]
        );
    }

    #[test]
    fn writes_are_told_from_reads() {
        let writes = |sql| check(sql, DbType::MySql).writes;
        for sql in [
            "INSERT INTO t VALUES (1)",
            "update t set x = 1 where id = 2",
            "CREATE TABLE t (i int)",
            "SELECT 1; DELETE FROM t WHERE id = 1",
            "replace into t values (1)",
            "WITH x AS (UPDATE t SET a = 1 RETURNING *) SELECT * FROM x",
        ] {
            assert!(writes(sql), "{sql}");
        }
        for sql in [
            "SELECT * FROM t",
            "SELECT (REPLACE(name, 'a', 'b')) FROM t",
            "with x as (select 1) select * from x",
            "SHOW TABLES",
            "EXPLAIN SELECT 1",
            "select * from t for update",
        ] {
            assert!(!writes(sql), "{sql}");
        }
    }

    #[test]
    fn other_dialects_work_too() {
        assert_eq!(
            check("DELETE FROM `orders`", DbType::MySql).destructive,
            ["DELETE without WHERE on orders"]
        );
        assert_eq!(
            check("DELETE FROM \"Orders\" -- all", DbType::Sqlite).destructive,
            ["DELETE without WHERE on Orders"]
        );
    }
}
