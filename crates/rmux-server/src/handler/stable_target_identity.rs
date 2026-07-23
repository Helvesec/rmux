use std::future::Future;
use std::sync::Arc;

use rmux_proto::{
    PaneId, PaneTarget, RmuxError, SessionId, SessionName, Target, WindowId, WindowTarget,
};

use super::lifecycle_support::{LeaseResolution, LifecycleTargetLease};
use super::scripting_support::rename_target_session;
use crate::pane_terminals::{HandlerState, WindowLinkOccurrenceId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct StableTargetIdentity {
    target: Target,
    session_id: SessionId,
    window_id: Option<WindowId>,
    occurrence_id: Option<WindowLinkOccurrenceId>,
    pane_id: Option<PaneId>,
    pane_output_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct StablePaneOutputIdentity {
    target: PaneTarget,
    pane_id: PaneId,
    generation: u64,
}

impl StableTargetIdentity {
    #[cfg(test)]
    pub(in crate::handler) fn pane_for_test(target: PaneTarget) -> Self {
        Self {
            target: Target::Pane(target),
            session_id: SessionId::new(1),
            window_id: Some(WindowId::new(1)),
            occurrence_id: Some(WindowLinkOccurrenceId::new_for_test(1)),
            pane_id: Some(PaneId::new(10)),
            pane_output_generation: Some(1),
        }
    }

    pub(in crate::handler) fn capture(
        state: &mut HandlerState,
        target: Target,
    ) -> Result<Self, RmuxError> {
        let (session_id, window_id, occurrence_id, pane_id, pane_output_generation) = match &target
        {
            Target::Session(session_name) => {
                let session = state
                    .sessions
                    .session(session_name)
                    .ok_or_else(|| unavailable(&target))?;
                (session.id(), None, None, None, None)
            }
            Target::Window(window_target) => {
                let occurrence_id = state
                    .ensure_live_window_link_occurrence_id(
                        window_target.session_name(),
                        window_target.window_index(),
                    )
                    .ok_or_else(|| unavailable(&target))?;
                let session = state
                    .sessions
                    .session(window_target.session_name())
                    .ok_or_else(|| unavailable(&target))?;
                let window = session
                    .window_at(window_target.window_index())
                    .ok_or_else(|| unavailable(&target))?;
                (
                    session.id(),
                    Some(window.id()),
                    Some(occurrence_id),
                    None,
                    None,
                )
            }
            Target::Pane(pane_target) => {
                let occurrence_id = state
                    .ensure_live_window_link_occurrence_id(
                        pane_target.session_name(),
                        pane_target.window_index(),
                    )
                    .ok_or_else(|| unavailable(&target))?;
                let session = state
                    .sessions
                    .session(pane_target.session_name())
                    .ok_or_else(|| unavailable(&target))?;
                let window = session
                    .window_at(pane_target.window_index())
                    .ok_or_else(|| unavailable(&target))?;
                let pane_id = window
                    .pane(pane_target.pane_index())
                    .map(rmux_core::Pane::id)
                    .ok_or_else(|| unavailable(&target))?;
                let pane_output_generation =
                    state.pane_output_generation_for_target(pane_target, pane_id);
                (
                    session.id(),
                    Some(window.id()),
                    Some(occurrence_id),
                    Some(pane_id),
                    Some(pane_output_generation),
                )
            }
        };
        Ok(Self {
            target,
            session_id,
            window_id,
            occurrence_id,
            pane_id,
            pane_output_generation,
        })
    }

    fn matches_session(&self, state: &HandlerState, target: &rmux_proto::SessionName) -> bool {
        matches!(&self.target, Target::Session(expected) if expected == target)
            && state
                .sessions
                .session(target)
                .is_some_and(|session| session.id() == self.session_id)
    }

    fn matches_window(&self, state: &HandlerState, target: &WindowTarget) -> bool {
        matches!(&self.target, Target::Window(expected) if expected == target)
            && self.matches_window_components(state, target)
    }

    fn matches_pane(&self, state: &HandlerState, target: &PaneTarget) -> bool {
        matches!(&self.target, Target::Pane(expected) if expected == target)
            && self.matches_window_components(
                state,
                &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
            )
            && state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .is_some_and(|pane| Some(pane.id()) == self.pane_id)
            && self.pane_id.is_some_and(|pane_id| {
                Some(state.pane_output_generation_for_target(target, pane_id))
                    == self.pane_output_generation
            })
    }

    fn matches_window_components(&self, state: &HandlerState, target: &WindowTarget) -> bool {
        state
            .sessions
            .session(target.session_name())
            .filter(|session| session.id() == self.session_id)
            .and_then(|session| session.window_at(target.window_index()))
            .is_some_and(|window| Some(window.id()) == self.window_id)
            && state.window_link_occurrence_id(target.session_name(), target.window_index())
                == self.occurrence_id
    }

    pub(in crate::handler) fn matches_target(&self, state: &HandlerState, target: &Target) -> bool {
        match target {
            Target::Session(target) => self.matches_session(state, target),
            Target::Window(target) => self.matches_window(state, target),
            Target::Pane(target) => self.matches_pane(state, target),
        }
    }

    pub(in crate::handler) fn is_current(&self, state: &HandlerState) -> bool {
        self.matches_target(state, &self.target)
    }

    pub(in crate::handler) fn require(
        &self,
        state: &HandlerState,
        target: &Target,
        operation: &str,
    ) -> Result<(), RmuxError> {
        if self.matches_target(state, target) {
            return Ok(());
        }
        Err(RmuxError::Server(format!(
            "{operation} target was replaced before execution"
        )))
    }

    pub(in crate::handler) fn target(&self) -> &Target {
        &self.target
    }

    pub(in crate::handler) fn rename_session(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        rename_target_session(&mut self.target, old_name, new_name);
    }
}

impl StablePaneOutputIdentity {
    pub(in crate::handler) fn capture_for_target(
        state: &HandlerState,
        target: &Target,
    ) -> Option<Self> {
        let target = pane_target_for_scope(state, target)?;
        let pane_id = state
            .sessions
            .session(target.session_name())?
            .pane_id_in_window(target.window_index(), target.pane_index())?;
        let generation = state.pane_output_generation_for_target(&target, pane_id);
        Some(Self {
            target,
            pane_id,
            generation,
        })
    }

    pub(in crate::handler) fn require(
        &self,
        state: &HandlerState,
        operation: &str,
    ) -> Result<(), RmuxError> {
        let current_id = state
            .sessions
            .session(self.target.session_name())
            .and_then(|session| {
                session.pane_id_in_window(self.target.window_index(), self.target.pane_index())
            });
        if current_id == Some(self.pane_id)
            && state.pane_output_generation_for_target(&self.target, self.pane_id)
                == self.generation
        {
            return Ok(());
        }
        Err(RmuxError::Server(format!(
            "{operation} pane process was replaced before execution"
        )))
    }

    pub(in crate::handler) fn rename_session(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        super::scripting_support::rename_pane_target_session(&mut self.target, old_name, new_name);
    }
}

fn pane_target_for_scope(state: &HandlerState, target: &Target) -> Option<PaneTarget> {
    match target {
        Target::Pane(target) => Some(target.clone()),
        Target::Window(target) => state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(rmux_core::Window::active_pane)
            .map(|pane| {
                PaneTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                    pane.index(),
                )
            }),
        Target::Session(session_name) => state.sessions.session(session_name).and_then(|session| {
            session.active_pane().map(|pane| {
                PaneTarget::with_window(
                    session_name.clone(),
                    session.active_window_index(),
                    pane.index(),
                )
            })
        }),
    }
}

fn unavailable(target: &Target) -> RmuxError {
    RmuxError::invalid_target(
        target.to_string(),
        "stable target identity was unavailable during queue capture",
    )
}

tokio::task_local! {
    static EXPECTED_STABLE_TARGET_IDENTITIES: ExpectedStableTargetIdentities;
}

#[derive(Debug, Clone, Default)]
struct ExpectedStableTargetIdentities {
    identities: Vec<StableTargetIdentity>,
    retained_lifecycle_target: Option<Arc<LifecycleTargetLease>>,
    retained_lifecycle_identity: Option<StableTargetIdentity>,
}

pub(in crate::handler) async fn with_expected_stable_target_identities<T, F>(
    identities: Vec<StableTargetIdentity>,
    retained_lifecycle_target: Option<Arc<LifecycleTargetLease>>,
    retained_lifecycle_identity: Option<StableTargetIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_STABLE_TARGET_IDENTITIES
        .scope(
            ExpectedStableTargetIdentities {
                identities,
                retained_lifecycle_target,
                retained_lifecycle_identity,
            },
            future,
        )
        .await
}

pub(in crate::handler) async fn without_expected_stable_target_identities<T, F>(future: F) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_STABLE_TARGET_IDENTITIES
        .scope(ExpectedStableTargetIdentities::default(), future)
        .await
}

pub(in crate::handler) fn require_expected_stable_session_identity(
    state: &HandlerState,
    target: &rmux_proto::SessionName,
) -> Result<(), RmuxError> {
    require_expected_identity(state, TargetRef::Session(target))
}

pub(in crate::handler) fn require_expected_stable_window_identity(
    state: &HandlerState,
    target: &WindowTarget,
) -> Result<(), RmuxError> {
    require_expected_identity(state, TargetRef::Window(target))
}

pub(in crate::handler) fn require_expected_stable_pane_identity(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    require_expected_identity(state, TargetRef::Pane(target))
}

enum TargetRef<'a> {
    Session(&'a rmux_proto::SessionName),
    Window(&'a WindowTarget),
    Pane(&'a PaneTarget),
}

fn require_expected_identity(state: &HandlerState, target: TargetRef<'_>) -> Result<(), RmuxError> {
    EXPECTED_STABLE_TARGET_IDENTITIES
        .try_with(|expected| {
            require_live_lifecycle_target(expected, state)?;
            let mut relevant = expected
                .identities
                .iter()
                .filter(|identity| identity_matches_kind(identity, &target));
            let Some(first) = relevant.next() else {
                return Ok(());
            };
            let matches = std::iter::once(first)
                .chain(relevant)
                .any(|identity| identity_matches_target(identity, state, &target));
            if matches {
                Ok(())
            } else {
                Err(RmuxError::invalid_target(
                    target_string(&target),
                    "target identity changed before queued mutation: queued target was replaced",
                ))
            }
        })
        .unwrap_or(Ok(()))
}

fn require_live_lifecycle_target(
    expected: &ExpectedStableTargetIdentities,
    state: &HandlerState,
) -> Result<(), RmuxError> {
    match expected
        .retained_lifecycle_target
        .as_deref()
        .map(|target| target.resolve(state))
    {
        Some(LeaseResolution::Retired(_)) => Err(RmuxError::Server(
            "queued lifecycle target retired before execution".to_owned(),
        )),
        Some(LeaseResolution::Replaced) => Err(RmuxError::Server(
            "queued lifecycle target was replaced before execution".to_owned(),
        )),
        Some(LeaseResolution::Live(target)) => {
            if expected
                .retained_lifecycle_identity
                .as_ref()
                .is_some_and(|identity| identity.matches_target(state, &target))
            {
                Ok(())
            } else {
                Err(RmuxError::Server(
                    "queued lifecycle target changed after parsing".to_owned(),
                ))
            }
        }
        None => Ok(()),
    }
}

fn identity_matches_kind(identity: &StableTargetIdentity, target: &TargetRef<'_>) -> bool {
    matches!(
        (&identity.target, target),
        (Target::Session(_), TargetRef::Session(_))
            | (Target::Window(_), TargetRef::Window(_))
            | (Target::Pane(_), TargetRef::Pane(_))
    )
}

fn identity_matches_target(
    identity: &StableTargetIdentity,
    state: &HandlerState,
    target: &TargetRef<'_>,
) -> bool {
    match target {
        TargetRef::Session(target) => identity.matches_session(state, target),
        TargetRef::Window(target) => identity.matches_window(state, target),
        TargetRef::Pane(target) => identity.matches_pane(state, target),
    }
}

fn target_string(target: &TargetRef<'_>) -> String {
    match target {
        TargetRef::Session(target) => target.to_string(),
        TargetRef::Window(target) => target.to_string(),
        TargetRef::Pane(target) => target.to_string(),
    }
}
