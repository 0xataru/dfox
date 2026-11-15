//! Looking for databases on this machine while the connection manager is
//! open: containers, local ports and SQLite files, again every few seconds,
//! so a forward opened in another terminal shows up and a closed one goes.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::mpsc::UnboundedSender;

use super::{compose, containers, kubernetes, ports, sqlite};
use crate::components::manager::Detected;

/// How often the file index and the cluster are asked again: files and
/// services come and go slower than forwards, and a cluster is far away.
const SLOW_EVERY: Duration = Duration::from_secs(60);

/// A slow search: what it last found, when, and whether it runs now.
#[derive(Debug, Default)]
struct Slow {
    at: Option<Instant>,
    found: Vec<Detected>,
    running: bool,
}

type Searched = Arc<Mutex<Slow>>;

/// Finds databases in the background and sends what is there now, all of
/// it, once every search is done.
#[derive(Debug, Default, Clone)]
pub struct Discovery {
    probed: Arc<ports::Probed>,
    /// Folders searched for SQLite files besides the index.
    folders: Vec<PathBuf>,
    /// The Kubernetes contexts to ask, changed from the app.
    contexts: Arc<Mutex<Vec<String>>>,
    /// The last SQLite and cluster searches, kept for [`SLOW_EVERY`].
    sqlite: Searched,
    kube: Searched,
}

impl Discovery {
    pub fn new(folders: Vec<PathBuf>, contexts: Vec<String>) -> Self {
        Self {
            folders,
            contexts: Arc::new(Mutex::new(contexts)),
            ..Default::default()
        }
    }

    /// Asks `contexts` from now on, at the next scan.
    pub fn set_contexts(&self, contexts: Vec<String>) {
        *self.contexts.lock().unwrap_or_else(|e| e.into_inner()) = contexts;
        self.kube.lock().unwrap_or_else(|e| e.into_inner()).at = None;
    }

    pub fn spawn(&self, found: UnboundedSender<Vec<Detected>>) {
        let this = self.clone();
        tokio::spawn(async move {
            let cwd = std::env::current_dir().unwrap_or_default();
            // Running containers first: a Compose service that runs is theirs.
            let sqlite = slow(&this.sqlite, sqlite::find(this.folders.clone()));
            let kube = slow(
                &this.kube,
                kubernetes::find(
                    this.contexts
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone(),
                ),
            );
            let (mut all, ports, compose) = tokio::join!(
                containers::find(),
                ports::find(&this.probed),
                compose::find(&cwd)
            );
            all.extend(ports);
            all.extend(compose);
            all.extend(sqlite);
            all.extend(kube);
            let _ = found.send(all);
        });
    }
}

/// What `search` last found, at once. Once [`SLOW_EVERY`] has passed it
/// runs again in the background, and a later scan gets its answer: a slow
/// cluster login never holds up containers and ports.
fn slow(
    last: &Searched,
    search: impl std::future::Future<Output = Vec<Detected>> + Send + 'static,
) -> Vec<Detected> {
    let mut state = last.lock().unwrap_or_else(|e| e.into_inner());
    let stale = state.at.is_none_or(|at| at.elapsed() >= SLOW_EVERY);
    if stale && !state.running {
        state.running = true;
        let last = last.clone();
        tokio::spawn(async move {
            let found = search.await;
            let mut state = last.lock().unwrap_or_else(|e| e.into_inner());
            *state = Slow {
                at: Some(Instant::now()),
                found,
                running: false,
            };
        });
    }
    state.found.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn slow_searches_answer_at_once_and_run_in_the_background() {
        let last = Searched::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let search = |runs: Arc<AtomicUsize>| async move {
            runs.fetch_add(1, Ordering::SeqCst);
            let connection = crate::connections::Connection::new(
                "x",
                dfox_core::models::connections::DbType::Redis,
            );
            vec![Detected {
                connection,
                password: None,
                source: "test".into(),
            }]
        };
        // Nothing known yet: an empty answer, and the search starts.
        assert!(slow(&last, search(runs.clone())).is_empty());
        for _ in 0..50 {
            if last.lock().expect("lock").at.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Then its answer, without running it again within a minute.
        assert_eq!(slow(&last, search(runs.clone())).len(), 1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
}
