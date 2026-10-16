//! MySQL and MariaDB for dfox: a native session on `mysql_async`
//! ([`native`]).

pub mod native;

use dfox_core::{
    driver::{server_form, Driver, DriverInfo, FormSpec},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};
use futures_util::future::BoxFuture;

/// MySQL and MariaDB.
#[derive(Debug, Clone, Copy, Default)]
pub struct MySqlDriver;

impl Driver for MySqlDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "mysql",
            name: "MySQL",
            default_port: Some(3306),
            url_schemes: &["mysql", "mariadb"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        server_form(3306)
    }

    fn db_type(&self) -> DbType {
        DbType::MySql
    }

    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        Box::pin(native::open(params))
    }
}
