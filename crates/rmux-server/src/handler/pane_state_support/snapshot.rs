//! Building a pane-state snapshot for a subscription and keeping its response frameable.

use rmux_core::PaneId;
use rmux_proto::{
    encode_frame, PaneOptionEntry, PaneStateSnapshot, PaneStateSubscriptionId, PaneTarget,
    Response, RmuxError, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
};

use crate::foreground_probe::{
    capture_foreground_probe_seed, probe_foreground, ForegroundProbeSeed,
};
use crate::pane_state_journal::PaneStateInclude;
use crate::pane_terminals::HandlerState;

use super::super::RequestHandler;
use super::{pane_target_for_pane_id, PANE_STATE_SNAPSHOT_OPTION_BYTE_LIMIT};

impl RequestHandler {
    pub(super) async fn snapshot_for_subscription(
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

pub(super) fn pane_state_snapshot_locked(
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

pub(super) fn ensure_pane_state_response_frameable(response: &Response) -> Result<(), RmuxError> {
    encode_frame(response).map(|_| ()).map_err(|error| {
        RmuxError::Server(format!(
            "pane state snapshot exceeds the {DEFAULT_MAX_DETACHED_FRAME_LENGTH}-byte detached frame limit: {error}"
        ))
    })
}
