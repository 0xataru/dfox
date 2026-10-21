//! Reading Redis commands without running them, the way redis-cli takes
//! them: one command a line, words split at spaces, `"double quotes"` with
//! escapes and `'single quotes'` without. A line starting with `#` is a
//! comment.

use crate::{guard::Findings, Kind, Piece, Pos, Stmt};

/// A word of a line, from column `start` to `end`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    start: usize,
    end: usize,
    text: String,
    quoted: bool,
}

/// The words of one command line, or why they can't be read.
pub fn words(line: &str) -> Result<Vec<String>, String> {
    scan(line).map(|words| words.into_iter().map(|w| w.text).collect())
}

fn scan(line: &str) -> Result<Vec<Word>, String> {
    let chars: Vec<char> = line.chars().collect();
    let mut words = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let mut text = String::new();
        let quote = chars[i];
        if quote == '"' || quote == '\'' {
            i += 1;
            loop {
                let Some(&c) = chars.get(i) else {
                    return Err(format!("a {quote} quote isn't closed"));
                };
                i += 1;
                match c {
                    c if c == quote => break,
                    '\\' if quote == '\'' => {
                        if chars.get(i) == Some(&'\'') {
                            text.push('\'');
                            i += 1;
                        } else {
                            text.push('\\');
                        }
                    }
                    '\\' => {
                        let Some(&escaped) = chars.get(i) else {
                            return Err(format!("a {quote} quote isn't closed"));
                        };
                        i += 1;
                        match escaped {
                            'n' => text.push('\n'),
                            'r' => text.push('\r'),
                            't' => text.push('\t'),
                            'b' => text.push('\u{8}'),
                            'a' => text.push('\u{7}'),
                            'x' => {
                                let hex: String = chars.iter().skip(i).take(2).collect();
                                match u8::from_str_radix(&hex, 16) {
                                    // shortcut: a byte above 0x7f becomes that
                                    // code point, not raw bytes; send bytes if
                                    // binary values are needed.
                                    Ok(byte) if hex.len() == 2 => {
                                        text.push(char::from(byte));
                                        i += 2;
                                    }
                                    _ => text.push('x'),
                                }
                            }
                            other => text.push(other),
                        }
                    }
                    c => text.push(c),
                }
            }
            if chars.get(i).is_some_and(|c| !c.is_whitespace()) {
                return Err("a closing quote must be followed by a space".into());
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                text.push(chars[i]);
                i += 1;
            }
        }
        words.push(Word {
            start,
            end: i,
            text,
            quoted: quote == '"' || quote == '\'',
        });
    }
    Ok(words)
}

/// `word` as a command takes it: bare when it can be, in double quotes
/// otherwise.
pub fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '"' | '\'' | '\\'));
    if plain {
        return word.to_string();
    }
    let mut out = String::from('"');
    for c in word.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// The commands of a script: each line that isn't blank or a comment.
pub fn statements(text: &str) -> Vec<Stmt> {
    text.split('\n')
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !is_comment(line))
        .map(|(n, line)| {
            let start = line.chars().take_while(|c| c.is_whitespace()).count();
            let end = line.trim_end().chars().count();
            Stmt {
                start: (n, start),
                end: (n, end),
                zone_end: (n, line.chars().count()),
            }
        })
        .collect()
}

/// The command the cursor is in or just after.
pub fn statement_at(text: &str, cursor: Pos) -> Option<Stmt> {
    let all = statements(text);
    all.iter()
        .rev()
        .find(|s| s.start.0 <= cursor.0)
        .or(all.first())
        .copied()
}

/// The coloured pieces of each line.
pub fn highlight(lines: &[String]) -> Vec<Vec<Piece>> {
    lines
        .iter()
        .map(|line| {
            if is_comment(line) {
                let start = line.chars().take_while(|c| c.is_whitespace()).count();
                let end = line.chars().count();
                return vec![Piece {
                    start,
                    end,
                    kind: Kind::Comment,
                }];
            }
            // A line still being typed may have an open quote: colour what
            // can be read.
            let words = scan(line).unwrap_or_default();
            let command = words
                .first()
                .map(|w| w.text.to_uppercase())
                .unwrap_or_default();
            words
                .iter()
                .enumerate()
                .filter_map(|(i, word)| {
                    let name = (i == 0 && COMMANDS.contains(&command.as_str()))
                        || (i == 1
                            && CONTAINERS.contains(&command.as_str())
                            && word
                                .text
                                .chars()
                                .all(|c| c.is_ascii_alphabetic() || c == '-'));
                    let kind = if word.quoted {
                        Kind::String
                    } else if name {
                        Kind::Keyword
                    } else if word.text.parse::<f64>().is_ok() {
                        Kind::Number
                    } else {
                        return None;
                    };
                    Some(Piece {
                        start: word.start,
                        end: word.end,
                        kind,
                    })
                })
                .collect()
        })
        .collect()
}

/// Commands that wipe out a whole database or stop the server.
pub fn check(text: &str) -> Findings {
    let mut findings = Findings::default();
    for line in text.split('\n').filter(|line| !is_comment(line)) {
        let Some(command) = words(line).ok().and_then(|w| w.into_iter().next()) else {
            continue;
        };
        let reason = match command.to_uppercase().as_str() {
            "FLUSHALL" => "FLUSHALL empties every database",
            "FLUSHDB" => "FLUSHDB empties the database",
            "SHUTDOWN" => "SHUTDOWN stops the server",
            _ => continue,
        };
        findings.writes = true;
        findings.destructive.push(reason.to_string());
    }
    findings
}

/// When the cursor ends the first word of a line, where that word starts
/// and what of it is typed: a command name to complete.
pub fn command_at(text: &str, cursor: Pos) -> Option<(Pos, String)> {
    let line = text.split('\n').nth(cursor.0)?;
    if is_comment(line) {
        return None;
    }
    let before: String = line.chars().take(cursor.1).collect();
    let typed = before.trim_start();
    if typed.chars().any(char::is_whitespace) || typed.starts_with(['"', '\'']) {
        return None;
    }
    let start = before.chars().count() - typed.chars().count();
    Some(((cursor.0, start), typed.to_string()))
}

/// The command that shows the value of `key`, a key of `key_type` as
/// `TYPE` names it: its first `first` items, or all of them.
pub fn open_command(key_type: &str, key: &str, first: Option<usize>) -> String {
    let key = quote(key);
    let last = first.map_or("-1".to_string(), |n| n.saturating_sub(1).to_string());
    match key_type {
        "string" => format!("GET {key}"),
        // shortcut: hashes and sets come whole, the row limit trims them on
        // this side; page with HSCAN and SSCAN for huge ones.
        "hash" => format!("HGETALL {key}"),
        "set" => format!("SMEMBERS {key}"),
        "list" => format!("LRANGE {key} 0 {last}"),
        "zset" => format!("ZRANGE {key} 0 {last} WITHSCORES"),
        "stream" => match first {
            Some(n) => format!("XRANGE {key} - + COUNT {n}"),
            None => format!("XRANGE {key} - +"),
        },
        "ReJSON-RL" => format!("JSON.GET {key}"),
        _ => format!("TYPE {key}"),
    }
}

/// Commands whose second word is a subcommand: `CLIENT LIST`, `CONFIG GET`.
pub const CONTAINERS: &[&str] = &[
    "ACL", "CLIENT", "CLUSTER", "COMMAND", "CONFIG", "DEBUG", "FUNCTION", "LATENCY", "MEMORY",
    "MODULE", "OBJECT", "PUBSUB", "SCRIPT", "SLOWLOG", "XGROUP", "XINFO",
];

/// Commands, for colouring and completion.
pub const COMMANDS: &[&str] = &[
    // Keys
    "COPY",
    "DEL",
    "DUMP",
    "EXISTS",
    "EXPIRE",
    "EXPIREAT",
    "EXPIRETIME",
    "KEYS",
    "MOVE",
    "OBJECT",
    "PERSIST",
    "PEXPIRE",
    "PEXPIREAT",
    "PEXPIRETIME",
    "PTTL",
    "RANDOMKEY",
    "RENAME",
    "RENAMENX",
    "RESTORE",
    "SCAN",
    "SORT",
    "SORT_RO",
    "TOUCH",
    "TTL",
    "TYPE",
    "UNLINK",
    "WAIT",
    // Strings
    "APPEND",
    "DECR",
    "DECRBY",
    "GET",
    "GETDEL",
    "GETEX",
    "GETRANGE",
    "GETSET",
    "INCR",
    "INCRBY",
    "INCRBYFLOAT",
    "LCS",
    "MGET",
    "MSET",
    "MSETNX",
    "PSETEX",
    "SET",
    "SETEX",
    "SETNX",
    "SETRANGE",
    "STRLEN",
    "SUBSTR",
    // Hashes
    "HDEL",
    "HEXISTS",
    "HEXPIRE",
    "HGET",
    "HGETALL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HMSET",
    "HPERSIST",
    "HRANDFIELD",
    "HSCAN",
    "HSET",
    "HSETNX",
    "HSTRLEN",
    "HTTL",
    "HVALS",
    // Lists
    "BLMOVE",
    "BLMPOP",
    "BLPOP",
    "BRPOP",
    "BRPOPLPUSH",
    "LINDEX",
    "LINSERT",
    "LLEN",
    "LMOVE",
    "LMPOP",
    "LPOP",
    "LPOS",
    "LPUSH",
    "LPUSHX",
    "LRANGE",
    "LREM",
    "LSET",
    "LTRIM",
    "RPOP",
    "RPOPLPUSH",
    "RPUSH",
    "RPUSHX",
    // Sets
    "SADD",
    "SCARD",
    "SDIFF",
    "SDIFFSTORE",
    "SINTER",
    "SINTERCARD",
    "SINTERSTORE",
    "SISMEMBER",
    "SMEMBERS",
    "SMISMEMBER",
    "SMOVE",
    "SPOP",
    "SRANDMEMBER",
    "SREM",
    "SSCAN",
    "SUNION",
    "SUNIONSTORE",
    // Sorted sets
    "BZMPOP",
    "BZPOPMAX",
    "BZPOPMIN",
    "ZADD",
    "ZCARD",
    "ZCOUNT",
    "ZDIFF",
    "ZDIFFSTORE",
    "ZINCRBY",
    "ZINTER",
    "ZINTERCARD",
    "ZINTERSTORE",
    "ZLEXCOUNT",
    "ZMPOP",
    "ZMSCORE",
    "ZPOPMAX",
    "ZPOPMIN",
    "ZRANDMEMBER",
    "ZRANGE",
    "ZRANGEBYLEX",
    "ZRANGEBYSCORE",
    "ZRANGESTORE",
    "ZRANK",
    "ZREM",
    "ZREMRANGEBYLEX",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREVRANGE",
    "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE",
    "ZREVRANK",
    "ZSCAN",
    "ZSCORE",
    "ZUNION",
    "ZUNIONSTORE",
    // Streams
    "XACK",
    "XADD",
    "XAUTOCLAIM",
    "XCLAIM",
    "XDEL",
    "XGROUP",
    "XINFO",
    "XLEN",
    "XPENDING",
    "XRANGE",
    "XREAD",
    "XREADGROUP",
    "XREVRANGE",
    "XSETID",
    "XTRIM",
    // Bitmaps, HyperLogLog, geo
    "BITCOUNT",
    "BITFIELD",
    "BITFIELD_RO",
    "BITOP",
    "BITPOS",
    "GETBIT",
    "SETBIT",
    "PFADD",
    "PFCOUNT",
    "PFMERGE",
    "GEOADD",
    "GEODIST",
    "GEOHASH",
    "GEOPOS",
    "GEOSEARCH",
    "GEOSEARCHSTORE",
    // Pub/sub, scripting, transactions
    "PUBLISH",
    "PUBSUB",
    "EVAL",
    "EVALSHA",
    "EVAL_RO",
    "EVALSHA_RO",
    "FCALL",
    "FCALL_RO",
    "FUNCTION",
    "SCRIPT",
    "DISCARD",
    "EXEC",
    "MULTI",
    "UNWATCH",
    "WATCH",
    // Connection and server
    "ACL",
    "AUTH",
    "BGREWRITEAOF",
    "BGSAVE",
    "CLIENT",
    "CLUSTER",
    "COMMAND",
    "CONFIG",
    "DBSIZE",
    "DEBUG",
    "ECHO",
    "FLUSHALL",
    "FLUSHDB",
    "HELLO",
    "INFO",
    "LASTSAVE",
    "LATENCY",
    "LOLWUT",
    "MEMORY",
    "MODULE",
    "PING",
    "ROLE",
    "SAVE",
    "SELECT",
    "SHUTDOWN",
    "SLOWLOG",
    "SWAPDB",
    "TIME",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    #[test]
    fn words_split_like_redis_cli() {
        assert_eq!(
            words(r#"SET "a key" 'it\'s' plain"#),
            Ok(vec![
                "SET".into(),
                "a key".into(),
                "it's".into(),
                "plain".into()
            ])
        );
        assert_eq!(
            words(r#"SET k "line\nbreak \"q\" \x41""#),
            Ok(vec!["SET".into(), "k".into(), "line\nbreak \"q\" A".into()])
        );
        assert_eq!(words("  "), Ok(vec![]));
        assert!(words(r#"GET "open"#).is_err());
        assert!(words(r#"GET "a"b"#).is_err());
    }

    #[test]
    fn quoting_round_trips() {
        for word in ["user:1", "a key", "it's", "back\\slash", "new\nline", ""] {
            assert_eq!(words(&quote(word)), Ok(vec![word.to_string()]), "{word:?}");
        }
        assert_eq!(quote("user:1"), "user:1");
        assert_eq!(quote("a key"), "\"a key\"");
    }

    #[test]
    fn each_line_is_a_command() {
        let text = "# setup\nSET a 1\n\n  GET a  \n";
        let all = statements(text);
        assert_eq!(all.len(), 2);
        assert_eq!((all[1].start, all[1].end), ((3, 2), (3, 7)));
        assert_eq!(statement_at(text, (2, 0)).map(|s| s.start.0), Some(1));
        assert_eq!(statement_at(text, (3, 4)).map(|s| s.start.0), Some(3));
        assert_eq!(statement_at(text, (0, 0)).map(|s| s.start.0), Some(1));
    }

    #[test]
    fn commands_strings_and_numbers_have_colours() {
        let pieces = highlight(&lines(
            "zadd scores 1.5 \"bob\"\nCONFIG GET maxmemory\n# hi",
        ));
        let kinds = |line: &[Piece]| line.iter().map(|p| (p.start, p.kind)).collect::<Vec<_>>();
        assert_eq!(
            kinds(&pieces[0]),
            [(0, Kind::Keyword), (12, Kind::Number), (16, Kind::String)]
        );
        assert_eq!(kinds(&pieces[1]), [(0, Kind::Keyword), (7, Kind::Keyword)]);
        assert_eq!(kinds(&pieces[2]), [(0, Kind::Comment)]);
        assert!(highlight(&lines("NOTACOMMAND x"))[0].is_empty());
    }

    #[test]
    fn flushing_and_shutdown_ask_first() {
        let findings = check("GET a\nflushdb\n# FLUSHALL");
        assert_eq!(findings.destructive, ["FLUSHDB empties the database"]);
        assert!(check("DEL a").destructive.is_empty());
    }

    #[test]
    fn each_type_opens_with_its_own_command() {
        assert_eq!(open_command("list", "jobs", Some(100)), "LRANGE jobs 0 99");
        assert_eq!(open_command("list", "jobs", None), "LRANGE jobs 0 -1");
        assert_eq!(
            open_command("zset", "a b", Some(10)),
            "ZRANGE \"a b\" 0 9 WITHSCORES"
        );
        assert_eq!(open_command("stream", "s", Some(5)), "XRANGE s - + COUNT 5");
        assert_eq!(open_command("hash", "user:1", Some(5)), "HGETALL user:1");
        assert_eq!(open_command("vectorset", "v", None), "TYPE v");
    }

    #[test]
    fn the_first_word_completes_as_a_command() {
        assert_eq!(
            command_at("GET a\n  hg", (1, 4)),
            Some(((1, 2), "hg".into()))
        );
        assert_eq!(command_at("GET a", (0, 5)), None);
        assert_eq!(command_at("# GE", (0, 4)), None);
    }
}
