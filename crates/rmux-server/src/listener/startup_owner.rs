//! Observe bootstrap abandonment without reconnecting to a replaceable endpoint.

use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

use crate::handler::RequestHandler;

pub(super) fn watch(
    owner: Option<UnixStream>,
    handler: Arc<RequestHandler>,
) -> io::Result<Option<JoinHandle<()>>> {
    let Some(owner) = owner else {
        return Ok(None);
    };
    owner.set_nonblocking(true)?;
    let mut owner = tokio::net::UnixStream::from_std(owner)?;
    Ok(Some(tokio::spawn(async move {
        let mut ready = [0];
        match owner.read(&mut ready).await {
            Ok(0) => {}
            // A successful readiness handoff leaves ordinary lifecycle policy
            // in charge. Read errors do not establish owner abandonment.
            _ => return,
        }
        // Startup configuration may still create a session, disable exit-empty,
        // or admit another client. Evaluate only its completed state.
        while handler.config_loading_active() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handler.shutdown_abandoned_startup().await;
    })))
}

#[cfg(all(test, feature = "web"))]
mod shutdown_idle_policy_tests;
