//! Connection cancellation is enabled only after an execution offer resolves
//! to a cancel-safe entry. The outer admission barrier still owns direct work.

use std::future::Future;
use tokio::sync::watch;

tokio::task_local! {
    static PEER_CANCELLATION: watch::Sender<bool>;
}

pub(crate) async fn scope<F: Future>(
    policy: Option<watch::Sender<bool>>,
    execution: F,
) -> F::Output {
    match policy {
        Some(policy) => PEER_CANCELLATION.scope(policy, execution).await,
        None => execution.await,
    }
}

pub(crate) fn resolve(behavior: super::RequestQuiesceBehavior) {
    if behavior.cancels_on_peer_disconnect() {
        let _ = PEER_CANCELLATION.try_with(|policy| policy.send_replace(true));
    }
}

pub(super) async fn wait_for_peer_close(
    stream: &rmux_ipc::LocalStream,
    policy: Option<&mut watch::Receiver<bool>>,
) -> std::io::Result<()> {
    if let Some(policy) = policy {
        if policy.wait_for(|cancel| *cancel).await.is_ok() {
            return rmux_ipc::wait_for_peer_close(stream).await;
        }
    }
    std::future::pending().await
}
