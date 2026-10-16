//! MongoDB for dfox: a native session on the official driver ([`native`]),
//! running mongo shell commands and showing documents as Arrow columns.

pub mod native;

use dfox_core::{
    driver::{server_form, Driver, DriverInfo, FormSpec},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};
use futures_util::future::BoxFuture;

/// MongoDB servers.
#[derive(Debug, Clone, Copy, Default)]
pub struct MongoDriver;

impl Driver for MongoDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "mongodb",
            name: "MongoDB",
            default_port: Some(27017),
            url_schemes: &["mongodb"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        server_form(27017)
    }

    fn db_type(&self) -> DbType {
        DbType::Mongo
    }

    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(native::open(params))
    }
}
