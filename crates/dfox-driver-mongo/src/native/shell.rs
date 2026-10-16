//! Mongo shell commands, read from what the user typed: `db.<collection>.
//! <method>(…)` with cursor methods chained after, `db.<method>(…)`, and
//! `show dbs`, `show collections` and `use <db>`. Several may follow one
//! another, on lines of their own or after `;`.

use mongodb::bson::Bson;

use super::literal::{ParseError, Reader};

/// A call: a method's name and its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Vec<Bson>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    ShowDatabases,
    ShowCollections,
    /// Switch to another database for the commands that follow.
    Use(String),
    /// A method of the database: `db.runCommand({…})`.
    Database(Call),
    /// A method of a collection, and the calls chained after it:
    /// `db.users.find({…}).sort({…}).limit(10)`.
    Collection {
        collection: String,
        call: Call,
        then: Vec<Call>,
    },
}

/// Reads every command of `text`.
pub fn parse(text: &str) -> Result<Vec<Command>, ParseError> {
    let mut reader = Reader::new(text);
    let mut commands = Vec::new();
    while !reader.at_end() {
        if reader.eat(';') {
            continue;
        }
        commands.push(command(&mut reader)?);
    }
    Ok(commands)
}

fn command(reader: &mut Reader) -> Result<Command, ParseError> {
    let start = reader.pos;
    let word = reader.identifier();
    match word.as_deref() {
        Some("show") => {
            let what = reader.rest_of_line();
            match what.as_str() {
                "dbs" | "databases" => Ok(Command::ShowDatabases),
                "collections" | "tables" => Ok(Command::ShowCollections),
                _ => reader.error(format!(
                    "dfox can't show {what:?}: try `show dbs` or `show collections`"
                )),
            }
        }
        Some("use") => {
            let name = reader.rest_of_line();
            let name = name.trim_matches(|c| c == '"' || c == '\'');
            if name.is_empty() {
                return reader.error("use which database?");
            }
            Ok(Command::Use(name.to_string()))
        }
        Some("db") => database(reader),
        _ => {
            reader.pos = start;
            reader.error("dfox runs shell commands: db.<collection>.<method>(…), db.<method>(…), show or use")
        }
    }
}

/// What follows `db`.
fn database(reader: &mut Reader) -> Result<Command, ParseError> {
    // `db["name"]` or `db.getCollection("name")` for awkward names.
    if reader.eat('[') {
        let name = match reader.value()? {
            Bson::String(name) => name,
            _ => return reader.error("expected a collection's name"),
        };
        reader.expect(']')?;
        return collection(reader, name);
    }
    // A dotted collection name runs up to the first name called as a method.
    let mut names = Vec::new();
    loop {
        reader.expect('.')?;
        let Some(name) = reader.identifier() else {
            return reader.error("expected a name after `.`");
        };
        reader.skip_space();
        if reader.peek() == Some('(') {
            reader.pos += 1;
            let args = reader.values_until(')')?;
            let call = Call { name, args };
            if names.is_empty() {
                if call.name == "getCollection" {
                    let Some(Bson::String(collection_name)) = call.args.first().cloned() else {
                        return reader.error("getCollection needs a name");
                    };
                    return self::collection(reader, collection_name);
                }
                return Ok(Command::Database(call));
            }
            let then = chained(reader)?;
            return Ok(Command::Collection {
                collection: names.join("."),
                call,
                then,
            });
        }
        names.push(name);
    }
}

/// A collection's method after its name.
fn collection(reader: &mut Reader, name: String) -> Result<Command, ParseError> {
    reader.expect('.')?;
    let Some(method) = reader.identifier() else {
        return reader.error("expected a method");
    };
    reader.expect('(')?;
    let args = reader.values_until(')')?;
    let then = chained(reader)?;
    Ok(Command::Collection {
        collection: name,
        call: Call { name: method, args },
        then,
    })
}

/// `.sort({…}).limit(10)`: calls chained after a method.
fn chained(reader: &mut Reader) -> Result<Vec<Call>, ParseError> {
    let mut calls = Vec::new();
    loop {
        let before = reader.pos;
        if !reader.eat('.') {
            reader.pos = before;
            return Ok(calls);
        }
        let Some(name) = reader.identifier() else {
            return reader.error("expected a method after `.`");
        };
        reader.expect('(')?;
        let args = reader.values_until(')')?;
        calls.push(Call { name, args });
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{bson, doc};

    use super::*;

    fn call(name: &str, args: Vec<Bson>) -> Call {
        Call {
            name: name.into(),
            args,
        }
    }

    #[test]
    fn scripts_read_as_commands() {
        let script = "show dbs\nuse shop;\nshow collections\n\
                      db.users.find({ age: { $gt: 30 } }, { name: 1 })\n  .sort({ name: -1 })\n  .limit(10);\n\
                      db.system.views.find()\n\
                      db['odd name'].countDocuments({})\n\
                      db.getCollection(\"other\").insertOne({ a: 1 })\n\
                      db.runCommand({ ping: 1 })";
        let commands = parse(script).expect("parse");
        assert_eq!(
            commands,
            [
                Command::ShowDatabases,
                Command::Use("shop".into()),
                Command::ShowCollections,
                Command::Collection {
                    collection: "users".into(),
                    call: call(
                        "find",
                        vec![bson!({ "age": { "$gt": 30 } }), bson!({ "name": 1 })]
                    ),
                    then: vec![
                        call("sort", vec![bson!({ "name": -1 })]),
                        call("limit", vec![Bson::Int32(10)])
                    ],
                },
                Command::Collection {
                    collection: "system.views".into(),
                    call: call("find", vec![]),
                    then: vec![],
                },
                Command::Collection {
                    collection: "odd name".into(),
                    call: call("countDocuments", vec![bson!({})]),
                    then: vec![],
                },
                Command::Collection {
                    collection: "other".into(),
                    call: call("insertOne", vec![Bson::Document(doc! { "a": 1 })]),
                    then: vec![],
                },
                Command::Database(call("runCommand", vec![bson!({ "ping": 1 })])),
            ]
        );
    }

    #[test]
    fn mistakes_say_what_was_expected() {
        let err = |text| parse(text).expect_err("an error").to_string();
        assert!(err("SELECT 1").starts_with("dfox runs shell commands"));
        assert!(err("db.users").contains("expected `.`"));
        assert!(err("db.users.find({)").contains("expected a key"));
        assert!(err("show users").contains("can't show"));
        assert!(err("use").contains("use which database"));
        assert_eq!(parse("// nothing\n;;"), Ok(vec![]));
    }
}
