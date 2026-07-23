use std::sync::{Arc, Mutex};

use rmux_proto::SessionName;
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum LockIdentityPausePoint {
    ServerClient(u32),
    Session(SessionName),
    SessionClients(SessionName),
    Client(u32),
}

#[derive(Debug, Default)]
pub(in crate::handler) struct LockIdentityPause {
    reached: Notify,
    release: Notify,
}

impl LockIdentityPause {
    pub(in crate::handler) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(in crate::handler) fn release(&self) {
        self.release.notify_one();
    }
}

static PAUSES: Mutex<Vec<(LockIdentityPausePoint, Arc<LockIdentityPause>)>> =
    Mutex::new(Vec::new());

pub(in crate::handler) fn install_lock_identity_pause(
    point: LockIdentityPausePoint,
) -> Arc<LockIdentityPause> {
    let pause = Arc::new(LockIdentityPause::default());
    let mut pauses = PAUSES.lock().expect("lock identity pause lock");
    pauses.retain(|(installed, _)| installed != &point);
    pauses.push((point, pause.clone()));
    pause
}

pub(in crate::handler) async fn pause_after_lock_identity_capture(point: LockIdentityPausePoint) {
    let pause = {
        let mut pauses = PAUSES.lock().expect("lock identity pause lock");
        pauses
            .iter()
            .position(|(installed, _)| installed == &point)
            .map(|index| pauses.swap_remove(index).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}
