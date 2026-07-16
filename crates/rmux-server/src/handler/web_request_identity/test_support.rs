use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::{ActiveAttachIdentity, RequestHandler};

#[derive(Debug, Default)]
pub(in crate::handler) struct AttachedQueueSwitchResponsePause {
    pub(in crate::handler) reached: Notify,
    pub(in crate::handler) release: Notify,
}

type AttachRegistration = (u32, u64);
type InstalledPause = (AttachRegistration, Arc<AttachedQueueSwitchResponsePause>);

static ATTACHED_QUEUE_SWITCH_RESPONSE_PAUSES: Mutex<Vec<InstalledPause>> = Mutex::new(Vec::new());

impl RequestHandler {
    pub(in crate::handler) fn install_attached_queue_switch_response_pause(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Arc<AttachedQueueSwitchResponsePause> {
        let registration = (identity.attach_pid(), identity.attach_id());
        let pause = Arc::new(AttachedQueueSwitchResponsePause::default());
        let mut pauses = ATTACHED_QUEUE_SWITCH_RESPONSE_PAUSES
            .lock()
            .expect("attached queue switch response pause lock");
        pauses.retain(|(installed, _)| *installed != registration);
        pauses.push((registration, pause.clone()));
        pause
    }
}

pub(super) async fn pause_before_attached_queue_switch_response_correlation(
    pid: u32,
    attach_id: u64,
) {
    let pause = {
        let mut pauses = ATTACHED_QUEUE_SWITCH_RESPONSE_PAUSES
            .lock()
            .expect("attached queue switch response pause lock");
        pauses
            .iter()
            .position(|(installed, _)| *installed == (pid, attach_id))
            .map(|index| pauses.remove(index).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}
