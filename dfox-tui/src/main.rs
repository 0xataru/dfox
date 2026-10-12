use std::env;

use app::App;
use config::Config;
use theme::ColorDepth;

mod action;
mod app;
mod command;
mod component;
mod components;
mod config;
mod icons;
mod keymap;
#[cfg(test)]
mod testing;
mod theme;
mod widgets;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let (config, problem) = Config::load(config_dir.as_deref());
    let themes_dir = config_dir.map(|dir| dir.join("themes"));

    let (engine, events) = dfox_core::engine::spawn();
    let mut app = App::new(
        engine,
        config,
        themes_dir,
        ColorDepth::detect(),
        problem.into_iter().collect(),
    );

    // Restores the terminal on panic too.
    let mut terminal = ratatui::try_init()?;
    let result = app.run(&mut terminal, events).await;
    ratatui::restore();
    result?;

    log::info!("dfox application finished");
    Ok(())
}
