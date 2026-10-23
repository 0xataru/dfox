//! The command line: a command, or a database to open in the TUI.

pub const USAGE: &str = "\
dfox — a terminal database client

Usage: dfox [OPTIONS] [URL]
       dfox <COMMAND>

  URL                connect right away, for example
                     postgres://user@localhost:5432/shop
                     mysql://root@127.0.0.1/app
                     sqlite:///path/to/file.db  (or just the path)

Commands:
  connect <NAME>     open a saved connection
  export <CONNECTION> <QUERY> -o <FILE>
                     write a query's rows to a .csv, .tsv, .jsonl or
                     .parquet file
  copy <FROM> <QUERY> <TO> <TABLE> [--create]
                     add a query's rows to a table of another connection,
                     creating it from the rows' columns with --create
  list               list the saved connections
  paths              show where settings, connections and history live
  version            show the version and the databases this build supports
  help               show this help

A CONNECTION is a saved connection's name or a URL. Passwords come from the
keychain, ~/.pgpass, ~/.my.cnf or the URL: these commands never ask.

Options:
  --reduced-motion   no splash, transitions or spinners (also with NO_COLOR
                     set, or animations = false in config.toml)
  -h, --help         show this help
  -V, --version      show the version
";

#[derive(Debug, PartialEq)]
pub enum Cli {
    /// Start the TUI, opening `open` right away if given.
    Run {
        open: Option<Open>,
        reduced_motion: bool,
    },
    List,
    Paths,
    Version,
    Help,
    /// Write `query`'s rows from `from` to `file`.
    Export {
        from: String,
        query: String,
        file: std::path::PathBuf,
    },
    /// Add `query`'s rows from `from` to `table` of `to`.
    Copy {
        from: String,
        query: String,
        to: String,
        table: String,
        create: bool,
    },
}

#[derive(Debug, PartialEq)]
pub enum Open {
    /// A URL or a SQLite file.
    Url(String),
    /// A saved connection, by name.
    Saved(String),
}

/// Reads the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut words = Vec::new();
    let mut reduced_motion = false;
    let mut output = None;
    let mut create = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "--reduced-motion" => reduced_motion = true,
            "--create" => create = true,
            "-o" | "--output" => output = Some(args.next().ok_or("-o needs a file name")?),
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option {flag}"))
            }
            _ => words.push(arg),
        }
    }
    let mut words = words.into_iter();
    let first = words.next();
    let cli = match first.as_deref() {
        None => Cli::Run {
            open: None,
            reduced_motion,
        },
        Some("help") => return Ok(Cli::Help),
        Some("version") => Cli::Version,
        Some("list") => Cli::List,
        Some("paths") => Cli::Paths,
        Some("export") => {
            let usage = "usage: dfox export <connection> <query> -o <file>";
            let (Some(from), Some(query)) = (words.next(), words.next()) else {
                return Err(usage.into());
            };
            let file = output.take().ok_or(usage)?;
            Cli::Export {
                from,
                query,
                file: file.into(),
            }
        }
        Some("copy") => {
            let (Some(from), Some(query), Some(to), Some(table)) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                return Err("usage: dfox copy <from> <query> <to> <table> [--create]".into());
            };
            Cli::Copy {
                from,
                query,
                to,
                table,
                create,
            }
        }
        Some("connect") => {
            let name = words
                .next()
                .ok_or("connect needs the name of a saved connection; `dfox list` shows them")?;
            Cli::Run {
                open: Some(Open::Saved(name)),
                reduced_motion,
            }
        }
        Some(_) => Cli::Run {
            open: first.map(Open::Url),
            reduced_motion,
        },
    };
    if let Some(extra) = words.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    match (output, create, &cli) {
        (Some(_), _, _) => Err("-o only goes with export".into()),
        (_, true, cli) if !matches!(cli, Cli::Copy { .. }) => {
            Err("--create only goes with copy".into())
        }
        _ => Ok(cli),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        super::parse(args.iter().map(|a| a.to_string()))
    }

    fn run(open: Option<Open>) -> Cli {
        Cli::Run {
            open,
            reduced_motion: false,
        }
    }

    #[test]
    fn commands_work_with_and_without_dashes() {
        assert_eq!(parse(&[]), Ok(run(None)));
        assert_eq!(parse(&["version"]), Ok(Cli::Version));
        assert_eq!(parse(&["--version"]), Ok(Cli::Version));
        assert_eq!(parse(&["-V"]), Ok(Cli::Version));
        assert_eq!(parse(&["help"]), Ok(Cli::Help));
        assert_eq!(parse(&["-h"]), Ok(Cli::Help));
        assert_eq!(parse(&["list"]), Ok(Cli::List));
        assert_eq!(parse(&["paths"]), Ok(Cli::Paths));
        // Asking for help wins over anything else on the line.
        assert_eq!(parse(&["connect", "--help"]), Ok(Cli::Help));
    }

    #[test]
    fn a_database_opens_by_url_or_by_name() {
        assert_eq!(
            parse(&["postgres://localhost/shop"]),
            Ok(run(Some(Open::Url("postgres://localhost/shop".into()))))
        );
        assert_eq!(
            parse(&["shop.db", "--reduced-motion"]),
            Ok(Cli::Run {
                open: Some(Open::Url("shop.db".into())),
                reduced_motion: true,
            })
        );
        assert_eq!(
            parse(&["connect", "prod db"]),
            Ok(run(Some(Open::Saved("prod db".into()))))
        );
    }

    #[test]
    fn rows_export_and_copy_from_the_command_line() {
        assert_eq!(
            parse(&[
                "export",
                "prod",
                "SELECT * FROM orders",
                "-o",
                "orders.parquet"
            ]),
            Ok(Cli::Export {
                from: "prod".into(),
                query: "SELECT * FROM orders".into(),
                file: "orders.parquet".into(),
            })
        );
        assert_eq!(
            parse(&[
                "copy",
                "prod",
                "SELECT * FROM orders",
                "dw",
                "orders",
                "--create"
            ]),
            Ok(Cli::Copy {
                from: "prod".into(),
                query: "SELECT * FROM orders".into(),
                to: "dw".into(),
                table: "orders".into(),
                create: true,
            })
        );
        assert!(parse(&["export", "prod", "SELECT 1"])
            .unwrap_err()
            .contains("-o"));
        assert!(parse(&["copy", "prod", "SELECT 1", "dw"])
            .unwrap_err()
            .contains("usage"));
        assert_eq!(
            parse(&["list", "--create"]),
            Err("--create only goes with copy".into())
        );
        assert_eq!(
            parse(&["-o", "x.csv"]),
            Err("-o only goes with export".into())
        );
    }

    #[test]
    fn mistakes_are_errors() {
        assert!(parse(&["connect"]).unwrap_err().contains("dfox list"));
        assert_eq!(
            parse(&["--verbose"]),
            Err("unknown option --verbose".into())
        );
        assert_eq!(
            parse(&["list", "extra"]),
            Err("unexpected argument \"extra\"".into())
        );
    }
}
