//! Edits from the grid, written to a collection: documents found by `_id`,
//! fields set, documents inserted and deleted. MongoDB has no schema to cast
//! to, so a changed field keeps the type it has; a new field or document is
//! read as the mongo shell reads values: `42` a number, `"42"` text, `{…}`
//! a document. Anything else is text.
//!
//! Each change is written on its own: a transaction would need a replica
//! set, which a single server isn't.

use std::str::FromStr;

use dfox_core::{
    errors::DbError,
    result::OwnedCell,
    session::{ApplyReport, Change, ChangeSet},
};
use mongodb::{
    bson::{doc, oid::ObjectId, Bson, Decimal128, Document},
    Client,
};

use super::{db_error, literal};

/// The filter that finds the document whose `_id` the grid shows as `key`.
/// The grid shows an ObjectId as its hex, so hex matches either.
fn id_filter(key: &[OwnedCell]) -> Result<Document, DbError> {
    let id = match key {
        [OwnedCell::Int(n)] => Bson::Int64(*n),
        [OwnedCell::Float(n)] => Bson::Double(*n),
        [OwnedCell::Text(text)] => match ObjectId::parse_str(text) {
            Ok(oid) => return Ok(doc! { "_id": { "$in": [oid, text.as_ref()] } }),
            Err(_) => Bson::String(text.to_string()),
        },
        _ => return Err(DbError::General("a document is found by its _id".into())),
    };
    Ok(doc! { "_id": id })
}

/// `text` as a value of the type `current` has; as the shell reads it when
/// there is none.
pub fn typed(field: &str, text: &str, current: Option<&Bson>) -> Result<Bson, DbError> {
    let wrong = |what: &str| DbError::General(format!("{field}: {text:?} is not {what}"));
    Ok(match current {
        Some(Bson::Int32(_)) => match text.trim().parse::<i32>() {
            Ok(n) => Bson::Int32(n),
            Err(_) => Bson::Int64(text.trim().parse().map_err(|_| wrong("a whole number"))?),
        },
        Some(Bson::Int64(_)) => {
            Bson::Int64(text.trim().parse().map_err(|_| wrong("a whole number"))?)
        }
        Some(Bson::Double(_)) => Bson::Double(text.trim().parse().map_err(|_| wrong("a number"))?),
        Some(Bson::Boolean(_)) => Bson::Boolean(match text.trim() {
            "true" => true,
            "false" => false,
            _ => return Err(wrong("true or false")),
        }),
        Some(Bson::String(_) | Bson::Symbol(_)) => Bson::String(text.to_string()),
        Some(Bson::ObjectId(_)) => {
            Bson::ObjectId(ObjectId::parse_str(text.trim()).map_err(|_| wrong("an ObjectId"))?)
        }
        Some(Bson::Decimal128(_)) => {
            Bson::Decimal128(Decimal128::from_str(text.trim()).map_err(|_| wrong("a decimal"))?)
        }
        // The grid shows dates as `2024-01-02 03:04:05`.
        Some(Bson::DateTime(_)) => Bson::DateTime(
            literal::parse_date(&text.trim().replacen(' ', "T", 1))
                .ok_or_else(|| wrong("a date"))?,
        ),
        Some(Bson::Document(_) | Bson::Array(_)) => match literal::parse(text) {
            Ok(value @ (Bson::Document(_) | Bson::Array(_))) => value,
            _ => return Err(wrong("a document or an array")),
        },
        // A new field, or a null one: as the shell reads it, else text.
        _ => guess(text),
    })
}

/// `text` as the shell reads a value, or as text when it doesn't read as
/// one. A number with a leading zero stays text: it is most likely a code.
fn guess(text: &str) -> Bson {
    let trimmed = text.trim();
    let leading_zero = trimmed.len() > 1 && trimmed.starts_with('0') && !trimmed.starts_with("0.");
    if leading_zero {
        return Bson::String(text.to_string());
    }
    literal::parse(trimmed).unwrap_or_else(|_| Bson::String(text.to_string()))
}

/// The value the grid sends for a field, as BSON of `current`'s type.
fn value(field: &str, cell: &OwnedCell, current: Option<&Bson>) -> Result<Bson, DbError> {
    match cell {
        OwnedCell::Null => Ok(Bson::Null),
        OwnedCell::Bool(b) => Ok(Bson::Boolean(*b)),
        OwnedCell::Int(n) => Ok(Bson::Int64(*n)),
        OwnedCell::Float(n) => Ok(Bson::Double(*n)),
        OwnedCell::Text(text) | OwnedCell::Json(text) | OwnedCell::Decimal(text) => {
            typed(field, text, current)
        }
        other => typed(field, &other.as_cell().to_string(), current),
    }
}

/// Writes `changes` to their collection, one change at a time.
pub async fn apply(client: &Client, changes: ChangeSet) -> Result<ApplyReport, DbError> {
    let [database, collection] = changes.table.parts() else {
        return Err(DbError::General("edits go to a collection".into()));
    };
    // Updates and deletes find documents by `_id`; inserts need no key.
    let finds = changes
        .changes
        .iter()
        .any(|c| !matches!(c, Change::Insert(_)));
    if finds && changes.key != ["_id"] {
        return Err(DbError::General("a document is found by its _id".into()));
    }
    let coll = client.database(database).collection::<Document>(collection);
    let mut report = ApplyReport::default();
    for change in &changes.changes {
        match change {
            Change::Update { key, set } => {
                let filter = id_filter(key)?;
                let current = coll
                    .find_one(filter.clone())
                    .await
                    .map_err(db_error)?
                    .unwrap_or_default();
                let mut fields = Document::new();
                for (field, cell) in set {
                    fields.insert(field.clone(), value(field, cell, current.get(field))?);
                }
                let done = coll
                    .update_one(filter, doc! { "$set": fields })
                    .await
                    .map_err(db_error)?;
                report.updated += done.matched_count;
            }
            Change::Insert(values) => {
                let mut document = Document::new();
                for (field, cell) in values {
                    document.insert(field.clone(), value(field, cell, None)?);
                }
                coll.insert_one(document).await.map_err(db_error)?;
                report.inserted += 1;
            }
            Change::Delete(key) => {
                let done = coll.delete_one(id_filter(key)?).await.map_err(db_error)?;
                report.deleted += done.deleted_count;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use mongodb::bson::DateTime;

    use super::*;

    #[test]
    fn a_changed_field_keeps_its_type() {
        let int = Some(&Bson::Int32(1));
        assert_eq!(typed("n", "42", int).ok(), Some(Bson::Int32(42)));
        assert_eq!(
            typed("n", "5000000000", int).ok(),
            Some(Bson::Int64(5_000_000_000))
        );
        assert!(typed("n", "abc", int)
            .unwrap_err()
            .to_string()
            .contains("whole number"));
        let text = Some(&Bson::String("x".into()));
        assert_eq!(
            typed("s", "124", text).ok(),
            Some(Bson::String("124".into()))
        );
        let date = Some(&Bson::DateTime(DateTime::from_millis(0)));
        assert_eq!(
            typed("at", "2024-01-02 03:04:05", date).ok(),
            Some(Bson::DateTime(
                DateTime::parse_rfc3339_str("2024-01-02T03:04:05Z").expect("date")
            ))
        );
        let object = Some(&Bson::Document(Document::new()));
        assert_eq!(
            typed("a", r#"{"city": "Kyiv"}"#, object).ok(),
            Some(Bson::Document(doc! { "city": "Kyiv" }))
        );
        assert!(typed("a", "Kyiv", object).is_err());
    }

    #[test]
    fn a_new_field_reads_as_the_shell_does() {
        assert_eq!(guess("42"), Bson::Int32(42));
        assert_eq!(guess("\"42\""), Bson::String("42".into()));
        assert_eq!(guess("Ann"), Bson::String("Ann".into()));
        assert_eq!(guess("007"), Bson::String("007".into()));
        assert_eq!(guess("0.5"), Bson::Double(0.5));
        assert_eq!(guess("true"), Bson::Boolean(true));
        assert_eq!(
            guess("[1, 2]"),
            Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)])
        );
    }

    #[test]
    fn hex_ids_match_object_ids_and_text() {
        let hex = "64b7f0c2a1b2c3d4e5f60718";
        let filter = id_filter(&[OwnedCell::Text(hex.into())]).expect("filter");
        let oid = ObjectId::parse_str(hex).expect("oid");
        assert_eq!(filter, doc! { "_id": { "$in": [oid, hex] } });
        assert_eq!(
            id_filter(&[OwnedCell::Text("ann".into())]).expect("filter"),
            doc! { "_id": "ann" }
        );
        assert_eq!(
            id_filter(&[OwnedCell::Int(7)]).expect("filter"),
            doc! { "_id": 7_i64 }
        );
    }
}
