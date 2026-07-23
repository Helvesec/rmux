use std::sync::{Arc, Mutex};

use super::super::super::RequestHandler;

#[derive(Debug, Default)]
pub(in crate::handler) struct AttachedKeyDispatchCommitPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

static COMMIT_PAUSES: Mutex<Vec<(usize, u32, Arc<AttachedKeyDispatchCommitPause>)>> =
    Mutex::new(Vec::new());

impl RequestHandler {
    pub(in crate::handler) fn install_attached_key_dispatch_commit_pause(
        &self,
        attach_pid: u32,
    ) -> Arc<AttachedKeyDispatchCommitPause> {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = Arc::new(AttachedKeyDispatchCommitPause::default());
        COMMIT_PAUSES
            .lock()
            .expect("attached key dispatch pause registry lock")
            .push((handler_key, attach_pid, Arc::clone(&pause)));
        pause
    }

    pub(super) async fn pause_attached_key_dispatch_after_lookup(&self, attach_pid: u32) {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = {
            let mut pauses = COMMIT_PAUSES
                .lock()
                .expect("attached key dispatch pause registry lock");
            pauses
                .iter()
                .position(|(key, pid, _)| *key == handler_key && *pid == attach_pid)
                .map(|position| pauses.swap_remove(position).2)
        };
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
}
