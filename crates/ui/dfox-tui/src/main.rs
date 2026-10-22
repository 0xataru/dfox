use std::{env, sync::Arc};

use dfox_core::{
    driver::{Driver, Drivers},
    engine::Command as DbCommand,
};

use dfox_tui::{
    app::{App, Detected, Setup},
    cli::{self, Cli, Open, USAGE},
    clipboard,
    components::manager::EntryKey,
    config::Config,
    connections::{self, pgpass, secrets::Keychain, Store},
    history::History,
    terminal,
    theme::ColorDepth,
};

/// A faster allocator than the system's for the app's many small strings.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The drivers this build has, picked by cargo features.
fn drivers() -> Vec<Arc<dyn Driver>> {
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

/// Prints a line of a command's output. A closed pipe (`dfox list | head -1`)
/// ends the output quietly instead of panicking.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// Ends the program with `message` and the usage, as a command-line mistake.
fn usage_error(message: &str) -> ! {
    eprintln!("dfox: {message}\n\n{USAGE}");
    std::process::exit(2);
}

/// Sessions go through an SSH tunnel when the connection has one.
fn opener() -> dfox_core::engine::Opener {
    Arc::new(|driver, params| {
        Box::pin(async move { dfox_ssh::open(driver.as_ref(), &params).await })
    })
}

/// Runs `dfox export` or `dfox copy`, reporting on stderr, and returns the
/// exit code.
async fn headless(cli: Cli) -> i32 {
    let config_dir = Config::dir();
    let (store, problem) = Store::load(config_dir.as_deref());
    if let Some(problem) = problem {
        eprintln!("dfox: {problem}");
    }
    let pgpass = pgpass::load();
    let resolve = |name: &str| dfox_tui::headless::resolve(name, &store, &Keychain, &pgpass);
    let (from, command, done) = match cli {
        Cli::Export { from, query, file } => {
            let done = format!("to {}", file.display());
            let command = DbCommand::Export {
                id: 1,
                sql: query,
                file,
            };
            (from, Ok(command), ("Exported", done))
        }
        Cli::Copy {
            from,
            query,
            to,
            table,
            create,
        } => {
            let command = resolve(&to).map(|(connection, params)| DbCommand::Copy {
                id: 1,
                sql: query,
                target: Box::new(dfox_core::engine::CopyTarget {
                    db_type: connection.driver,
                    params,
                    table: dfox_tui::components::copy::target_path(&connection, &table),
                    create,
                }),
            });
            let into = dfox_tui::headless::shown(&to);
            (from, command, ("Copied", format!("into {into}: {table}")))
        }
        _ => return 2,
    };
    let outcome = match (resolve(&from), command) {
        (Ok((connection, params)), Ok(command)) => {
            let (engine, mut events) =
                dfox_core::engine::spawn_with(Drivers::new(drivers()), opener());
            let progress = dfox_tui::headless::shows_progress();
            dfox_tui::headless::run(&engine, &mut events, &connection, params, command, progress)
                .await
        }
        (Err(err), _) | (_, Err(err)) => Err(err),
    };
    match outcome {
        Ok(stats) => {
            let (verb, place) = done;
            eprintln!(
                "{verb} {} rows {place} in {:.1} s",
                stats.rows,
                stats.elapsed.as_secs_f64()
            );
            0
        }
        Err(err) => {
            eprintln!("dfox: {err}");
            1
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (open, reduced_motion) = match cli::parse(env::args().skip(1)) {
        Err(err) => usage_error(&err),
        Ok(Cli::Help) => {
            out!("{}", USAGE.trim_end());
            return Ok(());
        }
        Ok(Cli::Version) => {
            out!("dfox {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Ok(Cli::Paths) => {
            let show = |path: Option<std::path::PathBuf>| {
                path.map_or("unknown".to_string(), |p| p.display().to_string())
            };
            let config = Config::dir();
            out!(
                "config       {}",
                show(config.as_ref().map(|d| d.join("config.toml")))
            );
            out!(
                "connections  {}",
                show(config.as_ref().map(|d| d.join("connections.toml")))
            );
            out!(
                "themes       {}",
                show(config.as_ref().map(|d| d.join("themes")))
            );
            out!("history      {}", show(History::dir()));
            return Ok(());
        }
        Ok(Cli::List) => {
            let (store, problem) = Store::load(Config::dir().as_deref());
            if let Some(problem) = problem {
                eprintln!("dfox: {problem}");
                std::process::exit(1);
            }
            if store.connections.is_empty() {
                out!("No saved connections. Add one in the connection manager: run dfox.");
            }
            let width = store
                .connections
                .iter()
                .map(|c| c.name.chars().count())
                .max();
            for connection in &store.connections {
                let driver = drivers()
                    .iter()
                    .find(|d| d.db_type() == connection.driver)
                    .map_or(format!("{:?}", connection.driver), |d| {
                        d.info().name.to_string()
                    });
                out!(
                    "{:width$}  {driver:10}  {}",
                    connection.name,
                    connection.address(),
                    width = width.unwrap_or(0)
                );
            }
            return Ok(());
        }
        Ok(cli @ (Cli::Export { .. } | Cli::Copy { .. })) => {
            std::process::exit(headless(cli).await);
        }
        Ok(Cli::Run {
            open,
            reduced_motion,
        }) => (open, reduced_motion),
    };
    let reduced_motion = reduced_motion || env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());

    dotenvy::dotenv().ok();
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "off");
    }
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(Box::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("dfox-debug.log")?,
        )))
        .init();
    log::info!("Starting dfox application");

    let config_dir = Config::dir();
    let (mut config, config_problem) = Config::load(config_dir.as_deref());
    if reduced_motion {
        config.animations = false;
    }
    let (store, store_problem) = Store::load(config_dir.as_deref());
    let (history, history_problem) = History::load(History::dir().as_deref());
    let mut problems: Vec<String> = config_problem
        .into_iter()
        .chain(store_problem)
        .chain(history_problem)
        .collect();

    let pgpass = pgpass::load();
    let mut detected = Vec::new();
    let mut connect = None;
    match open {
        None => {}
        // Mistakes on the command line end here, before the TUI starts.
        Some(Open::Url(url)) => match connections::url::parse("command line", &url) {
            Ok((connection, password)) => {
                connect = Some(EntryKey::Detected(detected.len()));
                detected.push(Detected {
                    connection,
                    password,
                    source: "the command line".to_string(),
                });
            }
            Err(err) => usage_error(&err),
        },
        Some(Open::Saved(name)) => match store.get(&name) {
            Some(_) => connect = Some(EntryKey::Saved(name)),
            None => {
                eprintln!("dfox: no saved connection named {name:?}; `dfox list` shows them");
                std::process::exit(2);
            }
        },
    }
    if let Some(url) = env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
    {
        match connections::url::parse("DATABASE_URL", &url) {
            Ok((connection, password)) => detected.push(Detected {
                connection,
                password,
                source: "DATABASE_URL".to_string(),
            }),
            Err(err) => problems.push(format!("DATABASE_URL: {err}")),
        }
    }
    detected.extend(
        pgpass::connections(&pgpass)
            .into_iter()
            .map(|connection| Detected {
                password: pgpass::lookup(&pgpass, &connection).map(str::to_string),
                connection,
                source: ".pgpass".to_string(),
            }),
    );

    let mouse = config.mouse;
    let update = config.update_check;
    if update {
        dfox_tui::update::refresh(History::dir());
    }
    let (engine, events) = dfox_core::engine::spawn_with(Drivers::new(drivers()), opener());
    let mut app = App::new(
        engine,
        Setup {
            config,
            config_dir,
            depth: ColorDepth::detect(),
            store,
            secrets: Box::new(Keychain),
            pgpass,
            history,
            clipboard: Box::new(clipboard::System::default()),
            detected,
            connect,
            problems,
            splash: true,
            update: update
                .then(|| dfox_tui::update::notice(History::dir().as_deref()))
                .flatten(),
        },
    );

    // Restores the terminal on panic too.
    let mut terminal = terminal::init(mouse)?;
    let result = app.run(&mut terminal, events).await;
    terminal::restore();
    result?;

    log::info!("dfox application finished");
    Ok(())
}
