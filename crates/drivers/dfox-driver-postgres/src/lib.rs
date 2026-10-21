//! PostgreSQL for dfox: a native session on `tokio-postgres` ([`native`]).

pub mod native;

use dfox_core::{
    driver::{server_form, Driver, DriverInfo, FormSpec},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};
use futures_util::future::BoxFuture;

/// PostgreSQL and compatible servers.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresDriver;

impl Driver for PostgresDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "postgres",
            name: "PostgreSQL",
            default_port: Some(5432),
            url_schemes: &["postgres", "postgresql"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        server_form(5432)
    }

    fn db_type(&self) -> DbType {
        DbType::Postgres
    }

    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(native::open(params))
    }
}
