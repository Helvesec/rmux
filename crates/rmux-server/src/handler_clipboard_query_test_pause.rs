//! Deterministic test pauses for clipboard query race coverage.

use std::sync::{Arc, Mutex};

use super::{clipboard_query_support::ClipboardQueryState, RequestHandler};

#[derive(Debug, Default)]
pub(in crate::handler) struct ClipboardQueryTestPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl ClipboardQueryTestPause {
    pub(in crate::handler) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(in crate::handler) fn release(&self) {
        self.release.notify_one();
    }
}

type PauseRegistry = Mutex<Vec<(usize, Arc<ClipboardQueryTestPause>)>>;

static PANE_QUERY_DRAIN_PAUSES: PauseRegistry = Mutex::new(Vec::new());
static RESPONSE_COMMIT_PAUSES: PauseRegistry = Mutex::new(Vec::new());
static RESPONSE_STORE_PAUSES: PauseRegistry = Mutex::new(Vec::new());

impl RequestHandler {
    pub(in crate::handler) fn install_pane_query_drain_pause_for_test(
        &self,
    ) -> Arc<ClipboardQueryTestPause> {
        install_drain(&self.clipboard_queries)
    }

    pub(in crate::handler) fn install_clipboard_response_store_pause_for_test(
        &self,
    ) -> Arc<ClipboardQueryTestPause> {
        install_store(&self.clipboard_queries)
    }

    pub(in crate::handler) fn install_clipboard_response_commit_pause_for_test(
        &self,
    ) -> Arc<ClipboardQueryTestPause> {
        install_commit(&self.clipboard_queries)
    }

    pub(in crate::handler) async fn pause_after_first_queued_pane_query_for_test(&self) {
        pause_drain(&self.clipboard_queries).await;
    }

    pub(in crate::handler) async fn pause_after_clipboard_response_store_for_test(&self) {
        pause_store(&self.clipboard_queries).await;
    }

    pub(in crate::handler) async fn pause_before_clipboard_response_commit_for_test(&self) {
        pause_commit(&self.clipboard_queries).await;
    }
}

pub(super) fn install_drain(
    state: &Arc<std::sync::Mutex<ClipboardQueryState>>,
) -> Arc<ClipboardQueryTestPause> {
    install(state, &PANE_QUERY_DRAIN_PAUSES)
}

pub(super) fn install_store(
    state: &Arc<std::sync::Mutex<ClipboardQueryState>>,
) -> Arc<ClipboardQueryTestPause> {
    install(state, &RESPONSE_STORE_PAUSES)
}

pub(super) fn install_commit(
    state: &Arc<std::sync::Mutex<ClipboardQueryState>>,
) -> Arc<ClipboardQueryTestPause> {
    install(state, &RESPONSE_COMMIT_PAUSES)
}

pub(super) async fn pause_drain(state: &Arc<std::sync::Mutex<ClipboardQueryState>>) {
    pause(state, &PANE_QUERY_DRAIN_PAUSES).await;
}

pub(super) async fn pause_store(state: &Arc<std::sync::Mutex<ClipboardQueryState>>) {
    pause(state, &RESPONSE_STORE_PAUSES).await;
}

pub(super) async fn pause_commit(state: &Arc<std::sync::Mutex<ClipboardQueryState>>) {
    pause(state, &RESPONSE_COMMIT_PAUSES).await;
}

fn state_key(state: &Arc<std::sync::Mutex<ClipboardQueryState>>) -> usize {
    Arc::as_ptr(state).addr()
}

fn install(
    state: &Arc<std::sync::Mutex<ClipboardQueryState>>,
    registry: &PauseRegistry,
) -> Arc<ClipboardQueryTestPause> {
    let pause = Arc::new(ClipboardQueryTestPause::default());
    registry
        .lock()
        .expect("clipboard query test pause mutex must not be poisoned")
        .push((state_key(state), Arc::clone(&pause)));
    pause
}

async fn pause(state: &Arc<std::sync::Mutex<ClipboardQueryState>>, registry: &PauseRegistry) {
    let key = state_key(state);
    let pause = {
        let mut registry = registry
            .lock()
            .expect("clipboard query test pause mutex must not be poisoned");
        registry
            .iter()
            .position(|(candidate, _)| *candidate == key)
            .map(|position| registry.swap_remove(position).1)
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}
