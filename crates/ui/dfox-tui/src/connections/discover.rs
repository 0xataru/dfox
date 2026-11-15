//! Looking for databases on this machine while the connection manager is
//! open: containers and local ports, again every few seconds, so a forward
//! opened in another terminal shows up and a closed one goes.

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::{compose, containers, ports};
use crate::components::manager::Detected;

/// Finds containers and ports in the background and sends what is there
/// now, all of it, once both are done.
#[derive(Debug, Default, Clone)]
pub struct Discovery {
    probed: Arc<ports::Probed>,
}

impl Discovery {
    pub fn spawn(&self, found: UnboundedSender<Vec<Detected>>) {
        let probed = self.probed.clone();
        tokio::spawn(async move {
            let cwd = std::env::current_dir().unwrap_or_default();
            // Running containers first: a Compose service that runs is theirs.
            let (mut all, ports, compose) = tokio::join!(
                containers::find(),
                ports::find(&probed),
                compose::find(&cwd)
            );
            all.extend(ports);
            all.extend(compose);
            let _ = found.send(all);
        });
    }
}
