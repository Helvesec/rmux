use std::sync::{Arc, Mutex};

use rmux_proto::SessionName;
use tokio::sync::Notify;

#[derive(Debug, Default)]
pub(in crate::handler) struct PasteBufferIdentityPause {
    reached: Notify,
    release: Notify,
}

impl PasteBufferIdentityPause {
    pub(in crate::handler) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(in crate::handler) fn release(&self) {
        self.release.notify_one();
    }
}

static PAUSES: Mutex<Vec<(SessionName, Arc<PasteBufferIdentityPause>)>> = Mutex::new(Vec::new());

pub(in crate::handler) fn install_paste_buffer_identity_pause(
    session_name: SessionName,
) -> Arc<PasteBufferIdentityPause> {
    let pause = Arc::new(PasteBufferIdentityPause::default());
    let mut pauses = PAUSES.lock().expect("paste-buffer identity pause lock");
    pauses.retain(|(paused_session, _)| paused_session != &session_name);
    pauses.push((session_name, pause.clone()));
    pause
}

pub(in crate::handler) async fn pause_after_paste_buffer_identity_capture(
    session_name: &SessionName,
) {
    let pause = PAUSES
        .lock()
        .expect("paste-buffer identity pause lock")
        .iter()
        .find(|(paused_session, _)| paused_session == session_name)
        .map(|(_, pause)| pause.clone());
    let Some(pause) = pause else {
        return;
    };

    pause.reached.notify_one();
    pause.release.notified().await;

    PAUSES
        .lock()
        .expect("paste-buffer identity pause lock")
        .retain(|(paused_session, current)| {
            paused_session != session_name || !Arc::ptr_eq(current, &pause)
        });
}
