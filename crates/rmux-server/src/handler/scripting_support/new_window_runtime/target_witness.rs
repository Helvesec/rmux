//! The target witness a queued `new-window` captures at admission and revalidates before mutation.

use rmux_core::ManagedSessionStore;
use rmux_proto::{RmuxError, SessionName, Target, WindowTarget};

use super::super::queue_parse::{NewWindowPlacement, ParsedNewWindowCommand};
use super::super::targets::NewWindowTargetIndex;
use super::selection;
use crate::handler::StableTargetIdentity;
use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone)]
pub(in crate::handler) struct QueuedNewWindowTargetWitness {
    session_name: SessionName,
    session: StableTargetIdentity,
    pub(super) resolved_window_index: Option<u32>,
    anchor: Option<StableTargetIdentity>,
    destination: Option<WindowSlotWitness>,
    // Keep both the match and lookup failure across waits. Access policy still
    // takes precedence, but execution must never repeat the name lookup.
    pub(super) existing_selection: Result<Option<(WindowTarget, StableTargetIdentity)>, RmuxError>,
}

#[derive(Debug, Clone)]
enum WindowSlotWitness {
    Vacant(u32),
    Occupied(StableTargetIdentity),
}

impl QueuedNewWindowTargetWitness {
    pub(in crate::handler::scripting_support) fn capture(
        state: &mut HandlerState,
        command: &ParsedNewWindowCommand,
    ) -> Result<Self, RmuxError> {
        let active_window_index = state
            .sessions
            .session(&command.target)
            .ok_or_else(|| RmuxError::SessionNotFound(command.target.to_string()))?
            .active_window_index();
        let session =
            StableTargetIdentity::capture(state, Target::Session(command.target.clone()))?;
        let resolved_window_index = resolve_queued_new_window_target_index(
            &state.sessions,
            &command.target,
            command.target_window_index,
        )?;
        let anchor_index = match (command.placement, command.target_window_index) {
            (Some(NewWindowPlacement::Before), _) => resolved_window_index,
            (Some(NewWindowPlacement::After), _) => Some(
                resolved_window_index
                    .and_then(|index| index.checked_sub(1))
                    .ok_or_else(|| {
                        RmuxError::Server("new-window placement anchor underflowed".to_owned())
                    })?,
            ),
            (None, Some(NewWindowTargetIndex::Relative(_))) => Some(active_window_index),
            (None, _) => None,
        };
        let anchor = anchor_index
            .map(|index| {
                StableTargetIdentity::capture(
                    state,
                    Target::Window(WindowTarget::with_window(command.target.clone(), index)),
                )
            })
            .transpose()?;
        let destination = resolved_window_index
            .map(|index| capture_window_slot(state, &command.target, index))
            .transpose()?;
        let existing_selection = if command.select_existing && resolved_window_index.is_none() {
            command.name.as_deref().map_or(Ok(None), |name| {
                selection::find_existing_new_window_by_name(state, &command.target, name)
            })
        } else {
            Ok(None)
        };
        Ok(Self {
            existing_selection,
            session_name: command.target.clone(),
            session,
            resolved_window_index,
            anchor,
            destination,
        })
    }

    pub(super) fn validate(&self, state: &HandlerState) -> Result<(), RmuxError> {
        if !self.session.is_current(state)
            || self
                .anchor
                .as_ref()
                .is_some_and(|anchor| !anchor.is_current(state))
            || self
                .destination
                .as_ref()
                .is_some_and(|slot| !slot.matches(state, &self.session_name))
        {
            return Err(changed_new_window_target(&self.session_name));
        }
        Ok(())
    }
}

impl WindowSlotWitness {
    fn matches(&self, state: &HandlerState, session_name: &SessionName) -> bool {
        match self {
            Self::Occupied(identity) => identity.is_current(state),
            Self::Vacant(index) => state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(*index))
                .is_none(),
        }
    }
}

fn capture_window_slot(
    state: &mut HandlerState,
    session_name: &SessionName,
    index: u32,
) -> Result<WindowSlotWitness, RmuxError> {
    let occupied = state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(index))
        .is_some();
    if occupied {
        StableTargetIdentity::capture(
            state,
            Target::Window(WindowTarget::with_window(session_name.clone(), index)),
        )
        .map(WindowSlotWitness::Occupied)
    } else {
        Ok(WindowSlotWitness::Vacant(index))
    }
}

fn changed_new_window_target(session_name: &SessionName) -> RmuxError {
    RmuxError::invalid_target(
        session_name.to_string(),
        "queued new-window target changed before mutation",
    )
}

fn resolve_queued_new_window_target_index(
    sessions: &ManagedSessionStore,
    target: &rmux_proto::SessionName,
    target_window_index: Option<NewWindowTargetIndex>,
) -> Result<Option<u32>, RmuxError> {
    let Some(target_window_index) = target_window_index else {
        return Ok(None);
    };

    match target_window_index {
        NewWindowTargetIndex::Absolute(index) => Ok(Some(index)),
        NewWindowTargetIndex::Relative(offset) => {
            let active = sessions
                .session(target)
                .ok_or_else(|| RmuxError::SessionNotFound(target.to_string()))?
                .active_window_index();
            if offset >= 0 {
                Ok(Some(active.checked_add(offset as u32).ok_or_else(
                    || RmuxError::Server("window index space exhausted for new-window".to_owned()),
                )?))
            } else {
                Ok(Some(active.checked_sub(offset.unsigned_abs()).ok_or_else(
                    || RmuxError::invalid_target(target.to_string(), "window offset out of range"),
                )?))
            }
        }
    }
}
