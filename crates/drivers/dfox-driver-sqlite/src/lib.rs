//! SQLite for dfox: a native session on `rusqlite`, on a thread of its own
//! ([`native`]).

pub mod native;

use dfox_core::{
    driver::{Driver, DriverInfo, FieldKind, FormField, FormSpec},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};
use futures_util::future::BoxFuture;

/// SQLite files.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteDriver;

impl Driver for SqliteDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "sqlite",
            name: "SQLite",
            default_port: None,
            url_schemes: &["sqlite"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        FormSpec {
            fields: vec![
                FormField {
                    key: "database",
                    label: "File",
                    kind: FieldKind::Path,
                    default: None,
                    required: true,
                },
                FormField {
                    key: "read_only",
                    label: "Read-only",
                    kind: FieldKind::Bool,
                    default: Some("false".to_string()),
                    required: false,
                },
            ],
        }
    }

    fn db_type(&self) -> DbType {
        DbType::Sqlite
    }

    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(native::open(params))
    }
}
