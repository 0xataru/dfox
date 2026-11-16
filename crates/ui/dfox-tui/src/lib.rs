//! The dfox terminal app, as a library so benchmarks and tests can reach its
//! parts. `main.rs` is the program.

pub mod action;
pub mod app;
pub mod catalog;
pub mod cli;
pub mod clipboard;
pub mod command;
pub mod component;
pub mod components;
pub mod config;
pub mod connections;
pub mod headless;
pub mod history;
pub mod icons;
pub mod keymap;
pub mod motion;
pub mod sql;
pub mod terminal;
#[cfg(test)]
mod testing;
pub mod theme;
pub mod update;
pub mod widgets;

use std::sync::Arc;

use dfox_core::driver::Driver;

/// The drivers this build has, picked by cargo features.
pub fn drivers() -> Vec<Arc<dyn Driver>> {
    let drivers: Vec<Option<Arc<dyn Driver>>> = vec![
        #[cfg(feature = "postgres")]
        Some(Arc::new(dfox_driver_postgres::PostgresDriver)),
        #[cfg(feature = "mysql")]
        Some(Arc::new(dfox_driver_mysql::MySqlDriver)),
        #[cfg(feature = "sqlite")]
        Some(Arc::new(dfox_driver_sqlite::SqliteDriver)),
        #[cfg(feature = "mongo")]
        Some(Arc::new(dfox_driver_mongo::MongoDriver)),
        #[cfg(feature = "cassandra")]
        Some(Arc::new(dfox_driver_cassandra::CassandraDriver)),
        #[cfg(feature = "redis")]
        Some(Arc::new(dfox_driver_redis::RedisDriver)),
        None,
    ];
    drivers.into_iter().flatten().collect()
}
