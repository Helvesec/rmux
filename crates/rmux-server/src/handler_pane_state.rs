use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::MutexGuard;
use std::time::Duration;

use rmux_core::{OptionMutationOutcome, PaneId};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, PaneForegroundStateResponse, PaneOptionEntry, PaneStateCursorRequest,
    PaneStateCursorResponse, PaneStateLagResponse, PaneStateSnapshot, PaneStateSubscriptionId,
    PaneTarget, Response, RmuxError, SubscribePaneStateRequest, SubscribePaneStateResponse,
    UnsubscribePaneStateRequest, UnsubscribePaneStateResponse,
};

use crate::foreground_probe::{
    capture_foreground_probe_seed, probe_foreground, ForegroundProbeSeed,
};
use crate::pane_state_journal::{
    PaneStateChange, PaneStateInclude, PaneStateJournal, PaneStateRead, PANE_STATE_CURSOR_BATCH,
};
use crate::pane_terminals::HandlerState;

use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;

const PANE_STATE_WAIT_CAP: Duration = Duration::from_secs(25);
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
const FOREGROUND_MAX_PANES_PER_TICK: usize = 32;

impl RequestHandler {
    fn lock_pane_state_journal(&self) -> MutexGuard<'_, PaneStateJournal> {
        self.pane_state_journal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
            let mut journal = self.lock_pane_state_journal();
            let revision = journal.current_revision();
            let subscription_id = journal.subscribe(connection_id, pane_id, include);
            let (snapshot, foreground_seed) =
                pane_state_snapshot_locked(&state, &target, pane_id, include, revision);
            (subscription_id, pane_id, snapshot, foreground_seed)
        };

        if let Some(seed) = foreground_seed {
            snapshot.foreground = Some(probe_foreground(&seed));
        }
        if include.foreground {
            self.start_foreground_watch_if_needed();
        }

        Response::SubscribePaneState(Box::new(SubscribePaneStateResponse {
            subscription_id,
            pane_id,
            snapshot,
        }))
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

        loop {
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
                    return Response::PaneStateCursor(PaneStateCursorResponse {
                        subscription_id: request.subscription_id,
                        events,
                        next_revision,
                    });
                }
                Ok(PaneStateRead::Ready { .. }) => {
                    let notified = self.pane_state_notify.notified();
                    if tokio::time::timeout(PANE_STATE_WAIT_CAP, notified)
                        .await
                        .is_err()
                    {
                        return Response::PaneStateCursor(PaneStateCursorResponse {
                            subscription_id: request.subscription_id,
                            events: Vec::new(),
                            next_revision: request.after_revision,
                        });
                    }
                }
                Ok(PaneStateRead::Lag {
                    missed_from_revision,
                    resume_revision,
                }) => {
                    return match self
                        .snapshot_for_subscription(connection_id, request.subscription_id)
                        .await
                    {
                        Ok(snapshot) => Response::PaneStateLag(Box::new(PaneStateLagResponse {
                            subscription_id: request.subscription_id,
                            missed_from_revision,
                            resume_revision,
                            snapshot,
                        })),
                        Err(error) => Response::Error(ErrorResponse { error }),
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
        {
            let mut journal = self.lock_pane_state_journal();
            journal.push(pane_id, generation, change);
            if closes_pane {
                journal.mark_pane_closed(pane_id);
            }
        }
        self.pane_state_notify.notify_waiters();
    }

    fn start_foreground_watch_if_needed(&self) {
        if self
            .foreground_watch_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let handler = self.clone();
        tokio::spawn(async move {
            handler.watch_foreground_subscriptions().await;
        });
    }

    async fn watch_foreground_subscriptions(&self) {
        let mut cache: HashMap<PaneId, (u64, rmux_proto::ForegroundStateDto)> = HashMap::new();
        loop {
            let pane_ids = {
                let journal = self.lock_pane_state_journal();
                if journal.foreground_subscription_count() == 0 {
                    self.foreground_watch_started
                        .store(false, Ordering::Release);
                    if journal.foreground_subscription_count() == 0 {
                        return;
                    }
                    if self
                        .foreground_watch_started
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        return;
                    }
                }
                journal.pane_ids_with_foreground_subscriptions()
            };

            for pane_id in pane_ids.into_iter().take(FOREGROUND_MAX_PANES_PER_TICK) {
                let seed = {
                    let state = self.state.lock().await;
                    let Some(target) = pane_target_for_pane_id(&state, pane_id) else {
                        continue;
                    };
                    match capture_foreground_probe_seed(&state, &target) {
                        Ok(seed) => seed,
                        Err(_) => continue,
                    }
                };
                let next = probe_foreground(&seed);
                match cache.insert(pane_id, (seed.generation(), next.clone())) {
                    Some((generation, previous))
                        if generation == seed.generation()
                            && foreground_state_changed(&previous, &next) =>
                    {
                        self.record_pane_state_change(
                            pane_id,
                            Some(seed.generation()),
                            PaneStateChange::ForegroundChanged {
                                old: previous,
                                new: next,
                            },
                        );
                    }
                    _ => {}
                }
            }

            tokio::time::sleep(FOREGROUND_POLL_INTERVAL).await;
        }
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

    async fn snapshot_for_subscription(
        &self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
    ) -> Result<PaneStateSnapshot, RmuxError> {
        let info = {
            let journal = self.lock_pane_state_journal();
            journal
                .subscription_info(connection_id, subscription_id)
                .map_err(|message| RmuxError::Server(message.to_owned()))?
                .ok_or_else(|| RmuxError::Server("subscription not found".to_owned()))?
        };

        let (mut snapshot, foreground_seed) = {
            let state = self.state.lock().await;
            let target = pane_target_for_pane_id(&state, info.pane_id).ok_or_else(|| {
                RmuxError::Server(format!("pane {} not found", info.pane_id.as_u32()))
            })?;
            let revision = self.lock_pane_state_journal().current_revision();
            pane_state_snapshot_locked(&state, &target, info.pane_id, info.include, revision)
        };
        if let Some(seed) = foreground_seed {
            snapshot.foreground = Some(probe_foreground(&seed));
        }
        Ok(snapshot)
    }
}

fn pane_state_snapshot_locked(
    state: &HandlerState,
    target: &PaneTarget,
    pane_id: PaneId,
    include: PaneStateInclude,
    revision: u64,
) -> (PaneStateSnapshot, Option<ForegroundProbeSeed>) {
    let title = include.title.then(|| {
        state
            .pane_screen_state(target.session_name(), pane_id)
            .map(|screen| screen.title)
            .unwrap_or_default()
    });
    let options = if include.options {
        state
            .options
            .explicit_entries_for_scope(&OptionScopeSelector::Pane(target.clone()))
            .into_iter()
            .map(|(name, value)| PaneOptionEntry { name, value })
            .collect()
    } else {
        Vec::new()
    };
    let foreground_seed = include
        .foreground
        .then(|| capture_foreground_probe_seed(state, target).ok())
        .flatten();

    (
        PaneStateSnapshot {
            revision,
            title,
            options,
            foreground: None,
        },
        foreground_seed,
    )
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

fn foreground_state_changed(
    previous: &rmux_proto::ForegroundStateDto,
    next: &rmux_proto::ForegroundStateDto,
) -> bool {
    previous.pid != next.pid || previous.command != next.command || previous.cwd != next.cwd
}
