use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};

use rmux_core::events::{OutputCursorItem, OutputGap, SdkWaitKey, SdkWaitRegistry};
use rmux_proto::{
    CancelSdkWaitRequest, CancelSdkWaitResponse, ErrorResponse, PaneOutputSubscriptionStart,
    PaneTarget, PaneTargetRef, Response, RmuxError, SdkWaitForOutputRefRequest,
    SdkWaitForOutputRequest, SdkWaitForOutputResponse, SdkWaitId, SdkWaitOutcome, SdkWaitOwnerId,
};
use tokio::sync::oneshot;

use crate::pane_io::PaneOutputReceiver;
use crate::pane_terminals::{session_not_found, HandlerState};

use super::RequestHandler;

const SDK_WAIT_FINISHED_KEY_LIMIT: usize = 4096;
const SDK_WAIT_PENDING_CANCEL_LIMIT: usize = 4096;

#[derive(Debug)]
pub(in crate::handler) struct SdkWaitState {
    registry: SdkWaitRegistry,
    cancel_senders: HashMap<SdkWaitKey, oneshot::Sender<()>>,
    finished_waits: BoundedSdkWaitKeys,
    cancelled_before_register: BoundedSdkWaitKeys,
}

#[derive(Debug)]
struct BoundedSdkWaitKeys {
    keys: HashSet<SdkWaitKey>,
    order: VecDeque<SdkWaitKey>,
    limit: usize,
}

impl BoundedSdkWaitKeys {
    fn new(limit: usize) -> Self {
        Self {
            keys: HashSet::new(),
            order: VecDeque::new(),
            limit,
        }
    }

    fn insert(&mut self, key: SdkWaitKey) {
        if !self.keys.insert(key) {
            return;
        }

        self.order.push_back(key);
        while self.keys.len() > self.limit {
            let Some(expired) = self.order.pop_front() else {
                break;
            };
            self.keys.remove(&expired);
        }
    }

    fn remove(&mut self, key: &SdkWaitKey) -> bool {
        if !self.keys.remove(key) {
            return false;
        }

        self.order.retain(|candidate| candidate != key);
        true
    }

    fn contains(&self, key: &SdkWaitKey) -> bool {
        self.keys.contains(key)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.keys.len()
    }
}

impl Default for SdkWaitState {
    fn default() -> Self {
        Self {
            registry: SdkWaitRegistry::default(),
            cancel_senders: HashMap::new(),
            finished_waits: BoundedSdkWaitKeys::new(SDK_WAIT_FINISHED_KEY_LIMIT),
            cancelled_before_register: BoundedSdkWaitKeys::new(SDK_WAIT_PENDING_CANCEL_LIMIT),
        }
    }
}

enum SdkWaitRegistration {
    Registered(oneshot::Receiver<()>),
    CancelledBeforeRegistration,
}

impl SdkWaitState {
    fn register(
        &mut self,
        connection_id: u64,
        owner_id: SdkWaitOwnerId,
        wait_id: SdkWaitId,
    ) -> Result<SdkWaitRegistration, RmuxError> {
        let key = SdkWaitKey::new(owner_id, wait_id);

        if self.cancelled_before_register.remove(&key) {
            self.remember_finished(key);
            return Ok(SdkWaitRegistration::CancelledBeforeRegistration);
        }

        if !self.registry.register(connection_id, owner_id, wait_id) {
            return Err(RmuxError::Server(format!(
                "SDK wait {} could not be registered for owner {}",
                wait_id.as_u64(),
                owner_id.as_u64()
            )));
        }

        self.finished_waits.remove(&key);
        let (sender, receiver) = oneshot::channel();
        let previous = self.cancel_senders.insert(key, sender);
        debug_assert!(previous.is_none());
        Ok(SdkWaitRegistration::Registered(receiver))
    }

    fn complete(&mut self, owner_id: SdkWaitOwnerId, wait_id: SdkWaitId) -> bool {
        let key = SdkWaitKey::new(owner_id, wait_id);
        self.cancel_senders.remove(&key);
        let removed = self.registry.remove(owner_id, wait_id).is_some();
        if removed {
            self.remember_finished(key);
        }
        removed
    }

    fn cancel(&mut self, owner_id: SdkWaitOwnerId, wait_id: SdkWaitId) -> bool {
        let key = SdkWaitKey::new(owner_id, wait_id);
        let removed = self.registry.remove(owner_id, wait_id).is_some();
        if let Some(sender) = self.cancel_senders.remove(&key) {
            let _ = sender.send(());
        }
        if removed {
            self.remember_finished(key);
        } else if !self.finished_waits.contains(&key) {
            self.cancelled_before_register.insert(key);
        }
        removed
    }

    fn remove_connection(&mut self, connection_id: u64) {
        for record in self.registry.remove_connection(connection_id) {
            if let Some(sender) = self.cancel_senders.remove(&record.key()) {
                let _ = sender.send(());
            }
            self.remember_finished(record.key());
        }
    }

    fn remember_finished(&mut self, key: SdkWaitKey) {
        self.finished_waits.insert(key);
    }
}

struct RegisteredSdkWaitGuard {
    state: Arc<StdMutex<SdkWaitState>>,
    owner_id: SdkWaitOwnerId,
    wait_id: SdkWaitId,
    active: bool,
}

impl RegisteredSdkWaitGuard {
    fn new(
        state: Arc<StdMutex<SdkWaitState>>,
        owner_id: SdkWaitOwnerId,
        wait_id: SdkWaitId,
    ) -> Self {
        Self {
            state,
            owner_id,
            wait_id,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for RegisteredSdkWaitGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let _ = state.cancel(self.owner_id, self.wait_id);
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_sdk_wait_for_output(
        &self,
        connection_id: u64,
        request: SdkWaitForOutputRequest,
    ) -> Response {
        self.sdk_wait_for_output(
            connection_id,
            request.owner_id,
            request.wait_id,
            PaneTargetRef::slot(request.target),
            request.bytes,
            request.start,
        )
        .await
    }

    pub(in crate::handler) async fn handle_sdk_wait_for_output_ref(
        &self,
        connection_id: u64,
        request: SdkWaitForOutputRefRequest,
    ) -> Response {
        self.sdk_wait_for_output(
            connection_id,
            request.owner_id,
            request.wait_id,
            request.target,
            request.bytes,
            request.start,
        )
        .await
    }

    async fn sdk_wait_for_output(
        &self,
        connection_id: u64,
        owner_id: SdkWaitOwnerId,
        wait_id: SdkWaitId,
        target_ref: PaneTargetRef,
        bytes: Vec<u8>,
        start: PaneOutputSubscriptionStart,
    ) -> Response {
        if bytes.is_empty() {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("SDK wait bytes must not be empty".to_owned()),
            });
        }

        let mut receiver = {
            let state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &target_ref) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let output = match state.pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            ) {
                Ok(output) => output,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };

            match start {
                PaneOutputSubscriptionStart::Now => output.subscribe(),
                PaneOutputSubscriptionStart::Oldest => output.subscribe_from_oldest(),
            }
        };

        let cancel_receiver = {
            let mut waits = self
                .sdk_waits
                .lock()
                .expect("SDK wait registry mutex must not be poisoned");
            match waits.register(connection_id, owner_id, wait_id) {
                Ok(SdkWaitRegistration::Registered(receiver)) => receiver,
                Ok(SdkWaitRegistration::CancelledBeforeRegistration) => {
                    return Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                        wait_id,
                        outcome: SdkWaitOutcome::Cancelled,
                    });
                }
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        let mut guard = RegisteredSdkWaitGuard::new(Arc::clone(&self.sdk_waits), owner_id, wait_id);
        let outcome = wait_for_bytes(&mut receiver, &bytes, cancel_receiver).await;
        match outcome {
            SdkWaitOutcome::Matched => {
                let removed = self
                    .sdk_waits
                    .lock()
                    .expect("SDK wait registry mutex must not be poisoned")
                    .complete(owner_id, wait_id);
                guard.disarm();
                if removed {
                    Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                        wait_id,
                        outcome: SdkWaitOutcome::Matched,
                    })
                } else {
                    Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                        wait_id,
                        outcome: SdkWaitOutcome::Cancelled,
                    })
                }
            }
            SdkWaitOutcome::Cancelled => {
                guard.disarm();
                Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                    wait_id,
                    outcome: SdkWaitOutcome::Cancelled,
                })
            }
        }
    }

    pub(in crate::handler) async fn handle_cancel_sdk_wait(
        &self,
        request: CancelSdkWaitRequest,
    ) -> Response {
        let removed = self
            .sdk_waits
            .lock()
            .expect("SDK wait registry mutex must not be poisoned")
            .cancel(request.owner_id, request.wait_id);
        Response::CancelSdkWait(CancelSdkWaitResponse {
            wait_id: request.wait_id,
            removed,
        })
    }

    pub(crate) fn cleanup_connection_sdk_waits_sync(&self, connection_id: u64) {
        self.sdk_waits
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove_connection(connection_id);
    }
}

fn resolve_pane_target_ref(
    state: &HandlerState,
    target: &PaneTargetRef,
) -> Result<PaneTarget, RmuxError> {
    match target {
        PaneTargetRef::Slot(target) => Ok(target.clone()),
        PaneTargetRef::Id {
            session_name,
            pane_id,
        } => {
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let window_index = session
                .window_index_for_pane_id(*pane_id)
                .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), *pane_id))?;
            let pane_index = session
                .window_at(window_index)
                .and_then(|window| {
                    window
                        .panes()
                        .iter()
                        .find(|pane| pane.id() == *pane_id)
                        .map(|pane| pane.index())
                })
                .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), *pane_id))?;
            Ok(PaneTarget::with_window(
                session_name.clone(),
                window_index,
                pane_index,
            ))
        }
    }
}

async fn wait_for_bytes(
    receiver: &mut PaneOutputReceiver,
    needle: &[u8],
    mut cancel_receiver: oneshot::Receiver<()>,
) -> SdkWaitOutcome {
    let mut tail = Vec::new();
    loop {
        while let Some(item) = receiver.try_recv() {
            if observe_cursor_item(&mut tail, needle, item) {
                return SdkWaitOutcome::Matched;
            }
        }

        tokio::select! {
            item = receiver.recv() => {
                if observe_cursor_item(&mut tail, needle, item) {
                    return SdkWaitOutcome::Matched;
                }
            }
            _ = &mut cancel_receiver => {
                return SdkWaitOutcome::Cancelled;
            }
        }
    }
}

fn observe_cursor_item(tail: &mut Vec<u8>, needle: &[u8], item: OutputCursorItem) -> bool {
    match item {
        OutputCursorItem::Event(event) => observe_bytes(tail, needle, event.bytes()),
        OutputCursorItem::Gap(gap) => observe_gap(tail, needle, &gap),
    }
}

fn observe_gap(tail: &mut Vec<u8>, needle: &[u8], gap: &OutputGap) -> bool {
    let expected = gap.expected_sequence();
    let recent = gap.recent_snapshot();
    let starts_at_expected = recent.oldest_sequence_at_or_after(expected) == Some(expected)
        && recent.starts_at_event_start(expected);
    if !starts_at_expected {
        tail.clear();
    }
    observe_bytes(tail, needle, recent.bytes_from_sequence(expected))
}

fn observe_bytes(tail: &mut Vec<u8>, needle: &[u8], bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    let mut combined = Vec::with_capacity(tail.len() + bytes.len());
    combined.extend_from_slice(tail);
    combined.extend_from_slice(bytes);
    let matched = combined
        .windows(needle.len())
        .any(|candidate| candidate == needle);

    let keep = needle.len().saturating_sub(1);
    if keep == 0 {
        tail.clear();
    } else if combined.len() <= keep {
        *tail = combined;
    } else {
        *tail = combined[combined.len() - keep..].to_vec();
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::pane_output_channel_with_limits;

    fn owner(value: u64) -> SdkWaitOwnerId {
        SdkWaitOwnerId::new(value)
    }

    fn wait(value: u64) -> SdkWaitId {
        SdkWaitId::new(value)
    }

    #[test]
    fn byte_observer_matches_across_event_boundaries_without_unbounded_tail() {
        let mut tail = Vec::new();
        assert!(!observe_bytes(&mut tail, b"needle", b"xxnee"));
        assert_eq!(tail, b"xxnee");
        assert!(observe_bytes(&mut tail, b"needle", b"dleyy"));
        assert_eq!(tail, b"dleyy");
    }

    #[test]
    fn byte_observer_ignores_pre_arm_recent_output_after_cursor_gap() {
        let output = pane_output_channel_with_limits(1, 64);
        output.send(b"stale needle".to_vec());
        let mut receiver = output.subscribe();
        output.send(b"future without".to_vec());
        output.send(b"match".to_vec());

        let Some(OutputCursorItem::Gap(gap)) = receiver.try_recv() else {
            panic!("slow post-arm receiver should observe a cursor gap");
        };
        assert_eq!(gap.expected_sequence(), 1);
        assert_eq!(gap.resume_sequence(), 2);
        assert_eq!(
            gap.recent_snapshot().bytes(),
            b"stale needlefuture withoutmatch"
        );

        let mut tail = Vec::new();
        assert!(
            !observe_gap(&mut tail, b"needle", &gap),
            "wait matcher must not complete on recent output emitted before subscribe"
        );
    }

    #[test]
    fn byte_observer_matches_post_arm_recent_output_after_cursor_gap() {
        let output = pane_output_channel_with_limits(1, 64);
        output.send(b"stale".to_vec());
        let mut receiver = output.subscribe();
        output.send(b"future needle".to_vec());
        output.send(b"after".to_vec());

        let Some(OutputCursorItem::Gap(gap)) = receiver.try_recv() else {
            panic!("slow post-arm receiver should observe a cursor gap");
        };
        assert_eq!(gap.expected_sequence(), 1);
        assert_eq!(gap.resume_sequence(), 2);
        assert_eq!(gap.recent_snapshot().bytes(), b"stalefuture needleafter");

        let mut tail = Vec::new();
        assert!(
            observe_gap(&mut tail, b"needle", &gap),
            "wait matcher should still use missed output emitted after subscribe"
        );
    }

    #[test]
    fn byte_observer_does_not_match_across_trimmed_gap_prefix() {
        let output = pane_output_channel_with_limits(1, 4);
        let mut receiver = output.subscribe();
        output.send(b"nee".to_vec());
        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("receiver should observe the first retained output event");
        };
        let mut tail = Vec::new();
        assert!(!observe_bytes(&mut tail, b"needle", event.bytes()));
        assert_eq!(tail, b"nee");

        output.send(b"xxdle".to_vec());
        output.send(b"q".to_vec());
        let Some(OutputCursorItem::Gap(gap)) = receiver.try_recv() else {
            panic!("slow post-arm receiver should observe a cursor gap");
        };
        assert_eq!(gap.expected_sequence(), 1);
        assert_eq!(gap.resume_sequence(), 2);
        assert_eq!(gap.recent_snapshot().bytes(), b"dleq");
        assert!(!gap.recent_snapshot().starts_at_event_start(1));

        assert!(
            !observe_gap(&mut tail, b"needle", &gap),
            "wait matcher must not join observed tail across a trimmed gap prefix"
        );
    }

    #[tokio::test]
    async fn wait_for_bytes_returns_cancelled_when_registry_sends_cancel() {
        let output = pane_output_channel_with_limits(4, 64);
        let mut receiver = output.subscribe();
        let (cancel, cancel_receiver) = oneshot::channel();

        let wait =
            tokio::spawn(
                async move { wait_for_bytes(&mut receiver, b"never", cancel_receiver).await },
            );
        output.send(b"not it".to_vec());
        let _ = cancel.send(());

        assert_eq!(wait.await.expect("wait task"), SdkWaitOutcome::Cancelled);
    }

    #[test]
    fn connection_teardown_cancels_only_that_connections_sdk_waits() {
        let mut state = SdkWaitState::default();
        let mut first = registered_receiver(
            state
                .register(1, owner(10), wait(1))
                .expect("first registration succeeds"),
        );
        let mut second = registered_receiver(
            state
                .register(1, owner(10), wait(2))
                .expect("second registration succeeds"),
        );
        let mut other_connection = registered_receiver(
            state
                .register(2, owner(20), wait(1))
                .expect("other connection registration succeeds"),
        );

        state.remove_connection(1);

        assert!(matches!(first.try_recv(), Ok(())));
        assert!(matches!(second.try_recv(), Ok(())));
        assert!(matches!(
            other_connection.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        assert!(state.cancel(owner(20), wait(1)));
        assert!(matches!(other_connection.try_recv(), Ok(())));
        assert!(!state.cancel(owner(10), wait(1)));
    }

    #[test]
    fn pre_registration_cancel_is_consumed_by_late_sdk_wait_registration() {
        let mut state = SdkWaitState::default();

        assert!(!state.cancel(owner(9), wait(1)));
        let registration = state
            .register(33, owner(9), wait(1))
            .expect("late wait registration succeeds as cancelled");
        assert!(matches!(
            registration,
            SdkWaitRegistration::CancelledBeforeRegistration
        ));
        assert!(!state.cancel(owner(9), wait(1)));
    }

    #[test]
    fn sdk_wait_keys_are_reusable_after_completion_or_teardown() {
        let mut state = SdkWaitState::default();

        let registration = state
            .register(44, owner(10), wait(1))
            .expect("first registration succeeds");
        assert!(matches!(registration, SdkWaitRegistration::Registered(_)));
        assert!(state.complete(owner(10), wait(1)));
        assert!(!state.cancel(owner(10), wait(1)));
        assert!(matches!(
            state
                .register(45, owner(10), wait(1))
                .expect("completed key can be reused by a later connection"),
            SdkWaitRegistration::Registered(_)
        ));
        state.remove_connection(45);

        let registration = state
            .register(46, owner(10), wait(1))
            .expect("teardown also releases the key for a later connection");
        assert!(matches!(registration, SdkWaitRegistration::Registered(_)));
    }

    #[test]
    fn active_sdk_wait_keys_still_reject_duplicate_registration() {
        let mut state = SdkWaitState::default();

        let registration = state
            .register(44, owner(10), wait(1))
            .expect("first registration succeeds");
        assert!(matches!(registration, SdkWaitRegistration::Registered(_)));

        assert!(state.register(45, owner(10), wait(1)).is_err());
    }

    #[test]
    fn completed_sdk_wait_tracking_remains_bounded() {
        let mut state = SdkWaitState::default();

        for id in 1..=(SDK_WAIT_FINISHED_KEY_LIMIT + 128) as u64 {
            let registration = state
                .register(id, owner(10), wait(id))
                .expect("registration succeeds");
            assert!(matches!(registration, SdkWaitRegistration::Registered(_)));
            assert!(state.complete(owner(10), wait(id)));
        }

        assert!(state.registry.is_empty());
        assert!(state.cancel_senders.is_empty());
        assert_eq!(state.cancelled_before_register.len(), 0);
        assert!(state.finished_waits.len() <= SDK_WAIT_FINISHED_KEY_LIMIT);
    }

    fn registered_receiver(registration: SdkWaitRegistration) -> oneshot::Receiver<()> {
        match registration {
            SdkWaitRegistration::Registered(receiver) => receiver,
            SdkWaitRegistration::CancelledBeforeRegistration => {
                panic!("wait must register before cancellation")
            }
        }
    }
}
