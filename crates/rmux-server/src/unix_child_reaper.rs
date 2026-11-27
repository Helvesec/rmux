//! Reap closed terminals without keeping their PTYs open or killing jobs that
//! chose to survive hangup. One worker services all surviving children.

use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use rmux_pty::PtyChild;

pub(crate) fn reap(mut child: PtyChild) {
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }
    static REAPER: OnceLock<Option<mpsc::Sender<PtyChild>>> = OnceLock::new();
    let sender = REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<PtyChild>();
        match std::thread::Builder::new()
            .name("rmux-child-reaper".to_owned())
            .spawn(move || {
                let mut children = Vec::new();
                loop {
                    let received = if children.is_empty() {
                        receiver
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    } else {
                        receiver.recv_timeout(Duration::from_millis(50))
                    };
                    match received {
                        Ok(child) => children.push(child),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    children.extend(receiver.try_iter());
                    children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
                }
            }) {
            Ok(_) => Some(sender),
            Err(error) => {
                tracing::warn!("failed to start child reaper: {error}");
                None
            }
        }
    });
    if let Some(sender) = sender {
        let _ = sender.send(child);
    }
}
