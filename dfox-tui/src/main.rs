use std::env;

use app::{App, Detected, Setup};
use components::manager::EntryKey;
use config::Config;
use connections::{pgpass, secrets::Keychain, Store};
use history::History;
use theme::ColorDepth;

mod action;
mod app;
mod catalog;
mod clipboard;
mod command;
mod component;
mod components;
mod config;
mod connections;
mod history;
mod icons;
mod keymap;
mod sql;
mod terminal;
#[cfg(test)]
mod testing;
mod theme;
mod widgets;

const USAGE: &str = "\
dfox — a terminal database client

Usage: dfox [URL]

  URL    connect right away, for example
         postgres://user@localhost:5432/shop
         mysql://root@127.0.0.1/app
         sqlite:///path/to/file.db  (or just the path)

Saved connections and settings live in ~/.config/dfox/, the query
history in ~/.local/state/dfox/.
";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = env::args().nth(1);
    match argument.as_deref() {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("dfox {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

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
    let (config, config_problem) = Config::load(config_dir.as_deref());
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
    if let Some(url) = argument {
        match connections::url::parse("command line", &url) {
            Ok((connection, password)) => {
                connect = Some(EntryKey::Detected(detected.len()));
                detected.push(Detected {
                    connection,
                    password,
                    source: "the command line".to_string(),
                });
            }
            Err(err) => problems.push(err),
        }
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
    let (engine, events) = dfox_core::engine::spawn();
    let mut app = App::new(
        engine,
        Setup {
            config,
            themes_dir: config_dir.map(|dir| dir.join("themes")),
            depth: ColorDepth::detect(),
            store,
            secrets: Box::new(Keychain),
            pgpass,
            history,
            clipboard: Box::new(clipboard::System::default()),
            detected,
            connect,
            problems,
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
