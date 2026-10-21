//! Cassandra and ScyllaDB for dfox: a native session on the `scylla` driver
//! ([`native`]), running CQL and showing rows as Arrow columns.

pub mod native;

use dfox_core::{
    driver::{server_form, Driver, DriverInfo, FormSpec},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};
use futures_util::future::BoxFuture;

/// Cassandra and ScyllaDB clusters.
#[derive(Debug, Clone, Copy, Default)]
pub struct CassandraDriver;

impl Driver for CassandraDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "cassandra",
            name: "Cassandra / ScyllaDB",
            default_port: Some(9042),
            url_schemes: &["cassandra", "scylla"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        server_form(9042)
    }

    fn db_type(&self) -> DbType {
        DbType::Cassandra
    }

    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(native::open(params))
    }
}
