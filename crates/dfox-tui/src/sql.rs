//! SQL reading, from `dfox-sql`, in the dialect of a connection's database.

pub use dfox_sql::*;

use dfox_core::models::connections::DbType;

/// The SQL dialect a database speaks.
pub fn dialect(driver: DbType) -> Dialect {
    match driver {
        DbType::Postgres => Dialect::Postgres,
        DbType::MySql => Dialect::MySql,
        DbType::Sqlite => Dialect::Sqlite,
    }
}
