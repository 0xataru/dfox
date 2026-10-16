//! Values as the mongo shell writes them: JavaScript object literals with
//! unquoted keys and either quote, plus the shell's helpers (`ObjectId(…)`,
//! `ISODate(…)`, `NumberLong(…)`, `/regex/i`, …), read into BSON.

use std::str::FromStr;

use mongodb::bson::{
    oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Decimal128, Document, Regex,
    Timestamp,
};

/// Where reading stopped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Characters into the text.
    pub at: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at character {})", self.message, self.at + 1)
    }
}

/// Reads values from text, one character at a time.
pub struct Reader {
    chars: Vec<char>,
    pub pos: usize,
}

impl Reader {
    pub fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
        }
    }

    pub fn error<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            at: self.pos,
            message: message.into(),
        })
    }

    pub fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    pub fn at_end(&mut self) -> bool {
        self.skip_space();
        self.pos >= self.chars.len()
    }

    /// Skips whitespace and `//` and `/* */` comments.
    pub fn skip_space(&mut self) {
        loop {
            match (self.peek(), self.peek_at(1)) {
                (Some(c), _) if c.is_whitespace() => self.pos += 1,
                (Some('/'), Some('/')) => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                (Some('/'), Some('*')) => {
                    self.pos += 2;
                    while self.peek().is_some()
                        && !(self.peek() == Some('*') && self.peek_at(1) == Some('/'))
                    {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.chars.len());
                }
                _ => return,
            }
        }
    }

    /// Takes `c` after any space, or fails.
    pub fn expect(&mut self, c: char) -> Result<(), ParseError> {
        self.skip_space();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            self.error(format!("expected `{c}`"))
        }
    }

    /// Takes `c` after any space when it is next.
    pub fn eat(&mut self, c: char) -> bool {
        self.skip_space();
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// A name: letters, digits, `_` and `$`, not starting with a digit.
    pub fn identifier(&mut self) -> Option<String> {
        self.skip_space();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            if self.pos == start && self.peek().is_some_and(|c| c.is_ascii_digit()) {
                break;
            }
            self.pos += 1;
        }
        (self.pos > start).then(|| self.chars[start..self.pos].iter().collect())
    }

    /// The rest of the current line, trimmed.
    pub fn rest_of_line(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(|c| c != '\n' && c != ';') {
            self.pos += 1;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    }

    /// Values separated by commas, up to `close`.
    pub fn values_until(&mut self, close: char) -> Result<Vec<Bson>, ParseError> {
        let mut values = Vec::new();
        loop {
            if self.eat(close) {
                return Ok(values);
            }
            values.push(self.value()?);
            if !self.eat(',') {
                self.expect(close)?;
                return Ok(values);
            }
        }
    }

    pub fn value(&mut self) -> Result<Bson, ParseError> {
        self.skip_space();
        match self.peek() {
            Some('{') => {
                self.pos += 1;
                self.document_rest().map(Bson::Document)
            }
            Some('[') => {
                self.pos += 1;
                self.values_until(']').map(Bson::Array)
            }
            Some(q @ ('"' | '\'')) => self.string(q).map(Bson::String),
            Some('/') => self.regex(),
            Some(c) if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' => self.number(),
            Some(_) => self.word(),
            None => self.error("expected a value"),
        }
    }

    /// A document after its `{`.
    fn document_rest(&mut self) -> Result<Document, ParseError> {
        let mut doc = Document::new();
        loop {
            if self.eat('}') {
                return Ok(doc);
            }
            self.skip_space();
            let key = match self.peek() {
                Some(q @ ('"' | '\'')) => self.string(q)?,
                Some(c) if c.is_ascii_digit() => {
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.pos += 1;
                    }
                    self.chars[start..self.pos].iter().collect()
                }
                _ => match self.identifier() {
                    Some(key) => key,
                    None => return self.error("expected a key"),
                },
            };
            self.expect(':')?;
            let value = self.value()?;
            doc.insert(key, value);
            if !self.eat(',') {
                self.expect('}')?;
                return Ok(doc);
            }
        }
    }

    fn string(&mut self, quote: char) -> Result<String, ParseError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return self.error("the string never ends"),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.pos += 1;
                    let escaped = match self.peek() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('b') => '\u{8}',
                        Some('f') => '\u{c}',
                        Some('0') => '\0',
                        Some('u') => {
                            let hex: String =
                                self.chars.iter().skip(self.pos + 1).take(4).collect();
                            let code = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                            match code {
                                Some(c) if hex.len() == 4 => {
                                    self.pos += 4;
                                    c
                                }
                                _ => return self.error("a bad \\u escape"),
                            }
                        }
                        Some(c) => c,
                        None => return self.error("the string never ends"),
                    };
                    out.push(escaped);
                    self.pos += 1;
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn regex(&mut self) -> Result<Bson, ParseError> {
        self.pos += 1;
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            match self.peek() {
                None | Some('\n') => return self.error("the regular expression never ends"),
                Some('\\') => {
                    pattern.push('\\');
                    self.pos += 1;
                    if let Some(c) = self.peek() {
                        pattern.push(c);
                        self.pos += 1;
                    }
                }
                Some('/') if !in_class => {
                    self.pos += 1;
                    break;
                }
                Some(c) => {
                    match c {
                        '[' => in_class = true,
                        ']' => in_class = false,
                        _ => {}
                    }
                    pattern.push(c);
                    self.pos += 1;
                }
            }
        }
        let mut options = String::new();
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            options.extend(self.peek());
            self.pos += 1;
        }
        let mut sorted: Vec<char> = options.chars().collect();
        sorted.sort_unstable();
        Ok(Bson::RegularExpression(Regex {
            pattern: pattern
                .try_into()
                .map_err(|_| self.error_value("a NUL in the pattern"))?,
            options: sorted
                .into_iter()
                .collect::<String>()
                .try_into()
                .map_err(|_| self.error_value("a NUL in the options"))?,
        }))
    }

    fn error_value(&self, message: &str) -> ParseError {
        ParseError {
            at: self.pos,
            message: message.to_string(),
        }
    }

    fn number(&mut self) -> Result<Bson, ParseError> {
        let start = self.pos;
        if matches!(self.peek(), Some('-' | '+')) {
            self.pos += 1;
        }
        // `-Infinity`
        if self.peek() == Some('I') {
            return match self.identifier().as_deref() {
                Some("Infinity") if self.chars[start] == '-' => Ok(Bson::Double(f64::NEG_INFINITY)),
                Some("Infinity") => Ok(Bson::Double(f64::INFINITY)),
                _ => self.error("expected a number"),
            };
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            || (matches!(self.peek(), Some('-' | '+'))
                && matches!(self.chars.get(self.pos - 1), Some('e' | 'E')))
        {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos]
            .iter()
            .filter(|c| **c != '_')
            .collect();
        let float = text.contains(['.', 'e', 'E']) && !text.starts_with("0x");
        if !float {
            let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => i64::from_str_radix(hex, 16).ok(),
                None => text.parse::<i64>().ok(),
            };
            if let Some(n) = parsed {
                return Ok(match i32::try_from(n) {
                    // The shell reads numbers as doubles; whole ones that
                    // fit are stored as 32-bit integers by most drivers.
                    Ok(n) => Bson::Int32(n),
                    Err(_) => Bson::Int64(n),
                });
            }
        }
        match text.parse::<f64>() {
            Ok(n) => Ok(Bson::Double(n)),
            Err(_) => {
                self.pos = start;
                self.error(format!("{text} is not a number"))
            }
        }
    }

    /// `true`, `null`, or a helper call such as `ObjectId("…")`.
    fn word(&mut self) -> Result<Bson, ParseError> {
        let start = self.pos;
        let Some(mut name) = self.identifier() else {
            return self.error("expected a value");
        };
        if name == "new" {
            match self.identifier() {
                Some(next) => name = next,
                None => return self.error("expected a name after `new`"),
            }
        }
        match name.as_str() {
            "true" => return Ok(Bson::Boolean(true)),
            "false" => return Ok(Bson::Boolean(false)),
            "null" => return Ok(Bson::Null),
            "undefined" => return Ok(Bson::Undefined),
            "NaN" => return Ok(Bson::Double(f64::NAN)),
            "Infinity" => return Ok(Bson::Double(f64::INFINITY)),
            "MinKey" => return Ok(Bson::MinKey),
            "MaxKey" => return Ok(Bson::MaxKey),
            _ => {}
        }
        if name == "function" {
            self.pos = start;
            return self.error("dfox reads values, not JavaScript functions");
        }
        if !self.eat('(') {
            self.pos = start;
            return self.error(format!("`{name}` is not a value the shell knows"));
        }
        let args = self.values_until(')')?;
        let at = self.pos;
        let fail = |message: String| ParseError { at, message };
        let text = |i: usize| match args.get(i) {
            Some(Bson::String(s)) => Ok(s.clone()),
            Some(other) => Ok(other.to_string()),
            None => Err(fail(format!("{name}() needs a value"))),
        };
        match name.as_str() {
            "ObjectId" => match args.first() {
                None => Ok(Bson::ObjectId(ObjectId::new())),
                Some(_) => ObjectId::parse_str(text(0)?)
                    .map(Bson::ObjectId)
                    .map_err(|err| fail(format!("ObjectId: {err}"))),
            },
            "ISODate" | "Date" => match args.first() {
                None => Ok(Bson::DateTime(DateTime::now())),
                Some(Bson::Int32(ms)) => Ok(Bson::DateTime(DateTime::from_millis(i64::from(*ms)))),
                Some(Bson::Int64(ms)) => Ok(Bson::DateTime(DateTime::from_millis(*ms))),
                Some(Bson::Double(ms)) => Ok(Bson::DateTime(DateTime::from_millis(*ms as i64))),
                Some(_) => parse_date(&text(0)?)
                    .map(Bson::DateTime)
                    .ok_or_else(|| fail(format!("{} is not a date", text(0).unwrap_or_default()))),
            },
            "NumberLong" | "Long" => text(0)?
                .parse::<i64>()
                .map(Bson::Int64)
                .map_err(|err| fail(format!("NumberLong: {err}"))),
            "NumberInt" | "Int32" => text(0)?
                .parse::<i32>()
                .map(Bson::Int32)
                .map_err(|err| fail(format!("NumberInt: {err}"))),
            "Double" => text(0)?
                .parse::<f64>()
                .map(Bson::Double)
                .map_err(|err| fail(format!("Double: {err}"))),
            "NumberDecimal" | "Decimal128" => Decimal128::from_str(&text(0)?)
                .map(Bson::Decimal128)
                .map_err(|err| fail(format!("NumberDecimal: {err}"))),
            "UUID" => {
                let hex: String = text(0)?.chars().filter(|c| *c != '-').collect();
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("x"), 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .ok()
                    .filter(|b| b.len() == 16)
                    .ok_or_else(|| fail("UUID needs 32 hex digits".into()))?;
                Ok(Bson::Binary(Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes,
                }))
            }
            "Timestamp" => {
                let (time, increment) = match args.as_slice() {
                    [Bson::Document(d)] => (d.get("t").cloned(), d.get("i").cloned()),
                    [t, i] => (Some(t.clone()), Some(i.clone())),
                    _ => (None, None),
                };
                let number = |v: Option<Bson>| match v {
                    Some(Bson::Int32(n)) => u32::try_from(n).ok(),
                    Some(Bson::Int64(n)) => u32::try_from(n).ok(),
                    _ => None,
                };
                match (number(time), number(increment)) {
                    (Some(time), Some(increment)) => {
                        Ok(Bson::Timestamp(Timestamp { time, increment }))
                    }
                    _ => Err(fail("Timestamp needs a time and an increment".into())),
                }
            }
            "RegExp" => Ok(Bson::RegularExpression(Regex {
                pattern: text(0)?
                    .try_into()
                    .map_err(|_| fail("a NUL in the pattern".into()))?,
                options: match args.get(1) {
                    Some(Bson::String(o)) => o.clone(),
                    _ => String::new(),
                }
                .try_into()
                .map_err(|_| fail("a NUL in the options".into()))?,
            })),
            other => {
                self.pos = start;
                self.error(format!("`{other}()` is not a value the shell knows"))
            }
        }
    }
}

/// `2024-01-02`, `2024-01-02T03:04:05Z` and the like.
fn parse_date(text: &str) -> Option<DateTime> {
    if let Ok(date) = DateTime::parse_rfc3339_str(text) {
        return Some(date);
    }
    // Without a zone the shell reads UTC; without a time, midnight.
    let with_time = if text.len() == 10 {
        format!("{text}T00:00:00Z")
    } else {
        format!("{}Z", text.trim_end_matches('Z'))
    };
    DateTime::parse_rfc3339_str(with_time).ok()
}

/// Reads one value, the whole of `text`.
pub fn parse(text: &str) -> Result<Bson, ParseError> {
    let mut reader = Reader::new(text);
    let value = reader.value()?;
    if !reader.at_end() {
        return reader.error("unexpected text after the value");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;

    use super::*;

    #[test]
    fn shell_literals_read_as_bson() {
        let value = parse(
            r#"{ name: 'Ada', "age": 36, score: 9.5, big: 3000000000, tags: ['a', "b",],
                 nested: { ok: true, none: null }, $or: [{ x: { $gt: -1 } }], 'a.b': 1e3,
                 // a comment
                 id: ObjectId("64b7f0c2a1b2c3d4e5f60718"), at: ISODate("2024-01-02T03:04:05Z"),
                 day: new Date("2024-01-02"), n: NumberLong("9007199254740993"), i: NumberInt(7),
                 d: NumberDecimal("1.10"), re: /^ab[/c]+/i, hex: 0x1F, }"#,
        )
        .expect("parse");
        let expected = doc! {
            "name": "Ada",
            "age": 36,
            "score": 9.5,
            "big": 3_000_000_000_i64,
            "tags": ["a", "b"],
            "nested": { "ok": true, "none": null },
            "$or": [{ "x": { "$gt": -1 } }],
            "a.b": 1000.0,
            "id": ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").expect("oid"),
            "at": DateTime::parse_rfc3339_str("2024-01-02T03:04:05Z").expect("date"),
            "day": DateTime::parse_rfc3339_str("2024-01-02T00:00:00Z").expect("date"),
            "n": 9_007_199_254_740_993_i64,
            "i": 7,
            "d": Decimal128::from_str("1.10").expect("decimal"),
            "re": Regex { pattern: "^ab[/c]+".try_into().expect("pattern"), options: "i".try_into().expect("options") },
            "hex": 31,
        };
        assert_eq!(value, Bson::Document(expected));
    }

    #[test]
    fn mistakes_say_where() {
        let err = parse("{ a: 1, b: }").expect_err("an error");
        assert_eq!(err.to_string(), "expected a value (at character 12)");
        assert!(parse("{ a: 'open }").is_err());
        assert!(parse("{ a: function() {} }")
            .expect_err("no functions")
            .message
            .contains("not JavaScript functions"));
        assert!(parse("ObjectId('nope')").is_err());
        assert!(parse("{ a: 1 } extra").is_err());
        assert_eq!(parse("-Infinity"), Ok(Bson::Double(f64::NEG_INFINITY)));
        let half = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
        assert_eq!(
            parse("UUID('01234567-89ab-cdef-0123-456789abcdef')"),
            Ok(Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: [half, half].concat(),
            }))
        );
    }
}
