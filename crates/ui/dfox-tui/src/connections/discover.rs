//! Looking for databases on this machine while the connection manager is
//! open: containers, local ports and SQLite files, again every few seconds,
//! so a forward opened in another terminal shows up and a closed one goes.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::mpsc::UnboundedSender;

use super::{compose, containers, ports, sqlite};
use crate::components::manager::Detected;

/// How often the file index is asked again: files come and go slower than
/// forwards.
const SQLITE_EVERY: Duration = Duration::from_secs(60);

/// When the SQLite files were searched, and what was found.
type Searched = (Instant, Vec<Detected>);

/// Finds databases in the background and sends what is there now, all of
/// it, once every search is done.
#[derive(Debug, Default, Clone)]
pub struct Discovery {
    probed: Arc<ports::Probed>,
    /// Folders searched for SQLite files besides the index.
    folders: Vec<PathBuf>,
    /// The last SQLite search, kept for [`SQLITE_EVERY`].
    sqlite: Arc<Mutex<Option<Searched>>>,
}

impl Discovery {
    pub fn new(folders: Vec<PathBuf>) -> Self {
        Self {
            folders,
            ..Default::default()
        }
    }

    pub fn spawn(&self, found: UnboundedSender<Vec<Detected>>) {
        let this = self.clone();
        tokio::spawn(async move {
            let cwd = std::env::current_dir().unwrap_or_default();
            // Running containers first: a Compose service that runs is theirs.
            let (mut all, ports, compose, sqlite) = tokio::join!(
                containers::find(),
                ports::find(&this.probed),
                compose::find(&cwd),
                this.sqlite_files()
            );
            all.extend(ports);
            all.extend(compose);
            all.extend(sqlite);
            let _ = found.send(all);
        });
    }

    async fn sqlite_files(&self) -> Vec<Detected> {
        let last = self
            .sqlite
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some((_, files)) = last.filter(|(at, _)| at.elapsed() < SQLITE_EVERY) {
            return files;
        }
        let files = sqlite::find(self.folders.clone()).await;
        *self.sqlite.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), files.clone()));
        files
    }
}
