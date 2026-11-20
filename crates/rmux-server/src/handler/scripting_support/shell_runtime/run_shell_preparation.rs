//! Preparation of run-shell work: inherited targets, delayed requests, command contexts, expansion and profiles.

use rmux_core::command_parser::ParsedCommands;
use rmux_proto::{PaneTarget, RmuxError, RunShellRequest, Target};

use super::super::queue::QueueExecutionContext;
use super::super::run_shell_dispatch::render_run_shell_command_from_state;
use super::super::runtime::run_shell_delay_duration;
use super::super::targets::active_session_target;
use crate::client_format::ClientLoopSnapshot;
#[cfg(windows)]
use crate::handler::pane_support::format_references_pane_pid;
use crate::handler::target_support::{inherited_pane_target, requester_environment_context};
use crate::handler::{validate_expected_attach_identity, RequestHandler};
use crate::hook_runtime::current_hook_formats;
use crate::terminal::TerminalProfile;

use super::{QueuedRunShellState, ShellTargetPolicy};

impl RequestHandler {
    pub(super) async fn inherited_run_shell_target(
        &self,
        requester_pid: u32,
    ) -> Result<Option<PaneTarget>, RmuxError> {
        let requester_environment =
            requester_environment_context(requester_pid, &self.socket_path());
        let state = self.state.lock().await;
        inherited_pane_target(&state.sessions, requester_environment, None).map(|target| {
            target.and_then(|target| match target {
                Target::Pane(target) => Some(target),
                _ => None,
            })
        })
    }

    pub(super) async fn prepare_run_shell_request(
        &self,
        requester_pid: u32,
        mut request: RunShellRequest,
        target_policy: ShellTargetPolicy,
    ) -> Result<Option<RunShellRequest>, RmuxError> {
        if let Some(delay_seconds) = request.delay_seconds {
            tokio::time::sleep(run_shell_delay_duration(delay_seconds.as_secs_f64())?).await;
        }
        #[cfg(test)]
        self.pause_before_run_shell_target_capture_for_test(requester_pid)
            .await;
        if target_policy.follows_attached_session() {
            let identity = validate_expected_attach_identity(self, requester_pid)
                .await?
                .ok_or_else(|| {
                    RmuxError::Server(
                        "background command lost its attached client identity".to_owned(),
                    )
                })?;
            request.target = Some(self.followed_attached_pane_target(identity).await?);
        } else if request.as_commands {
            // Command-mode jobs can act on the attached registration even
            // with a fixed pane target, so retain the same-PID reuse guard.
            let _ = validate_expected_attach_identity(self, requester_pid).await?;
        }
        Ok((!request.command.is_empty()).then_some(request))
    }

    pub(super) async fn prepare_run_shell_commands(
        &self,
        requester_pid: u32,
        request: RunShellRequest,
        client_name: Option<String>,
        target_policy: ShellTargetPolicy,
        target_missing_canfail: bool,
        queue_state: &QueuedRunShellState,
    ) -> Result<(ParsedCommands, QueueExecutionContext), RmuxError> {
        let dispatch = queue_state.dispatch.as_ref();
        let has_fixed_target = request.target.is_some();
        let command = match dispatch {
            Some(dispatch) => dispatch.expanded_command.clone(),
            None => {
                self.expand_run_shell_command(
                    &request,
                    client_name.as_deref(),
                    target_missing_canfail,
                    queue_state
                        .parent_context
                        .as_ref()
                        .and_then(QueueExecutionContext::client_loop_snapshots),
                )
                .await?
            }
        };
        let parsed = self.parse_command_string_one_group(&command).await?;
        let (current_target, pinned_target_identity, captured_target_disappeared) =
            if target_missing_canfail {
                (None, None, false)
            } else if let Some(identity) =
                dispatch.and_then(|dispatch| dispatch.target_identity.as_ref())
            {
                let state = self.state.lock().await;
                match identity.resolve_current_pane_identity(&state) {
                    Some(current_identity) => (
                        Some(current_identity.target().clone()),
                        Some(current_identity),
                        false,
                    ),
                    None => (
                        Some(identity.target().clone()),
                        Some(identity.clone()),
                        true,
                    ),
                }
            } else {
                (
                    self.run_shell_commands_current_target(requester_pid, request.target.clone())
                        .await,
                    None,
                    false,
                )
            };
        let context = match queue_state.parent_context.as_ref() {
            Some(context) => match request.start_directory.clone() {
                Some(caller_cwd) => context.clone().with_caller_cwd(Some(caller_cwd)),
                None => context.clone(),
            },
            None => QueueExecutionContext::new(request.start_directory.clone()),
        };
        let context = if let Some(target_binding) = queue_state.target_binding.as_ref() {
            target_binding.child_context(&context)
        } else {
            match target_policy {
                ShellTargetPolicy::FollowAttachedSession => context
                    .with_implicit_current_target(current_target)
                    .following_attached_session(),
                ShellTargetPolicy::Fixed if has_fixed_target => {
                    context.with_current_target(current_target)
                }
                ShellTargetPolicy::Fixed => context.with_implicit_current_target(current_target),
            }
        };
        let context = if captured_target_disappeared {
            context.forbid_missing_current_target_fallback()
        } else {
            context
        }
        .with_client_name(client_name)
        .with_pinned_current_target_identity(pinned_target_identity)
        .with_mouse_event(super::super::queued_command_mouse_event().or_else(|| {
            queue_state
                .parent_context
                .as_ref()
                .and_then(|context| context.mouse_event.clone())
        }));
        let synchronous = request.delay_seconds.is_none();
        let context = match request.source_depth {
            Some(depth) => context.for_sourced_commands(depth, None),
            None => context,
        }
        .for_run_shell_commands(
            queue_state.parent_depth,
            synchronous,
            request.background,
        )?;
        Ok((parsed, context))
    }

    async fn run_shell_commands_current_target(
        &self,
        requester_pid: u32,
        target: Option<PaneTarget>,
    ) -> Option<Target> {
        if let Some(target) = target {
            return self
                .existing_target_or_none(Some(Target::Pane(target)))
                .await;
        }

        let session_name = match self.current_session_candidate(requester_pid).await {
            Some(session_name) => Some(session_name),
            None => self.preferred_session_name().await.ok(),
        }?;
        let state = self.state.lock().await;
        active_session_target(&state.sessions, &session_name)
    }

    pub(super) async fn expand_run_shell_command(
        &self,
        request: &RunShellRequest,
        client_name: Option<&str>,
        target_missing_canfail: bool,
        client_loop_snapshots: Option<&[ClientLoopSnapshot]>,
    ) -> Result<String, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(Some(&request.command)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let target = request.target.as_ref();
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };

        let hook_formats = current_hook_formats();
        let socket_path = self.socket_path();
        let owned_client_loop_snapshots = if client_loop_snapshots.is_none() {
            Some(self.client_loop_snapshots().await)
        } else {
            None
        };
        let client_loop_snapshots =
            client_loop_snapshots.or(owned_client_loop_snapshots.as_deref());
        let state = self.state.lock().await;
        render_run_shell_command_from_state(
            &state,
            request,
            client_name,
            target_missing_canfail,
            attached_count,
            hook_formats,
            &socket_path,
            client_loop_snapshots,
        )
    }

    pub(super) async fn run_shell_profile(
        &self,
        request: &RunShellRequest,
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        self.run_shell_profile_from_state(&state, request)
    }
}
