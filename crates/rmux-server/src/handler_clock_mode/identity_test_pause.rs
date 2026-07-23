use std::sync::{Arc, Mutex};

use rmux_proto::PaneTarget;
use tokio::sync::Notify;

use super::{ActiveAttachIdentity, RequestHandler};

#[derive(Debug, Default)]
pub(in crate::handler) struct ClockModeIdentityPause {
    reached: Notify,
    release: Notify,
}

impl ClockModeIdentityPause {
    pub(in crate::handler) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(in crate::handler) fn release(&self) {
        self.release.notify_one();
    }
}

type LiveExitPauseKey = (usize, ActiveAttachIdentity);
type ExitCommitPauseKey = (usize, PaneTarget);

static LIVE_EXIT_PAUSES: Mutex<Vec<(LiveExitPauseKey, Arc<ClockModeIdentityPause>)>> =
    Mutex::new(Vec::new());
static EXIT_COMMIT_PAUSES: Mutex<Vec<(ExitCommitPauseKey, Arc<ClockModeIdentityPause>)>> =
    Mutex::new(Vec::new());
static RESTORE_COMMIT_PAUSES: Mutex<Vec<(ExitCommitPauseKey, Arc<ClockModeIdentityPause>)>> =
    Mutex::new(Vec::new());

impl RequestHandler {
    pub(in crate::handler) fn install_live_clock_mode_exit_pause_for_test(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Arc<ClockModeIdentityPause> {
        install_pause((self.test_handler_key(), identity), &LIVE_EXIT_PAUSES)
    }

    pub(in crate::handler) async fn pause_before_live_clock_mode_exit_for_test(
        &self,
        identity: ActiveAttachIdentity,
    ) {
        pause_once(&(self.test_handler_key(), identity), &LIVE_EXIT_PAUSES).await;
    }

    pub(in crate::handler) fn install_clock_mode_exit_commit_pause_for_test(
        &self,
        target: PaneTarget,
    ) -> Arc<ClockModeIdentityPause> {
        install_pause((self.test_handler_key(), target), &EXIT_COMMIT_PAUSES)
    }

    pub(in crate::handler) async fn pause_after_clock_mode_exit_commit_for_test(
        &self,
        target: &PaneTarget,
    ) {
        pause_once(
            &(self.test_handler_key(), target.clone()),
            &EXIT_COMMIT_PAUSES,
        )
        .await;
    }

    pub(in crate::handler) fn install_clock_mode_restore_commit_pause_for_test(
        &self,
        target: PaneTarget,
    ) -> Arc<ClockModeIdentityPause> {
        install_pause((self.test_handler_key(), target), &RESTORE_COMMIT_PAUSES)
    }

    pub(in crate::handler) async fn pause_before_clock_mode_restore_commit_for_test(
        &self,
        target: &PaneTarget,
    ) {
        pause_once(
            &(self.test_handler_key(), target.clone()),
            &RESTORE_COMMIT_PAUSES,
        )
        .await;
    }

    fn test_handler_key(&self) -> usize {
        Arc::as_ptr(&self.active_attach) as usize
    }
}

fn install_pause<K: Eq + Clone>(
    key: K,
    registry: &Mutex<Vec<(K, Arc<ClockModeIdentityPause>)>>,
) -> Arc<ClockModeIdentityPause> {
    let pause = Arc::new(ClockModeIdentityPause::default());
    let mut pauses = registry.lock().expect("clock-mode identity pause lock");
    pauses.retain(|(installed, _)| installed != &key);
    pauses.push((key, pause.clone()));
    pause
}

async fn pause_once<K: Eq>(key: &K, registry: &Mutex<Vec<(K, Arc<ClockModeIdentityPause>)>>) {
    let pause = {
        let mut pauses = registry.lock().expect("clock-mode identity pause lock");
        pauses
            .iter()
            .position(|(installed, _)| installed == key)
            .map(|index| pauses.remove(index).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}
