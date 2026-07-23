//! Stable, bounded routing for pane-originated OSC 52 clipboard queries.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use rmux_core::{PaneId, TerminalClipboardQuery};
use rmux_proto::{OptionName, PaneTarget, SessionName, WindowId};
use tokio::time::Instant;

use super::attach_support::ActiveAttachIdentity;
use super::pane_support::{
    prepare_pane_input_write, write_attached_bytes_to_target_io, PaneInputLiveness,
};
use super::RequestHandler;
use crate::clipboard_protocol::{
    encode_clipboard_response, encode_clipboard_response_parts, CLIPBOARD_QUERY_SEQUENCE,
};
use crate::pane_io::AttachControl;
use crate::pane_terminals::HandlerState;

#[path = "handler_clipboard_query/desynchronization.rs"]
mod desynchronization;
#[path = "handler_clipboard_query/targeting.rs"]
mod targeting;

use desynchronization::{
    expire_pending_at, mark_clipboard_attaches_desynchronized, same_attach_generation,
};
use targeting::{
    most_recent_clipboard_attach, pane_target_for_stable_identity_in_session,
    resolve_pending_clipboard_target,
};

const CLIPBOARD_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_PENDING_CLIPBOARD_QUERIES: usize = 64;
const MAX_QUEUED_PANE_CLIPBOARD_QUERIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardRequestMode {
    Request,
    Both,
}

impl ClipboardRequestMode {
    fn stores_response(self) -> bool {
        matches!(self, Self::Both)
    }
}

#[derive(Debug, Clone)]
struct StableClipboardPane {
    runtime_session: SessionName,
    pane_id: PaneId,
    generation: Option<u64>,
    window_id: WindowId,
}

impl StableClipboardPane {
    fn capture(
        state: &HandlerState,
        runtime_session: &SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Option<Self> {
        let resolved_runtime =
            state.resolve_pane_event_runtime_session(runtime_session, pane_id, generation)?;
        let target = state.pane_target_for_runtime_pane(&resolved_runtime, pane_id)?;
        let window = state
            .sessions
            .session(target.session_name())?
            .window_at(target.window_index())?;
        (window.pane(target.pane_index())?.id() == pane_id).then_some(Self {
            runtime_session: resolved_runtime,
            pane_id,
            generation,
            window_id: window.id(),
        })
    }

    fn resolve(&self, state: &HandlerState) -> Option<PaneTarget> {
        let runtime_session = state.resolve_pane_event_runtime_session(
            &self.runtime_session,
            self.pane_id,
            self.generation,
        )?;
        if let Some(target) = pane_target_for_stable_identity_in_session(
            state,
            &runtime_session,
            self.window_id,
            self.pane_id,
        ) {
            return Some(target);
        }
        let target = state.pane_target_for_runtime_pane(&runtime_session, self.pane_id)?;
        let window = state
            .sessions
            .session(target.session_name())?
            .window_at(target.window_index())?;
        (window.id() == self.window_id && window.pane(target.pane_index())?.id() == self.pane_id)
            .then_some(target)
    }
}

#[derive(Debug, Clone)]
struct PendingClipboardQuery {
    token: u64,
    attach: ActiveAttachIdentity,
    pane: StableClipboardPane,
    query: TerminalClipboardQuery,
    mode: ClipboardRequestMode,
    expires_at: Instant,
}

#[derive(Debug)]
struct QueuedPaneClipboardQueries {
    runtime_session: SessionName,
    pane_id: PaneId,
    generation: Option<u64>,
    queries: Vec<TerminalClipboardQuery>,
}

#[derive(Debug, Default)]
pub(in crate::handler) struct ClipboardQueryState {
    next_token: u64,
    pending: VecDeque<PendingClipboardQuery>,
    queued_pane_queries: VecDeque<QueuedPaneClipboardQueries>,
    queued_pane_query_count: usize,
    pane_query_worker_running: bool,
}

impl ClipboardQueryState {
    fn enqueue_pane_queries(
        &mut self,
        runtime_session: SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
        mut queries: Vec<TerminalClipboardQuery>,
    ) -> (bool, usize) {
        let available =
            MAX_QUEUED_PANE_CLIPBOARD_QUERIES.saturating_sub(self.queued_pane_query_count);
        let dropped = queries.len().saturating_sub(available);
        queries.truncate(available);
        if !queries.is_empty() {
            self.queued_pane_query_count =
                self.queued_pane_query_count.saturating_add(queries.len());
            self.queued_pane_queries
                .push_back(QueuedPaneClipboardQueries {
                    runtime_session,
                    pane_id,
                    generation,
                    queries,
                });
        }
        let should_start_worker = !self.queued_pane_queries.is_empty()
            && !std::mem::replace(&mut self.pane_query_worker_running, true);
        (should_start_worker, dropped)
    }

    fn take_queued_pane_queries(&mut self) -> Option<QueuedPaneClipboardQueries> {
        let queued = self.queued_pane_queries.pop_front();
        if let Some(queued) = &queued {
            self.queued_pane_query_count = self
                .queued_pane_query_count
                .saturating_sub(queued.queries.len());
        } else {
            self.pane_query_worker_running = false;
        }
        queued
    }

    fn register(
        &mut self,
        attach: ActiveAttachIdentity,
        pane: StableClipboardPane,
        query: TerminalClipboardQuery,
        mode: ClipboardRequestMode,
        now: Instant,
    ) -> Option<u64> {
        if self.pending.len() >= MAX_PENDING_CLIPBOARD_QUERIES {
            return None;
        }
        let token = self.next_token;
        self.next_token = self.next_token.checked_add(1)?;
        self.pending.push_back(PendingClipboardQuery {
            token,
            attach,
            pane,
            query,
            mode,
            expires_at: now + CLIPBOARD_REQUEST_TIMEOUT,
        });
        Some(token)
    }

    fn cancel(&mut self, token: u64) {
        self.pending.retain(|pending| pending.token != token);
    }

    fn take_for_attach_registration(
        &mut self,
        attach: ActiveAttachIdentity,
    ) -> Option<PendingClipboardQuery> {
        let position = self
            .pending
            .iter()
            .position(|pending| same_attach_generation(pending.attach, attach))?;
        self.pending.remove(position)
    }

    fn expire_at(&mut self, now: Instant) -> Vec<ActiveAttachIdentity> {
        expire_pending_at(&mut self.pending, now)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    fn queued_len(&self) -> usize {
        self.queued_pane_query_count
    }
}

impl RequestHandler {
    pub(in crate::handler) fn enqueue_pane_clipboard_queries(
        &self,
        runtime_session: SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
        queries: Vec<TerminalClipboardQuery>,
    ) -> bool {
        let (should_start_worker, dropped) = self
            .clipboard_queries
            .lock()
            .expect("clipboard query mutex must not be poisoned")
            .enqueue_pane_queries(runtime_session, pane_id, generation, queries);
        if dropped > 0 {
            tracing::warn!(
                pane_id = pane_id.as_u32(),
                dropped,
                "dropped pane clipboard queries because the ordered queue is full"
            );
        }
        should_start_worker
    }

    pub(in crate::handler) async fn drain_pane_clipboard_queries(&self) {
        loop {
            let queued = self
                .clipboard_queries
                .lock()
                .expect("clipboard query mutex must not be poisoned")
                .take_queued_pane_queries();
            let Some(queued) = queued else {
                return;
            };
            self.pause_after_first_queued_pane_query_for_test().await;
            self.handle_pane_clipboard_queries(
                queued.runtime_session,
                queued.pane_id,
                queued.generation,
                queued.queries,
            )
            .await;
        }
    }

    #[cfg(not(test))]
    async fn pause_after_first_queued_pane_query_for_test(&self) {}

    #[cfg(not(test))]
    async fn pause_after_clipboard_response_store_for_test(&self) {}

    #[cfg(not(test))]
    async fn pause_before_clipboard_response_commit_for_test(&self) {}

    pub(in crate::handler) async fn handle_pane_clipboard_queries(
        &self,
        runtime_session: SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
        queries: Vec<TerminalClipboardQuery>,
    ) {
        for query in queries {
            self.handle_pane_clipboard_query(&runtime_session, pane_id, generation, query)
                .await;
        }
    }

    async fn handle_pane_clipboard_query(
        &self,
        runtime_session: &SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
        query: TerminalClipboardQuery,
    ) {
        let (pane, mode, buffered_response) = {
            let mut state = self.state.lock().await;
            if !matches!(
                state.options.resolve(None, OptionName::SetClipboard),
                Some("on")
            ) {
                return;
            }
            let Some(pane) =
                StableClipboardPane::capture(&state, runtime_session, pane_id, generation)
            else {
                return;
            };
            match state.options.resolve(None, OptionName::GetClipboard) {
                Some("off") | None => return,
                Some("buffer") => {
                    let content = state
                        .buffers
                        .stack_head()
                        .and_then(|name| state.buffers.get(name))
                        .map(<[u8]>::to_vec);
                    let response = content
                        .as_deref()
                        .and_then(|content| encode_clipboard_response(query, content));
                    let write = response.as_ref().and_then(|response| {
                        let target = pane.resolve(&state)?;
                        prepare_pane_input_write(
                            &mut state,
                            &target,
                            response,
                            PaneInputLiveness::TolerateDead,
                        )
                        .ok()
                    });
                    (pane, None, write.zip(response))
                }
                Some("request") => (pane, Some(ClipboardRequestMode::Request), None),
                Some("both") => (pane, Some(ClipboardRequestMode::Both), None),
                Some(_) => return,
            }
        };

        if let Some((write, response)) = buffered_response {
            let _ = write_attached_bytes_to_target_io(write, response).await;
            return;
        }
        let Some(mode) = mode else {
            return;
        };
        self.request_outer_clipboard(pane, query, mode).await;
    }

    async fn request_outer_clipboard(
        &self,
        pane: StableClipboardPane,
        query: TerminalClipboardQuery,
        mode: ClipboardRequestMode,
    ) {
        let failed_attach = {
            let state = self.state.lock().await;
            if pane.resolve(&state).is_none() {
                return;
            }
            let mut active_attach = self.active_attach.lock().await;
            let mut pending = self
                .clipboard_queries
                .lock()
                .expect("clipboard query mutex must not be poisoned");
            let expired_attaches = pending.expire_at(Instant::now());
            mark_clipboard_attaches_desynchronized(&mut active_attach, &expired_attaches);
            let Some((attach, control_tx)) =
                most_recent_clipboard_attach(&state, &active_attach, pane.window_id)
            else {
                return;
            };
            let Some(token) = pending.register(attach, pane, query, mode, Instant::now()) else {
                return;
            };
            drop(pending);
            let failed_attach = control_tx
                .send(AttachControl::Write(CLIPBOARD_QUERY_SEQUENCE.to_vec()))
                .err()
                .map(|_| attach);
            if failed_attach.is_some() {
                self.clipboard_queries
                    .lock()
                    .expect("clipboard query mutex must not be poisoned")
                    .cancel(token);
            }
            // The send may set the closing latch; release both ordered state
            // locks before common attach cleanup awaits.
            drop(active_attach);
            failed_attach
        };

        if let Some(identity) = failed_attach {
            self.finish_attach(identity.attach_pid(), identity.attach_id())
                .await;
            return;
        }
        self.schedule_clipboard_query_expiration();
    }

    fn schedule_clipboard_query_expiration(&self) {
        let handler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CLIPBOARD_REQUEST_TIMEOUT).await;
            let mut active_attach = handler.active_attach.lock().await;
            let mut pending = handler
                .clipboard_queries
                .lock()
                .expect("clipboard query mutex must not be poisoned");
            let expired_attaches = pending.expire_at(Instant::now());
            mark_clipboard_attaches_desynchronized(&mut active_attach, &expired_attaches);
        });
    }

    pub(in crate::handler) async fn handle_attached_clipboard_response(
        &self,
        identity: ActiveAttachIdentity,
        selection: Option<u8>,
        content: Vec<u8>,
    ) -> io::Result<bool> {
        let pending = {
            let mut active_attach = self.active_attach.lock().await;
            let mut queries = self
                .clipboard_queries
                .lock()
                .expect("clipboard query mutex must not be poisoned");
            let expired_attaches = queries.expire_at(Instant::now());
            mark_clipboard_attaches_desynchronized(&mut active_attach, &expired_attaches);
            if active_attach
                .by_pid
                .get(&identity.attach_pid())
                .filter(|active| identity.matches_active(active))
                .is_some_and(|active| active.clipboard_queries_desynchronized)
            {
                return Ok(false);
            }
            queries.take_for_attach_registration(identity)
        };
        let Some(pending) = pending else {
            return Ok(false);
        };
        let Some(response) =
            encode_clipboard_response_parts(selection, pending.query.terminator(), &content)
        else {
            return Ok(false);
        };

        if pending.mode.stores_response() {
            self.pause_before_clipboard_response_commit_for_test().await;
        }

        if pending.mode.stores_response() {
            let capacity = self
                .pane_mode_post_commit
                .acquire_capacity()
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            let post_commit = capacity.sequence();
            let handler = self.clone();
            let pending_for_store = pending.clone();
            let stored = post_commit
                .run_durable(async move {
                    let transaction = handler.pane_mode_transaction.clone().lock_owned().await;
                    let events = {
                        let mut state = handler.state.lock().await;
                        let active_attach = handler.active_attach.lock().await;
                        if resolve_pending_clipboard_target(
                            &state,
                            &active_attach,
                            identity,
                            &pending_for_store,
                        )
                        .is_none()
                        {
                            return Ok::<_, io::Error>(false);
                        }
                        // tmux 3.7b calls paste_add before input_reply_clipboard in `both`
                        // mode. Keep target revalidation and paste_add in one state ->
                        // active_attach critical section. An access change, attach
                        // replacement, or pane generation change can therefore linearize
                        // only before or after the buffer commit.
                        let outcome = Self::store_buffer_in_state(&mut state, None, content)
                            .map_err(|error| io::Error::other(error.to_string()))?;
                        let (_, events) =
                            Self::prepare_buffer_store_outcome_in_state(&mut state, &outcome);
                        events
                    };
                    drop(transaction);
                    for event in events {
                        handler.emit_prepared(event).await;
                    }
                    Ok::<_, io::Error>(true)
                })
                .await
                .map_err(|error| io::Error::other(error.to_string()))??;
            if !stored {
                return Ok(false);
            }
            self.pause_after_clipboard_response_store_for_test().await;
        }

        let write = {
            let mut state = self.state.lock().await;
            let active_attach = self.active_attach.lock().await;
            let Some(target) =
                resolve_pending_clipboard_target(&state, &active_attach, identity, &pending)
            else {
                return Ok(false);
            };
            prepare_pane_input_write(
                &mut state,
                &target,
                &response,
                PaneInputLiveness::TolerateDead,
            )
            .map_err(|error| io::Error::other(error.to_string()))?
        };

        write_attached_bytes_to_target_io(write, response)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(true)
    }

    #[cfg(test)]
    pub(in crate::handler) fn pending_clipboard_query_count_for_test(&self) -> usize {
        self.clipboard_queries
            .lock()
            .expect("clipboard query mutex must not be poisoned")
            .len()
    }

    #[cfg(test)]
    pub(in crate::handler) fn queued_clipboard_query_count_for_test(&self) -> usize {
        self.clipboard_queries
            .lock()
            .expect("clipboard query mutex must not be poisoned")
            .queued_len()
    }
}
