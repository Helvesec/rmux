use std::sync::{Arc, Mutex, Weak};

use tokio::sync::Notify;
use tokio::time::{timeout, Duration};

use crate::handler::RequestHandler;

type Key = (usize, u32);

static RUN_SHELL_CAPTURE_RENDEZVOUS: Mutex<Vec<(Key, Weak<CapturePause>)>> = Mutex::new(Vec::new());

#[derive(Default)]
struct CapturePause {
    reached: Notify,
    release: Notify,
}

pub(in crate::handler) struct RunShellCaptureRendezvous {
    key: Key,
    pause: Arc<CapturePause>,
}

impl RunShellCaptureRendezvous {
    pub(in crate::handler) async fn wait_until_reached(&self) {
        timeout(Duration::from_secs(5), self.pause.reached.notified())
            .await
            .expect("background run-shell reaches the pre-capture rendezvous");
    }

    pub(in crate::handler) fn release(&self) {
        self.pause.release.notify_one();
    }
}

impl Drop for RunShellCaptureRendezvous {
    fn drop(&mut self) {
        RUN_SHELL_CAPTURE_RENDEZVOUS
            .lock()
            .expect("run-shell capture rendezvous registry")
            .retain(|(key, _)| *key != self.key);
        // A failing test must not strand its independently running worker.
        self.release();
    }
}

impl RequestHandler {
    pub(in crate::handler) fn install_run_shell_capture_rendezvous_for_test(
        &self,
        requester_pid: u32,
    ) -> RunShellCaptureRendezvous {
        let key = (Arc::as_ptr(&self.state) as usize, requester_pid);
        let pause = Arc::new(CapturePause::default());
        let mut installed = RUN_SHELL_CAPTURE_RENDEZVOUS
            .lock()
            .expect("run-shell capture rendezvous registry");
        assert!(
            installed.iter().all(|(existing, _)| *existing != key),
            "only one pending capture rendezvous per handler and requester"
        );
        installed.push((key, Arc::downgrade(&pause)));
        RunShellCaptureRendezvous { key, pause }
    }

    pub(in crate::handler) async fn pause_before_run_shell_target_capture_for_test(
        &self,
        requester_pid: u32,
    ) {
        let key = (Arc::as_ptr(&self.state) as usize, requester_pid);
        let pause = {
            let mut installed = RUN_SHELL_CAPTURE_RENDEZVOUS
                .lock()
                .expect("run-shell capture rendezvous registry");
            installed
                .iter()
                .position(|(existing, _)| *existing == key)
                .and_then(|index| installed.swap_remove(index).1.upgrade())
        };
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
}
