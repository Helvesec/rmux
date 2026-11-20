//! Direct if-shell requests: the if-shell task, its profile and condition expansion.

use rmux_proto::{
    CommandOutput, ErrorResponse, IfShellRequest, IfShellResponse, Response, RmuxError, Target,
};

use super::super::format_context::{
    apply_client_loop_context, format_context_for_named_target,
    format_context_for_target_with_server_values, global_format_context,
};
use super::super::queue::QueueExecutionContext;
use super::super::runtime::shell_condition_is_true;
use super::super::targets::active_session_target;
use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::render_runtime_template;
use crate::handler::control_support::current_control_queue_identity;
#[cfg(windows)]
use crate::handler::pane_support::format_references_pane_pid;
use crate::handler::{
    current_expected_attach_identity, validate_expected_attach_identity, RequestHandler,
};
use crate::hook_runtime::{current_hook_execution, current_hook_formats, with_hook_execution};
use crate::terminal::TerminalProfile;

use super::{
    base_environment_for_target, if_shell_format_condition_is_true, if_shell_runs_in_background,
    with_background_client_identities, IfShellFormatTargetPolicy, ShellTargetPolicy,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_if_shell(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
    ) -> Response {
        self.handle_if_shell_with_client_name(requester_pid, request, None)
            .await
    }

    pub(in crate::handler) async fn handle_if_shell_with_client_name(
        &self,
        requester_pid: u32,
        request: IfShellRequest,
        client_name: Option<String>,
    ) -> Response {
        if if_shell_runs_in_background(request.background, request.format_mode) {
            let control_identity = current_control_queue_identity(requester_pid);
            let attach_identity = current_expected_attach_identity();
            let target_policy = if attach_identity.is_some() && request.target.is_none() {
                ShellTargetPolicy::FollowAttachedSession
            } else {
                ShellTargetPolicy::Fixed
            };
            let detached_request_guard = self.begin_detached_request();
            let requester_access_guard = self
                .begin_inherited_detached_requester_access(requester_pid)
                .await;
            let handler = self.clone();
            let hook_formats = current_hook_formats();
            let hook_execution = current_hook_execution();
            if let Err(error) = self.spawn_background_task("rmux-if-shell", move || async move {
                let task = async move {
                    let _detached_request_guard = detached_request_guard;
                    let _requester_access_guard = requester_access_guard;
                    let _ = handler
                        .if_shell_task(requester_pid, request, client_name, target_policy)
                        .await;
                };
                with_background_client_identities(control_identity, attach_identity, async move {
                    match hook_execution {
                        Some(execution) => with_hook_execution(execution, hook_formats, task).await,
                        None => task.await,
                    }
                })
                .await;
            }) {
                return Response::Error(ErrorResponse { error });
            }
            return Response::IfShell(IfShellResponse::no_output());
        }

        match self
            .if_shell_task(
                requester_pid,
                request,
                client_name,
                ShellTargetPolicy::Fixed,
            )
            .await
        {
            Ok(Some(output)) if !output.stdout().is_empty() => {
                Response::IfShell(IfShellResponse::from_output(output))
            }
            Ok(_) => Response::IfShell(IfShellResponse::no_output()),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn if_shell_task(
        &self,
        requester_pid: u32,
        mut request: IfShellRequest,
        client_name: Option<String>,
        target_policy: ShellTargetPolicy,
    ) -> Result<Option<CommandOutput>, RmuxError> {
        let expected_attach = validate_expected_attach_identity(self, requester_pid).await?;
        if target_policy.follows_attached_session() {
            let identity = expected_attach.ok_or_else(|| {
                RmuxError::Server("background command lost its attached client identity".to_owned())
            })?;
            request.target = Some(Target::Pane(
                self.followed_attached_pane_target(identity).await?,
            ));
        }
        let expanded_condition = self
            .expand_if_shell_condition(
                &request,
                client_name.as_deref(),
                None,
                IfShellFormatTargetPolicy::PreferredLive,
            )
            .await?;

        let condition_is_true = if request.format_mode {
            if_shell_format_condition_is_true(&expanded_condition)
        } else {
            let profile = self.if_shell_profile(&request).await?;
            shell_condition_is_true(
                expanded_condition,
                &profile,
                Some(self.shell_processes.clone()),
            )
            .await?
        };

        let selected_command = if condition_is_true {
            Some(request.then_command)
        } else {
            request.else_command
        };
        let Some(selected_command) = selected_command else {
            return Ok(None);
        };

        let parsed = self
            .parse_command_string_one_group(&selected_command)
            .await?;
        let current_target = self.existing_target_or_none(request.target).await;
        let context = QueueExecutionContext::new(request.caller_cwd);
        let context = match target_policy {
            ShellTargetPolicy::FollowAttachedSession => context
                .with_implicit_current_target(current_target)
                .following_attached_session(),
            ShellTargetPolicy::Fixed if current_target.is_some() => {
                context.with_current_target(current_target)
            }
            ShellTargetPolicy::Fixed => context.with_implicit_current_target(current_target),
        }
        .with_client_name(client_name);
        let output = self
            .execute_parsed_commands(requester_pid, parsed, context)
            .await?;
        Ok((!output.stdout().is_empty()).then_some(output))
    }

    async fn if_shell_profile(
        &self,
        request: &IfShellRequest,
    ) -> Result<TerminalProfile, RmuxError> {
        let state = self.state.lock().await;
        let (session_name, session_id) = request
            .target
            .as_ref()
            .and_then(|target| {
                state
                    .sessions
                    .session(target.session_name())
                    .map(|session| (Some(target.session_name()), Some(session.id().as_u32())))
            })
            .unwrap_or((None, None));

        let base_environment = request
            .target
            .as_ref()
            .and_then(|target| base_environment_for_target(&state, target));

        TerminalProfile::for_run_shell_with_base_environment(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            base_environment.as_ref(),
            !self.config_loading_active(),
            None,
            request.caller_cwd.as_deref(),
        )
    }

    pub(super) async fn expand_if_shell_condition(
        &self,
        request: &IfShellRequest,
        client_name: Option<&str>,
        client_loop_snapshots: Option<&[ClientLoopSnapshot]>,
        target_policy: IfShellFormatTargetPolicy,
    ) -> Result<String, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(Some(&request.condition)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        // Retained hook formats remain available without borrowing a live pane.
        let fallback_target = if request.target.is_none()
            && matches!(target_policy, IfShellFormatTargetPolicy::PreferredLive)
        {
            self.preferred_session_name().await.ok()
        } else {
            None
        };
        let attached_count = match (&request.target, &fallback_target) {
            (Some(target), _) => self.attached_count(target.session_name()).await,
            (None, Some(session_name)) => self.attached_count(session_name).await,
            (None, None) => 0,
        };

        let socket_path = self.socket_path();
        let owned_client_loop_snapshots = if client_loop_snapshots.is_none() {
            Some(self.client_loop_snapshots().await)
        } else {
            None
        };
        let client_loop_snapshots =
            client_loop_snapshots.or(owned_client_loop_snapshots.as_deref());
        let state = self.state.lock().await;
        // The -t target, or the queue's current one, names the pane the
        // condition reads; the preferred-session fallback only reaches one.
        let context = match &request.target {
            Some(target) => {
                format_context_for_named_target(&state, target, attached_count, &socket_path)
                    .unwrap_or_else(|_| global_format_context(&state, &socket_path))
            }
            None => fallback_target
                .as_ref()
                .and_then(|session_name| active_session_target(&state.sessions, session_name))
                .map(|target| {
                    format_context_for_target_with_server_values(
                        &state,
                        &target,
                        attached_count,
                        &socket_path,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| global_format_context(&state, &socket_path)),
        };
        let context = apply_client_loop_context(context, client_loop_snapshots, client_name);

        Ok(render_runtime_template(&request.condition, &context, false))
    }
}
