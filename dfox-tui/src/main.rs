use std::env;

use ui::DatabaseClientUI;
mod ui;

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

    let (engine, events) = dfox_core::engine::spawn();
    let mut tui = DatabaseClientUI::new(engine);
    tui.run_ui(events).await?;

    log::info!("dfox application finished");
    Ok(())
}
