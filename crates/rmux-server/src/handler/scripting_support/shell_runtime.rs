use std::future::Future;

use rmux_core::{formats::is_truthy, PaneId};
use rmux_proto::{PaneTarget, RmuxError, SessionName, Target};

use super::super::control_support::{with_control_queue_identity, ControlClientIdentity};
use super::super::{
    attach_support::ActiveAttachIdentity, with_expected_attach_registration, RequestHandler,
};
use super::queue::QueueExecutionContext;
use super::queue_special_target::QueueSpecialTargetBinding;
use super::run_shell_dispatch::SynchronousRunShellDispatch;
use super::targets::active_session_target;
use crate::terminal::SessionBaseEnvironment;

mod if_shell;
mod if_shell_queue;
mod run_shell;
mod run_shell_preparation;

pub(super) use run_shell_preparation::RunShellCommandError;

async fn with_background_control_identity<T, F>(
    identity: Option<ControlClientIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    match identity {
        Some(identity) => with_control_queue_identity(identity, future).await,
        None => future.await,
    }
}

async fn with_background_client_identities<T, F>(
    control_identity: Option<ControlClientIdentity>,
    attach_identity: Option<ActiveAttachIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let control_scoped = with_background_control_identity(control_identity, future);
    match attach_identity {
        Some(identity) => with_expected_attach_registration(identity, control_scoped).await,
        None => control_scoped.await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellTargetPolicy {
    Fixed,
    FollowAttachedSession,
}

impl ShellTargetPolicy {
    fn follows_attached_session(self) -> bool {
        self == Self::FollowAttachedSession
    }
}

#[derive(Debug, Clone, Copy)]
enum IfShellFormatTargetPolicy {
    PreferredLive,
    RetainedHistorical,
}

fn if_shell_runs_in_background(background: bool, format_mode: bool) -> bool {
    background && !format_mode
}

#[derive(Debug)]
struct QueuedRunShellState {
    parent_depth: usize,
    parent_context: Option<QueueExecutionContext>,
    target_binding: Option<QueueSpecialTargetBinding>,
    dispatch: Option<SynchronousRunShellDispatch>,
}

impl RequestHandler {
    async fn followed_attached_pane_target(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<PaneTarget, RmuxError> {
        match self
            .attached_queue_target_for_registration(identity)
            .await?
        {
            Target::Pane(target) => Ok(target),
            _ => Err(RmuxError::Server(
                "attached session has no current pane target".to_owned(),
            )),
        }
    }

    async fn existing_target_or_none(&self, target: Option<Target>) -> Option<Target> {
        let target = target?;
        let state = self.state.lock().await;
        let exists = match &target {
            Target::Session(session_name) => state.sessions.contains_session(session_name),
            Target::Window(target) => state
                .sessions
                .session(target.session_name())
                .is_some_and(|session| session.window_at(target.window_index()).is_some()),
            Target::Pane(target) => state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .is_some_and(|window| window.pane(target.pane_index()).is_some()),
        };
        exists.then_some(target)
    }
}

pub(super) fn pane_id_for_target(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
) -> Option<PaneId> {
    state
        .sessions
        .session(target.session_name())?
        .pane_id_in_window(target.window_index(), target.pane_index())
}

fn base_environment_for_target(
    state: &crate::pane_terminals::HandlerState,
    target: &Target,
) -> Option<SessionBaseEnvironment> {
    match target {
        Target::Pane(target) => state.session_base_environment_for_pane_target(target),
        Target::Window(target) => {
            state.session_base_environment_for_window(target.session_name(), target.window_index())
        }
        Target::Session(session_name) => {
            state.session_base_environment_for_active_pane(session_name)
        }
    }
}

pub(super) fn hook_session_default_target(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
) -> Option<Target> {
    active_session_target(&state.sessions, session_name).or_else(|| {
        let session = state.sessions.session(session_name)?;
        session.windows().find_map(|(window_index, window)| {
            window
                .active_pane()
                .or_else(|| window.panes().first())
                .map(|pane| {
                    Target::Pane(PaneTarget::with_window(
                        session_name.clone(),
                        window_index,
                        pane.index(),
                    ))
                })
        })
    })
}

fn if_shell_format_condition_is_true(value: &str) -> bool {
    is_truthy(value) && !value.starts_with('0')
}
