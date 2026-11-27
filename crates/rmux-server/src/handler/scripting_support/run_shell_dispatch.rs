use rmux_proto::{
    ResolveTargetRequest, ResolveTargetType, RmuxError, RunShellRequest, SessionName, Target,
};

#[cfg(windows)]
use super::super::pane_support::format_references_pane_pid;
use super::super::{RequestHandler, StableTargetIdentity};
use super::format_context::{
    apply_client_loop_context, format_context_for_named_target,
    format_context_for_target_with_server_values, global_format_context,
};
use super::shell_runtime::{hook_session_default_target, pane_id_for_target};
use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::current_hook_formats;
use crate::terminal::TerminalProfile;

#[derive(Debug)]
pub(super) struct SynchronousRunShellDispatch {
    pub(super) expanded_command: String,
    pub(super) shell_profile: Option<TerminalProfile>,
    pub(super) target_identity: Option<StableTargetIdentity>,
}

impl RequestHandler {
    pub(super) async fn capture_synchronous_run_shell_dispatch(
        &self,
        requester_pid: u32,
        request: &mut RunShellRequest,
        client_name: Option<&str>,
        target_missing_canfail: bool,
        client_loop_snapshots: Option<&[ClientLoopSnapshot]>,
    ) -> Result<SynchronousRunShellDispatch, RmuxError> {
        if request.target.is_none() && !target_missing_canfail {
            request.target = match self
                .resolve_target_for_requester(
                    requester_pid,
                    ResolveTargetRequest {
                        target: None,
                        target_type: ResolveTargetType::Pane,
                        window_index: false,
                        prefer_unattached: false,
                    },
                )
                .await
            {
                Ok(Target::Pane(target)) => Some(target),
                Ok(_) | Err(_) => None,
            };
        }
        #[cfg(windows)]
        if format_references_pane_pid(Some(&request.command)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let attached_count = match request.target.as_ref() {
            Some(target) => self.attached_count(target.session_name()).await,
            None => 0,
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
        let mut state = self.state.lock().await;
        let target_identity = request.target.as_ref().and_then(|target| {
            StableTargetIdentity::capture(&mut state, Target::Pane(target.clone())).ok()
        });
        let expanded_command = render_run_shell_command_from_state(
            &state,
            request,
            client_name,
            target_missing_canfail,
            attached_count,
            hook_formats,
            &socket_path,
            client_loop_snapshots,
        )?;
        let shell_profile = if request.as_commands {
            None
        } else {
            Some(self.run_shell_profile_from_state(&state, request)?)
        };
        Ok(SynchronousRunShellDispatch {
            expanded_command,
            shell_profile,
            target_identity,
        })
    }

    pub(super) fn run_shell_profile_from_state(
        &self,
        state: &crate::pane_terminals::HandlerState,
        request: &RunShellRequest,
    ) -> Result<TerminalProfile, RmuxError> {
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
            .and_then(|target| state.session_base_environment_for_pane_target(target));
        let pane_id = request
            .target
            .as_ref()
            .and_then(|target| pane_id_for_target(state, target));

        TerminalProfile::for_run_shell_with_base_environment(
            &state.environment,
            &state.options,
            session_name,
            session_id,
            &self.socket_path(),
            base_environment.as_ref(),
            !self.config_loading_active(),
            pane_id,
            request.start_directory.as_deref(),
        )
        .map(|profile| match request.source_depth {
            Some(depth) => profile.with_source_depth(depth),
            None if self.config_loading_active() => profile.with_source_depth(1),
            None => profile,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_run_shell_command_from_state(
    state: &crate::pane_terminals::HandlerState,
    request: &RunShellRequest,
    client_name: Option<&str>,
    target_missing_canfail: bool,
    attached_count: usize,
    hook_formats: Vec<(String, String)>,
    socket_path: &std::path::Path,
    client_loop_snapshots: Option<&[ClientLoopSnapshot]>,
) -> Result<String, RmuxError> {
    // The -t pane is named by the command; a hook's session only reaches one.
    let context = match request.target.as_ref() {
        Some(target) => format_context_for_named_target(
            state,
            &Target::Pane(target.clone()),
            attached_count,
            socket_path,
        )
        .unwrap_or_else(|_| global_format_context(state, socket_path)),
        None if !target_missing_canfail => match hook_formats
            .iter()
            .rev()
            .find(|(name, _)| name == "hook_session_name")
            .and_then(|(_, value)| SessionName::from_canonical(value.as_str()).ok())
            .and_then(|session_name| hook_session_default_target(state, &session_name))
        {
            Some(target) => {
                format_context_for_target_with_server_values(state, &target, 0, socket_path)?
            }
            None => global_format_context(state, socket_path),
        },
        None => global_format_context(state, socket_path),
    };
    let context = apply_client_loop_context(context, client_loop_snapshots, client_name);
    let context = hook_formats
        .into_iter()
        .fold(context, |context, (name, value)| {
            context.with_named_value(name, value)
        });
    let context = if request.as_commands {
        context
    } else {
        request
            .arguments
            .iter()
            .enumerate()
            .fold(context, |context, (index, value)| {
                context.with_named_value((index + 1).to_string(), value.clone())
            })
    };
    Ok(render_runtime_template(&request.command, &context, false))
}
