use std::collections::HashMap;
use std::sync::MutexGuard;
use std::time::Duration;

use rmux_core::{OptionMutationOutcome, PaneId};
use rmux_proto::{
    encode_frame, ErrorResponse, ForegroundStateDto, PaneForegroundStateResponse, PaneOptionEntry,
    PaneStateClosedReason, PaneStateCursorRequest, PaneStateCursorResponse, PaneStateLagResponse,
    PaneStateSnapshot, PaneStateSubscriptionId, PaneTarget, Response, RmuxError,
    SubscribePaneStateRequest, SubscribePaneStateResponse, UnsubscribePaneStateRequest,
    UnsubscribePaneStateResponse, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::foreground_probe::{
    capture_foreground_probe_seed, probe_foreground, ForegroundProbeSeed,
};
use crate::pane_state_journal::{
    PaneStateChange, PaneStateInclude, PaneStateJournal, PaneStateRead, PaneStateSubscriptionError,
    PANE_STATE_CURSOR_BATCH,
};
use crate::pane_terminals::HandlerState;

use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;

#[path = "handler_pane_state/foreground_watch.rs"]
mod foreground_watch;

#[cfg(not(test))]
const PANE_STATE_WAIT_CAP: Duration = Duration::from_secs(25);
#[cfg(test)]
const PANE_STATE_WAIT_CAP: Duration = Duration::from_millis(50);
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
const FOREGROUND_MAX_PANES_PER_TICK: usize = 32;
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
        let close_reason = match &change {
            PaneStateChange::Closed { reason } => Some(*reason),
            _ => None,
        };
        {
            let mut journal = self.lock_pane_state_journal();
            if closes_pane && !journal.mark_pane_closed(pane_id) {
                return;
            }
            let revision = journal.push(pane_id, generation, change);
            if let Some(reason) = close_reason {
                journal.remember_pane_closed_event(pane_id, reason, revision);
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
                    },
                );
                journal.remember_pane_closed_event(
                    pane_id,
                    PaneStateClosedReason::Killed,
                    revision,
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

    async fn snapshot_for_subscription(
        &self,
        connection_id: u64,
        subscription_id: PaneStateSubscriptionId,
    ) -> Result<PaneStateSnapshot, RmuxError> {
        let (mut snapshot, foreground_seed) = {
            let state = self.state.lock().await;
            // Pane lifecycle mutations use the same state -> journal lock
            // order. Reading both while state is held prevents a kill from
            // leaving an open subscription paired with an already-removed
            // pane during lag rebase.
            loop {
                let (info, revision) = {
                    let journal = self.lock_pane_state_journal();
                    let info = journal
                        .subscription_info(connection_id, subscription_id)
                        .map_err(|message| RmuxError::Server(message.to_owned()))?
                        .ok_or_else(|| RmuxError::Server("subscription not found".to_owned()))?;
                    (info, journal.current_revision())
                };
                let closed_snapshot_revision = info
                    .closed
                    .then(|| info.closed_revision.unwrap_or(revision).saturating_sub(1));

                let snapshot = match pane_target_for_pane_id(&state, info.pane_id) {
                    Some(target)
                        if info.closed
                            && info.generation.is_some_and(|generation| {
                                generation
                                    != state
                                        .pane_output_generation_for_target(&target, info.pane_id)
                            }) =>
                    {
                        (
                            PaneStateSnapshot {
                                revision: closed_snapshot_revision.unwrap_or(revision),
                                title: info.include.title.then(String::new),
                                options: Vec::new(),
                                foreground: None,
                            },
                            None,
                        )
                    }
                    Some(target) => {
                        let (mut snapshot, foreground_seed) = pane_state_snapshot_locked(
                            &state,
                            &target,
                            info.pane_id,
                            info.include,
                            closed_snapshot_revision.unwrap_or(revision),
                        )?;
                        if info.closed {
                            snapshot.foreground = None;
                            (snapshot, None)
                        } else {
                            (snapshot, foreground_seed)
                        }
                    }
                    None if info.closed => (
                        PaneStateSnapshot {
                            revision: closed_snapshot_revision.unwrap_or(revision),
                            title: info.include.title.then(String::new),
                            options: Vec::new(),
                            foreground: None,
                        },
                        None,
                    ),
                    None => {
                        return Err(RmuxError::Server(format!(
                            "pane {} not found",
                            info.pane_id.as_u32()
                        )))
                    }
                };
                if info.closed || self.lock_pane_state_journal().current_revision() == revision {
                    break snapshot;
                }
            }
        };
        if let Some(seed) = foreground_seed {
            let foreground = probe_foreground(&seed);
            self.seed_foreground_state_cache(seed.pane_id(), seed.generation(), foreground.clone());
            snapshot.foreground = Some(foreground);
        }
        Ok(snapshot)
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

fn pane_state_snapshot_locked(
    state: &HandlerState,
    target: &PaneTarget,
    pane_id: PaneId,
    include: PaneStateInclude,
    revision: u64,
) -> Result<(PaneStateSnapshot, Option<ForegroundProbeSeed>), RmuxError> {
    let title = include.title.then(|| {
        state
            .pane_screen_state(target.session_name(), pane_id)
            .map(|screen| screen.title)
            .unwrap_or_default()
    });
    let options = if include.options {
        let entries = state.pane_explicit_option_entries(target)?;
        let mut option_bytes = 0_usize;
        let mut options = Vec::with_capacity(entries.len());
        for (name, value) in entries {
            option_bytes = option_bytes
                .saturating_add(name.len())
                .saturating_add(value.len());
            if option_bytes > PANE_STATE_SNAPSHOT_OPTION_BYTE_LIMIT {
                return Err(RmuxError::Server(format!(
                    "pane state snapshot options exceed {PANE_STATE_SNAPSHOT_OPTION_BYTE_LIMIT} bytes"
                )));
            }
            options.push(PaneOptionEntry { name, value });
        }
        options
    } else {
        Vec::new()
    };
    let foreground_seed = include
        .foreground
        .then(|| capture_foreground_probe_seed(state, target).ok())
        .flatten();

    Ok((
        PaneStateSnapshot {
            revision,
            title,
            options,
            foreground: None,
        },
        foreground_seed,
    ))
}

fn ensure_pane_state_response_frameable(response: &Response) -> Result<(), RmuxError> {
    encode_frame(response).map(|_| ()).map_err(|error| {
        RmuxError::Server(format!(
            "pane state snapshot exceeds the {DEFAULT_MAX_DETACHED_FRAME_LENGTH}-byte detached frame limit: {error}"
        ))
    })
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

fn foreground_state_changed(previous: &ForegroundStateDto, next: &ForegroundStateDto) -> bool {
    previous.pid != next.pid
        || previous.command != next.command
        || previous.cwd != next.cwd
        || previous.exe != next.exe
}

fn pane_state_events_include_closed(events: &[rmux_proto::PaneStateEventDto]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, rmux_proto::PaneStateEventDto::Closed { .. }))
}

fn foreground_change_from_previous(
    previous: Option<(u64, ForegroundStateDto)>,
    generation: u64,
    next: &ForegroundStateDto,
) -> Option<ForegroundStateDto> {
    match previous {
        Some((previous_generation, previous))
            if previous_generation == generation && foreground_state_changed(&previous, next) =>
        {
            Some(previous)
        }
        _ => None,
    }
}

fn foreground_poll_batch(pane_ids: &[PaneId], cursor: &mut usize) -> Vec<PaneId> {
    if pane_ids.is_empty() {
        return Vec::new();
    }

    let count = pane_ids.len().min(FOREGROUND_MAX_PANES_PER_TICK);
    let start = *cursor % pane_ids.len();
    let mut batch = Vec::with_capacity(count);
    for offset in 0..count {
        batch.push(pane_ids[(start + offset) % pane_ids.len()]);
    }
    *cursor = (start + count) % pane_ids.len();
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn foreground_state(pid: u32, command: &str, cwd: &str) -> ForegroundStateDto {
        ForegroundStateDto {
            pid: Some(pid),
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            exe: None,
            sources: rmux_proto::ForegroundSourcesDto::default(),
        }
    }

    fn foreground_state_with_exe(
        pid: u32,
        command: &str,
        cwd: &str,
        exe: &str,
    ) -> ForegroundStateDto {
        ForegroundStateDto {
            exe: Some(exe.to_owned()),
            sources: rmux_proto::ForegroundSourcesDto {
                exe: Some(rmux_proto::ForegroundFieldSource::Process),
                ..Default::default()
            },
            ..foreground_state(pid, command, cwd)
        }
    }

    #[test]
    fn foreground_poll_batch_rotates_beyond_first_thirty_two_panes() {
        let pane_ids = (1..=40).map(PaneId::new).collect::<Vec<_>>();
        let mut cursor = 0;

        let first = foreground_poll_batch(&pane_ids, &mut cursor);
        let second = foreground_poll_batch(&pane_ids, &mut cursor);

        assert_eq!(first.len(), FOREGROUND_MAX_PANES_PER_TICK);
        assert_eq!(first[0], PaneId::new(1));
        assert!(first.contains(&PaneId::new(32)));
        assert!(!first.contains(&PaneId::new(33)));
        assert_eq!(second[0], PaneId::new(33));
        assert!(second.contains(&PaneId::new(40)));
        assert!(second.contains(&PaneId::new(1)));
    }

    #[test]
    fn seeded_foreground_cache_exposes_first_transition() {
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(7);
        let old_state = foreground_state(10, "cmd", "C:/old");
        let new_state = foreground_state(10, "cmd", "C:/new");

        handler.seed_foreground_state_cache(pane_id, 3, old_state.clone());
        handler.seed_foreground_state_cache(pane_id, 3, new_state.clone());
        let previous = handler
            .replace_foreground_state_cache(pane_id, 3, new_state.clone())
            .expect("seeded snapshot baseline is preserved");

        assert_eq!(previous, (3, old_state.clone()));
        assert_eq!(
            foreground_change_from_previous(Some(previous), 3, &new_state),
            Some(old_state)
        );
    }

    #[test]
    fn stale_foreground_seed_does_not_replace_newer_generation() {
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(8);
        let old_state = foreground_state(10, "cmd", "C:/old");
        let new_state = foreground_state(10, "cmd", "C:/new");
        let latest_state = foreground_state(10, "cmd", "C:/latest");

        handler.seed_foreground_state_cache(pane_id, 5, new_state.clone());
        handler.seed_foreground_state_cache(pane_id, 4, old_state);
        let previous = handler
            .replace_foreground_state_cache(pane_id, 5, latest_state)
            .expect("newer baseline should survive stale seed");

        assert_eq!(previous, (5, new_state));
    }

    #[test]
    fn stale_foreground_replace_does_not_overwrite_newer_generation() {
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(9);
        let stale_state = foreground_state(10, "cmd", "C:/stale");
        let current_state = foreground_state(11, "cmd", "C:/current");

        handler.seed_foreground_state_cache(pane_id, 5, current_state.clone());
        let previous = handler.replace_foreground_state_cache(pane_id, 4, stale_state);

        assert_eq!(previous, None);
        assert_eq!(
            handler.lock_foreground_state_cache().get(&pane_id).cloned(),
            Some((5, current_state))
        );
    }

    #[test]
    fn same_generation_foreground_replace_updates_cache() {
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(10);
        let old_state = foreground_state(10, "cmd", "C:/old");
        let next_state = foreground_state(10, "cmd", "C:/next");

        handler.seed_foreground_state_cache(pane_id, 5, old_state.clone());
        let previous = handler.replace_foreground_state_cache(pane_id, 5, next_state.clone());

        assert_eq!(previous, Some((5, old_state)));
        assert_eq!(
            handler.lock_foreground_state_cache().get(&pane_id).cloned(),
            Some((5, next_state))
        );
    }

    #[test]
    fn newer_generation_foreground_replace_advances_cache_without_transition() {
        let handler = RequestHandler::new();
        let pane_id = PaneId::new(11);
        let old_state = foreground_state(10, "cmd", "C:/old");
        let next_state = foreground_state(11, "cmd", "C:/next");

        handler.seed_foreground_state_cache(pane_id, 5, old_state.clone());
        let previous = handler.replace_foreground_state_cache(pane_id, 6, next_state.clone());

        assert_eq!(previous, Some((5, old_state)));
        assert_eq!(
            foreground_change_from_previous(previous, 6, &next_state),
            None
        );
        assert_eq!(
            handler.lock_foreground_state_cache().get(&pane_id).cloned(),
            Some((6, next_state))
        );
    }

    #[test]
    fn foreground_cache_ignores_generation_reset_as_transition() {
        let old_state = foreground_state(10, "cmd", "C:/old");
        let new_state = foreground_state(11, "cmd", "C:/new");

        assert_eq!(
            foreground_change_from_previous(Some((2, old_state)), 3, &new_state),
            None
        );
    }

    #[test]
    fn foreground_state_changed_observes_executable_path() {
        let old_state = foreground_state_with_exe(10, "cmd", "C:/work", "C:/Windows/cmd.exe");
        let new_state = foreground_state_with_exe(10, "cmd", "C:/work", "C:/Tools/cmd.exe");

        assert!(foreground_state_changed(&old_state, &new_state));
    }
}
