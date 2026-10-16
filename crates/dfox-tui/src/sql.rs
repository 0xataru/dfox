//! SQL reading, from `dfox-sql`, in the dialect of a connection's database.

pub use dfox_sql::*;

use dfox_core::{
    models::connections::DbType,
    session::{QueryLanguage, SqlDialect},
};

/// The dialect of the SQL a session speaks; `None` when it speaks no SQL.
pub fn session_dialect(language: QueryLanguage) -> Option<Dialect> {
    match language {
        QueryLanguage::Sql(SqlDialect::Postgres) => Some(Dialect::Postgres),
        QueryLanguage::Sql(SqlDialect::MySql) => Some(Dialect::MySql),
        QueryLanguage::Sql(SqlDialect::Sqlite) => Some(Dialect::Sqlite),
        // Unknown SQL reads closest to Postgres's standard style, and so
        // does CQL: the same quotes, comments and `$$`.
        QueryLanguage::Sql(SqlDialect::Generic) | QueryLanguage::Cql => Some(Dialect::Postgres),
        _ => None,
    }
}

/// The language a database speaks, before a session says.
pub fn language(driver: DbType) -> QueryLanguage {
    match driver {
        DbType::Postgres => QueryLanguage::Sql(SqlDialect::Postgres),
        DbType::MySql => QueryLanguage::Sql(SqlDialect::MySql),
        DbType::Sqlite => QueryLanguage::Sql(SqlDialect::Sqlite),
        DbType::Mongo => QueryLanguage::MongoShell,
        DbType::Cassandra => QueryLanguage::Cql,
    }
}

/// The SQL dialect a database speaks, before a session says.
pub fn dialect(driver: DbType) -> Dialect {
    match driver {
        DbType::Postgres => Dialect::Postgres,
        DbType::MySql => Dialect::MySql,
        DbType::Sqlite => Dialect::Sqlite,
        // Speak no SQL; the session says so once connected.
        DbType::Mongo | DbType::Cassandra => Dialect::Postgres,
    }
}
