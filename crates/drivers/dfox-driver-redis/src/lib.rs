//! Redis and Valkey for dfox: a native session on redis-rs ([`native`]),
//! running Redis commands and showing replies as tables.

pub mod native;

use dfox_core::{
    driver::{server_form, Driver, DriverInfo, FormSpec},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};
use futures_util::future::BoxFuture;

/// Redis servers, and Valkey, which speaks the same.
#[derive(Debug, Clone, Copy, Default)]
pub struct RedisDriver;

impl Driver for RedisDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "redis",
            name: "Redis / Valkey",
            default_port: Some(6379),
            url_schemes: &["redis", "rediss", "valkey", "valkeys"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        server_form(6379)
    }

    fn db_type(&self) -> DbType {
        DbType::Redis
    }

    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(native::open(params))
    }
}
