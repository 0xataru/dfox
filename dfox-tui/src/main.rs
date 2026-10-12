use std::sync::Arc;
use std::env;

use dfox_core::DbManager;
use ui::DatabaseClientUI;
mod db;
mod ui;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();    
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "off");
    }
    
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
