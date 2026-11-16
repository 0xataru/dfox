//! What dfox knows of each engine to find and open its databases, in one
//! place. A new `DbType` doesn't build until its row here is filled, and
//! every search reads these rows instead of lists of its own.

use dfox_core::models::connections::DbType;

/// Every engine, in the order the connection form steps through them.
pub const ALL: [DbType; 6] = [
    DbType::Postgres,
    DbType::MySql,
    DbType::Mongo,
    DbType::Cassandra,
    DbType::Redis,
    DbType::Sqlite,
];

pub struct Engine {
    /// The usual port, for servers.
    pub port: Option<u16>,
    /// The database opened when none is set: one every server has.
    pub database: &'static str,
    /// The user offered when a found database names none, for engines that
    /// need one.
    pub user: Option<&'static str>,
    /// Words in the names of images that run it.
    pub images: &'static [&'static str],
    /// Words in the names Kubernetes services give its port.
    pub port_names: &'static [&'static str],
    /// URL schemes naming it.
    pub schemes: &'static [&'static str],
    /// How framework configs name it: Rails' `adapter`, Laravel's
    /// `DB_CONNECTION`.
    pub adapters: &'static [&'static str],
}

pub fn of(driver: DbType) -> Engine {
    match driver {
        DbType::Postgres => Engine {
            port: Some(5432),
            database: "postgres",
            user: Some("postgres"),
            images: &["postgres", "postgis", "timescale"],
            port_names: &["postgres", "pg"],
            schemes: &["postgres", "postgresql"],
            adapters: &["postgresql", "postgres", "pgsql", "postgis"],
        },
        DbType::MySql => Engine {
            port: Some(3306),
            database: "mysql",
            user: Some("root"),
            images: &["mysql", "mariadb", "percona"],
            port_names: &["mysql", "mariadb"],
            schemes: &["mysql", "mariadb"],
            adapters: &["mysql", "mysql2", "trilogy", "mariadb"],
        },
        DbType::Mongo => Engine {
            port: Some(27017),
            // As in mongosh.
            database: "test",
            user: None,
            images: &["mongo"],
            port_names: &["mongo"],
            schemes: &["mongodb"],
            adapters: &["mongodb"],
        },
        DbType::Cassandra => Engine {
            port: Some(9042),
            database: "system",
            user: None,
            images: &["cassandra", "scylla"],
            port_names: &["cql", "cassandra", "scylla"],
            schemes: &["cassandra", "scylla", "scylladb"],
            adapters: &[],
        },
        DbType::Redis => Engine {
            port: Some(6379),
            // Redis numbers its databases.
            database: "0",
            user: None,
            images: &["redis", "valkey", "keydb", "dragonfly"],
            port_names: &["redis", "valkey"],
            schemes: &["redis", "rediss", "valkey", "valkeys"],
            adapters: &[],
        },
        DbType::Sqlite => Engine {
            port: None,
            database: "",
            user: None,
            images: &[],
            port_names: &[],
            schemes: &[],
            adapters: &["sqlite3", "sqlite"],
        },
    }
}

/// The first engine `field` of whose row holds a word `matches`.
pub fn find(
    field: fn(&Engine) -> &'static [&'static str],
    matches: impl Fn(&str) -> bool,
) -> Option<DbType> {
    ALL.into_iter()
        .find(|d| field(&of(*d)).iter().any(|w| matches(w)))
}

/// The engine whose usual port `port` is.
pub fn by_port(port: u16) -> Option<DbType> {
    ALL.into_iter().find(|d| of(*d).port == Some(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_are_told_apart() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                let (a, b) = (of(*a), of(*b));
                assert!(a.port.is_none() || a.port != b.port);
                for field in [
                    |e: &Engine| e.images,
                    |e: &Engine| e.schemes,
                    |e: &Engine| e.adapters,
                ] {
                    assert!(field(&a).iter().all(|w| !field(&b).contains(w)));
                }
            }
        }
    }
}
