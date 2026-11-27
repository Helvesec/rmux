use std::collections::HashMap;
use std::sync::MutexGuard;
use std::time::Duration;

use rmux_core::{OptionMutationOutcome, PaneId};
use rmux_proto::{
    ErrorResponse, ForegroundStateDto, PaneForegroundStateResponse, PaneStateClosedReason,
    PaneStateCursorRequest, PaneStateCursorResponse, PaneStateLagResponse, PaneStateSubscriptionId,
    PaneTarget, Response, RmuxError, SubscribePaneStateRequest, SubscribePaneStateResponse,
    UnsubscribePaneStateRequest, UnsubscribePaneStateResponse, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::foreground_probe::{capture_foreground_probe_seed, probe_foreground};
use crate::pane_state_journal::{
    PaneStateChange, PaneStateInclude, PaneStateJournal, PaneStateRead, PaneStateSubscriptionError,
    PANE_STATE_CURSOR_BATCH,
};
use crate::pane_terminals::HandlerState;

use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;

mod foreground_watch;
mod snapshot;

use self::snapshot::{ensure_pane_state_response_frameable, pane_state_snapshot_locked};

#[cfg(not(test))]
const PANE_STATE_WAIT_CAP: Duration = Duration::from_secs(25);
#[cfg(test)]
const PANE_STATE_WAIT_CAP: Duration = Duration::from_millis(50);
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub(super) const PANE_STATE_SNAPSHOT_OPTION_BYTE_LIMIT: usize =
    DEFAULT_MAX_DETACHED_FRAME_LENGTH / 2;

impl RequestHandler {
    fn lock_pane_state_journal(&self) -> MutexGuard<'_, PaneStateJournal> {
        self.pane_state_journal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn lock_foreground_state_cache(
        &self,
    ) -> MutexGuard<'_, HashMap<PaneId, (u64, ForegroundStateDto)>> {
        self.foreground_state_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn seed_foreground_state_cache(
        &self,
        pane_id: PaneId,
        generation: u64,
        state: ForegroundStateDto,
    ) {
        let mut cache = self.lock_foreground_state_cache();
        match cache.entry(pane_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((generation, state));
            }
            std::collections::hash_map::Entry::Occupied(mut entry)
                if generation > entry.get().0 =>
            {
                entry.insert((generation, state));
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
        }
    }

    fn replace_foreground_state_cache(
        &self,
        pane_id: PaneId,
        generation: u64,
        state: ForegroundStateDto,
    ) -> Option<(u64, ForegroundStateDto)> {
        let mut cache = self.lock_foreground_state_cache();
        match cache.entry(pane_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((generation, state));
                None
            }
            std::collections::hash_map::Entry::Occupied(mut entry)
                if generation >= entry.get().0 =>
            {
                Some(entry.insert((generation, state)))
            }
            std::collections::hash_map::Entry::Occupied(_) => None,
        }
    }

    fn remove_foreground_state_cache(&self, pane_id: PaneId) {
        self.lock_foreground_state_cache().remove(&pane_id);
    }

    fn clear_foreground_state_cache(&self) {
        self.lock_foreground_state_cache().clear();
    }

    pub(in crate::handler) async fn handle_subscribe_pane_state(
        &self,
        connection_id: u64,
        request: SubscribePaneStateRequest,
    ) -> Response {
        let include = PaneStateInclude {
            title: request.include_title,
            options: request.include_options,
            foreground: request.include_foreground,
        };
        // Reserve ownership before the subscription becomes observable. The
        // registration itself keeps shutdown waiting for this hand-off; no
        // mutation guard is needed (or wanted) while the process probe runs.
        let mut foreground_watch_registration = if include.foreground {
            match self.reserve_foreground_watch() {
                Ok(registration) => Some(registration),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };
        let (subscription_id, pane_id, mut snapshot, foreground_seed) = {
            let state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let pane_id = match pane_id_for_target(&state, &target) {
                Ok(pane_id) => pane_id,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            // Pane-state producers are driven by pane output and stamp this
            // generation. Lifecycle generations also advance on exit, so
            // mixing the two domains would strand subscriptions after a
            // kept-dead pane is respawned.
            let generation = state.pane_output_generation_for_target(&target, pane_id);
            loop {
                let (subscription_id, revision) = {
                    let mut journal = self.lock_pane_state_journal();
                    let revision = journal.current_revision();
                    let subscription_id = match journal.subscribe_at_generation(
                        connection_id,
                        pane_id,
                        include,
                        Some(generation),
                    ) {
                        Ok(subscription_id) => subscription_id,
                        Err(error) => {
                            return Response::Error(ErrorResponse {
                                error: pane_state_subscription_error(error),
                            });
                        }
                    };
                    (subscription_id, revision)
                };
                let (snapshot, foreground_seed) =
                    match pane_state_snapshot_locked(&state, &target, pane_id, include, revision) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            let _ = self
                                .lock_pane_state_journal()
                                .unsubscribe(connection_id, subscription_id);
                            return Response::Error(ErrorResponse { error });
                        }
                    };
                let mut journal = self.lock_pane_state_journal();
                if journal.current_revision() == revision {
                    break (subscription_id, pane_id, snapshot, foreground_seed);
                }
                let _ = journal.unsubscribe(connection_id, subscription_id);
            }
        };

        if let Some(seed) = foreground_seed {
            let foreground = probe_foreground(&seed);
            self.seed_foreground_state_cache(seed.pane_id(), seed.generation(), foreground.clone());
            snapshot.foreground = Some(foreground);
        }
        let response = Response::SubscribePaneState(Box::new(SubscribePaneStateResponse {
            subscription_id,
            pane_id,
            snapshot,
        }));
        if let Err(error) = ensure_pane_state_response_frameable(&response) {
            self.remove_pane_state_subscription(connection_id, subscription_id);
            return Response::Error(ErrorResponse { error });
        }
        if include.foreground {
            self.start_foreground_watch_if_needed(
                foreground_watch_registration
                    .take()
                    .expect("foreground watcher registration was reserved"),
            );
        }
        response
    }

    pub(in crate::handler) async fn handle_unsubscribe_pane_state(
        &self,
        connection_id: u64,
        request: UnsubscribePaneStateRequest,
    ) -> Response {
        let removed = {
            let mut journal = self.lock_pane_state_journal();
            match journal.unsubscribe(connection_id, request.subscription_id) {
                Ok(removed) => removed,
                Err(message) => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(message.to_owned()),
                    });
                }
            }
        };
        self.pane_state_notify.notify_waiters();
        Response::UnsubscribePaneState(UnsubscribePaneStateResponse {
            subscription_id: request.subscription_id,
            removed,
        })
    }

    pub(in crate::handler) async fn handle_pane_state_cursor(
        &self,
        connection_id: u64,
        request: PaneStateCursorRequest,
    ) -> Response {
        let limit = match state_cursor_limit(request.max_events) {
            Ok(limit) => limit,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        let wait_deadline = request
            .wait
            .then(|| tokio::time::Instant::now() + PANE_STATE_WAIT_CAP);

        loop {
            let notified = self.pane_state_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut events = Vec::new();
            let read = {
                let journal = self.lock_pane_state_journal();
                journal.read_after(
                    connection_id,
                    request.subscription_id,
                    request.after_revision,
                    limit,
                    &mut events,
                )
            };

            match read {
                Ok(PaneStateRead::Ready {
                    next_revision,
                    event_count,
                    ..
                }) if event_count > 0 || !request.wait => {
                    if pane_state_events_include_closed(&events) {
                        self.lock_pane_state_journal()
                            .remove_closed_subscription(connection_id, request.subscription_id);
                        self.pane_state_notify.notify_waiters();
                    }
                    return Response::PaneStateCursor(PaneStateCursorResponse {
                        subscription_id: request.subscription_id,
                        events,
                        next_revision,
                    });
                }
                Ok(PaneStateRead::Ready { next_revision, .. }) => {
                    let Some(deadline) = wait_deadline else {
                        return Response::PaneStateCursor(PaneStateCursorResponse {
                            subscription_id: request.subscription_id,
                            events: Vec::new(),
                            next_revision,
                        });
                    };
                    if tokio::time::timeout_at(deadline, notified.as_mut())
                        .await
                        .is_err()
                    {
                        return Response::PaneStateCursor(PaneStateCursorResponse {
                            subscription_id: request.subscription_id,
                            events: Vec::new(),
                            next_revision,
                        });
                    }
                }
                Ok(PaneStateRead::Lag {
                    missed_from_revision,
                    resume_revision,
                }) => {
                    self.pause_before_pane_state_lag_snapshot().await;
                    return match self
                        .snapshot_for_subscription(connection_id, request.subscription_id)
                        .await
                    {
                        Ok(snapshot) => {
                            let response = Response::PaneStateLag(Box::new(PaneStateLagResponse {
                                subscription_id: request.subscription_id,
                                missed_from_revision,
                                resume_revision,
                                snapshot,
                            }));
                            match ensure_pane_state_response_frameable(&response) {
                                Ok(()) => response,
                                Err(error) => {
                                    self.remove_pane_state_subscription(
                                        connection_id,
                                        request.subscription_id,
                                    );
                                    Response::Error(ErrorResponse { error })
                                }
                            }
                        }
                        Err(error) => {
                            self.remove_pane_state_subscription(
                                connection_id,
                                request.subscription_id,
                            );
                            Response::Error(ErrorResponse { error })
                        }
                    };
                }
                Err(message) => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(message.to_owned()),
                    });
                }
            }
        }
    }

    pub(in crate::handler) async fn handle_pane_foreground_state(
        &self,
        request: rmux_proto::PaneForegroundStateRequest,
    ) -> Response {
        let (pane_id, revision, foreground_seed) = {
            let state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let seed = match capture_foreground_probe_seed(&state, &target) {
                Ok(seed) => seed,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let revision = self.lock_pane_state_journal().current_revision();
            (seed.pane_id(), revision, seed)
        };

        Response::PaneForegroundState(Box::new(PaneForegroundStateResponse {
            pane_id,
            revision,
            state: Some(probe_foreground(&foreground_seed)),
        }))
    }

    pub(in crate::handler) fn record_pane_state_change(
        &self,
        pane_id: PaneId,
        generation: Option<u64>,
        change: PaneStateChange,
    ) {
        let closes_pane = matches!(change, PaneStateChange::Closed { .. });
        let close_details = match &change {
            PaneStateChange::Closed {
                reason,
                exit_status,
                exit_signal,
            } => Some((*reason, *exit_status, *exit_signal)),
            _ => None,
        };
        {
            let mut journal = self.lock_pane_state_journal();
            if closes_pane && !journal.mark_pane_closed(pane_id) {
                return;
            }
            let revision = journal.push(pane_id, generation, change);
            if let Some((reason, exit_status, exit_signal)) = close_details {
                journal.remember_pane_closed_event(
                    pane_id,
                    reason,
                    revision,
                    exit_status,
                    exit_signal,
                );
            }
        }
        if closes_pane {
            self.remove_foreground_state_cache(pane_id);
        }
        self.pane_state_notify.notify_waiters();
    }

    #[cfg(test)]
    pub(in crate::handler) fn reopen_pane_state(&self, pane_id: PaneId) {
        self.lock_pane_state_journal().reopen_pane(pane_id);
    }

    pub(in crate::handler) fn record_pane_respawn_boundary(&self, pane_id: PaneId) {
        let recorded_close = {
            let mut journal = self.lock_pane_state_journal();
            let recorded_close = journal.mark_pane_closed(pane_id);
            if recorded_close {
                let revision = journal.push(
                    pane_id,
                    None,
                    PaneStateChange::Closed {
                        reason: PaneStateClosedReason::Killed,
                        exit_status: None,
                        exit_signal: None,
                    },
                );
                journal.remember_pane_closed_event(
                    pane_id,
                    PaneStateClosedReason::Killed,
                    revision,
                    None,
                    None,
                );
            }
            journal.reopen_pane(pane_id);
            recorded_close
        };
        self.remove_foreground_state_cache(pane_id);
        if recorded_close {
            self.pane_state_notify.notify_waiters();
        }
    }

    pub(in crate::handler) fn record_panes_closed_as_killed(&self, pane_ids: &[PaneId]) {
        for pane_id in pane_ids {
            self.record_pane_state_change(
                *pane_id,
                None,
                PaneStateChange::Closed {
                    reason: PaneStateClosedReason::Killed,
                    exit_status: None,
                    exit_signal: None,
                },
            );
        }
        self.prune_web_panes(pane_ids);
    }

    pub(in crate::handler) fn record_pane_option_mutation(
        &self,
        pane_id: PaneId,
        generation: Option<u64>,
        outcome: &OptionMutationOutcome,
    ) {
        if !outcome.changed {
            return;
        }
        let change = match outcome.new_explicit.clone() {
            Some(new) => PaneStateChange::OptionSet {
                name: outcome.name.clone(),
                old: outcome.old_explicit.clone(),
                new,
            },
            None => PaneStateChange::OptionUnset {
                name: outcome.name.clone(),
                old: outcome.old_explicit.clone(),
            },
        };
        self.record_pane_state_change(pane_id, generation, change);
    }

    pub(crate) fn cleanup_connection_pane_state_subscriptions_sync(&self, connection_id: u64) {
        {
            let mut journal = self.lock_pane_state_journal();
            journal.remove_connection(connection_id);
        }
        self.pane_state_notify.notify_waiters();
    }

    /// Marks a negotiated connection as wanting the exit-bearing terminal
    /// event (`sdk.pane.state_events.exit`, required from the connection's
    /// handshake). The pane-state journal shapes its terminal events per
    /// connection.
    pub(crate) fn set_connection_pane_state_exit_events(&self, connection_id: u64, enabled: bool) {
        self.lock_pane_state_journal()
            .set_connection_exit_events(connection_id, enabled);
    }

    fn remove_pane_state_subscription(
        &self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
    ) {
        let _ = self
            .lock_pane_state_journal()
            .unsubscribe(connection_id, subscription_id);
        self.pane_state_notify.notify_waiters();
    }

    pub(in crate::handler) fn pane_state_has_title_subscriptions(&self) -> bool {
        self.lock_pane_state_journal().title_subscription_count() > 0
    }
}

fn pane_state_subscription_error(error: PaneStateSubscriptionError) -> RmuxError {
    match error {
        PaneStateSubscriptionError::Limit(error) => {
            super::subscription_support::pane_state_subscription_limit_error(error)
        }
        PaneStateSubscriptionError::Capacity { limit } => RmuxError::Server(format!(
            "pane state subscription capacity exceeded (limit {limit})"
        )),
    }
}

fn pane_id_for_target(state: &HandlerState, target: &PaneTarget) -> Result<PaneId, RmuxError> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })
}

fn pane_target_for_pane_id(state: &HandlerState, pane_id: PaneId) -> Option<PaneTarget> {
    let mut sessions = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .map(|(session_name, _)| session_name.clone())
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));

    for session_name in sessions {
        let Some(session) = state.sessions.session(&session_name) else {
            continue;
        };
        let Some(window_index) = session.window_index_for_pane_id(pane_id) else {
            continue;
        };
        let Some(pane_index) = session.window_at(window_index).and_then(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id() == pane_id)
                .map(|pane| pane.index())
        }) else {
            continue;
        };
        return Some(PaneTarget::with_window(
            session_name,
            window_index,
            pane_index,
        ));
    }
    None
}

fn state_cursor_limit(requested: Option<u16>) -> Result<usize, RmuxError> {
    match requested {
        Some(0) => Err(RmuxError::Server(
            "pane state cursor max_events must be greater than zero".to_owned(),
        )),
        Some(value) => Ok(usize::from(value).min(PANE_STATE_CURSOR_BATCH)),
        None => Ok(PANE_STATE_CURSOR_BATCH),
    }
}

fn pane_state_events_include_closed(events: &[rmux_proto::PaneStateEventDto]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            rmux_proto::PaneStateEventDto::Closed { .. }
                | rmux_proto::PaneStateEventDto::Exited { .. }
        )
    })
}
