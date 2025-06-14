use std::sync::Arc;
use std::env;

use dfox_core::DbManager;
use ui::DatabaseClientUI;
mod db;
mod ui;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if it exists
    dotenv::dotenv().ok();
    
    // Set default log level to off if RUST_LOG is not set
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "off");
    }
    
    // Initialize logging to file
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(Box::new(std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("dfox-debug.log")?)))
        .init();

    log::info!("Starting dfox application");

    let db_manager = Arc::new(DbManager::new());
    let mut tui = DatabaseClientUI::new(db_manager);
    tui.run_ui().await?;

    log::info!("dfox application finished");
    Ok(())
}
