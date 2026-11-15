//! Each dialect's common built-in functions, with their arguments, and its
//! column types.

use super::Dialect;

/// `(name, arguments)`.
pub type Function = (&'static str, &'static str);

const COMMON: &[Function] = &[
    ("abs", "x"),
    ("avg", "expr"),
    ("coalesce", "value, …"),
    ("count", "expr"),
    ("lag", "value, offset, default"),
    ("lead", "value, offset, default"),
    ("length", "text"),
    ("lower", "text"),
    ("max", "expr"),
    ("min", "expr"),
    ("nullif", "a, b"),
    ("rank", ""),
    ("replace", "text, from, to"),
    ("round", "x, digits"),
    ("row_number", ""),
    ("sum", "expr"),
    ("trim", "text"),
    ("upper", "text"),
];

const POSTGRES: &[Function] = &[
    ("age", "timestamp, timestamp"),
    ("array_agg", "value"),
    ("concat", "value, …"),
    ("date_trunc", "field, source"),
    ("dense_rank", ""),
    ("gen_random_uuid", ""),
    ("generate_series", "start, stop, step"),
    ("greatest", "value, …"),
    ("json_agg", "value"),
    ("jsonb_agg", "value"),
    ("jsonb_build_object", "key, value, …"),
    ("least", "value, …"),
    ("now", ""),
    ("pg_size_pretty", "bytes"),
    ("pg_total_relation_size", "table"),
    ("regexp_replace", "source, pattern, replacement, flags"),
    ("split_part", "text, delimiter, n"),
    ("string_agg", "value, delimiter"),
    ("substring", "text, start, count"),
    ("to_char", "value, format"),
    ("to_date", "text, format"),
    ("to_timestamp", "text, format"),
];

const MYSQL: &[Function] = &[
    ("concat", "str, …"),
    ("concat_ws", "separator, str, …"),
    ("date_add", "date, INTERVAL n unit"),
    ("date_format", "date, format"),
    ("datediff", "end, start"),
    ("dense_rank", ""),
    ("from_unixtime", "seconds, format"),
    ("greatest", "value, …"),
    ("group_concat", "expr"),
    ("if", "condition, then, else"),
    ("ifnull", "expr, alternative"),
    ("json_extract", "document, path, …"),
    ("json_object", "key, value, …"),
    ("least", "value, …"),
    ("now", ""),
    ("str_to_date", "text, format"),
    ("substring", "str, pos, len"),
    ("unix_timestamp", "date"),
    ("uuid", ""),
];

const SQLITE: &[Function] = &[
    ("date", "time, modifier, …"),
    ("datetime", "time, modifier, …"),
    ("group_concat", "x, separator"),
    ("ifnull", "x, y"),
    ("iif", "condition, then, else"),
    ("instr", "text, needle"),
    ("json_extract", "json, path, …"),
    ("json_object", "label, value, …"),
    ("printf", "format, …"),
    ("random", ""),
    ("strftime", "format, time, modifier, …"),
    ("substr", "text, start, length"),
    ("total", "x"),
    ("typeof", "x"),
];

const CQL: &[Function] = &[
    ("avg", "column"),
    ("count", "column"),
    ("max", "column"),
    ("maxTimeuuid", "timestamp"),
    ("min", "column"),
    ("minTimeuuid", "timestamp"),
    ("now", ""),
    ("sum", "column"),
    ("toDate", "timeuuid"),
    ("toJson", "value"),
    ("toTimestamp", "timeuuid"),
    ("toUnixTimestamp", "timeuuid"),
    ("token", "partition key"),
    ("ttl", "column"),
    ("uuid", ""),
    ("writetime", "column"),
];

/// The dialect's functions: the common ones and its own.
pub fn functions(dialect: Dialect) -> impl Iterator<Item = Function> {
    let own: &[Function] = match dialect {
        Dialect::Postgres => POSTGRES,
        Dialect::MySql => MYSQL,
        Dialect::Sqlite => SQLITE,
        Dialect::Cql => CQL,
    };
    let common: &[Function] = if dialect == Dialect::Cql { &[] } else { COMMON };
    common.iter().chain(own).copied()
}

/// The arguments of the function `name`, when the dialect has it.
pub fn arguments(name: &str, dialect: Dialect) -> Option<&'static str> {
    functions(dialect)
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, args)| args)
}

/// The dialect's column types.
pub fn types(dialect: Dialect) -> &'static [&'static str] {
    match dialect {
        Dialect::Postgres => &[
            "bigint",
            "bigserial",
            "boolean",
            "bytea",
            "char",
            "date",
            "double precision",
            "inet",
            "integer",
            "interval",
            "json",
            "jsonb",
            "numeric",
            "real",
            "serial",
            "smallint",
            "text",
            "time",
            "timestamp",
            "timestamptz",
            "uuid",
            "varchar",
        ],
        Dialect::MySql => &[
            "BIGINT",
            "BLOB",
            "BOOLEAN",
            "CHAR",
            "DATE",
            "DATETIME",
            "DECIMAL",
            "DOUBLE",
            "ENUM",
            "FLOAT",
            "INT",
            "JSON",
            "LONGTEXT",
            "SMALLINT",
            "TEXT",
            "TIME",
            "TIMESTAMP",
            "TINYINT",
            "VARCHAR",
        ],
        Dialect::Sqlite => &["BLOB", "INTEGER", "NUMERIC", "REAL", "TEXT"],
        Dialect::Cql => &[
            "ascii",
            "bigint",
            "blob",
            "boolean",
            "counter",
            "date",
            "decimal",
            "double",
            "duration",
            "float",
            "frozen",
            "inet",
            "int",
            "list",
            "map",
            "set",
            "smallint",
            "text",
            "time",
            "timestamp",
            "timeuuid",
            "tinyint",
            "tuple",
            "uuid",
            "varchar",
            "varint",
        ],
    }
}
