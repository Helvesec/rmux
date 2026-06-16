use super::pane_support::resolve_input_target;
use super::prompt_support::PromptInputEvent;
use super::RequestHandler;
use crate::copy_mode::{
    run_pipe_command, CopyBufferTarget, CopyModeCommandContext, CopyModePipeCommand, CopyModeState,
    CopyModeTransfer, ModeKeys,
};
use crate::mouse::copy_mode_mouse_context;
use crate::pane_terminals::HandlerState;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    CopyModeRequest, CopyModeResponse, ErrorResponse, OptionName, PaneTarget, Response, RmuxError,
    SendKeysResponse,
};

#[path = "handler_copy_mode/input.rs"]
mod input;
#[path = "handler_copy_mode/key_binding.rs"]
pub(in crate::handler) mod key_binding;
#[path = "handler_copy_mode/search.rs"]
mod search;

use input::{attached_copy_mode_input_action, AttachedCopyModeInputAction};

impl RequestHandler {
    pub(super) async fn handle_copy_mode(
        &self,
        requester_pid: u32,
        request: CopyModeRequest,
    ) -> Response {
        let attached_session = {
            let active_attach = self.active_attach.lock().await;
            active_attach.current_session_candidate(requester_pid)
        };
        let target = {
            let state = self.state.lock().await;
            match resolve_input_target(&state, request.target.as_ref(), attached_session.as_ref()) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        if request.cancel_mode {
            let transcript = {
                let state = self.state.lock().await;
                match state.transcript_handle(&target) {
                    Ok(transcript) => transcript,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            };
            let cleared = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .clear_copy_mode();
            if cleared {
                self.emit(LifecycleEvent::PaneModeChanged {
                    target: target.clone(),
                })
                .await;
                self.refresh_attached_session(target.session_name()).await;
            }
            return Response::CopyMode(CopyModeResponse {
                target,
                active: false,
                view_mode: false,
            });
        }

        let source_target = request.source.clone().unwrap_or_else(|| target.clone());
        let attached_mouse = if request.mouse_drag_start || request.scrollbar_scroll {
            let attach_pid = match self
                .resolve_attached_client_pid(requester_pid, "copy-mode")
                .await
            {
                Ok(attach_pid) => Some(attach_pid),
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let active_attach = self.active_attach.lock().await;
            attach_pid.and_then(|attach_pid| {
                active_attach.by_pid.get(&attach_pid).and_then(|active| {
                    active
                        .mouse
                        .current_event
                        .as_ref()
                        .cloned()
                        .map(|event| (event, active.mouse.slider_mpos, attach_pid))
                })
            })
        } else {
            None
        };

        let (target_transcript, source_screen, context) = {
            let state = self.state.lock().await;
            let target_transcript = match state.transcript_handle(&target) {
                Ok(transcript) => transcript,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let source_screen = match clone_screen_for_target(&state, &source_target) {
                Ok(screen) => screen,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let context = copy_mode_context(
                &state,
                &target,
                None,
                attached_mouse.as_ref().and_then(|(event, slider_mpos, _)| {
                    state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .and_then(|window| window.pane(target.pane_index()))
                        .and_then(|pane| {
                            copy_mode_mouse_context(event, pane.geometry(), *slider_mpos)
                        })
                }),
            );
            (target_transcript, source_screen, context)
        };

        let (view_mode, mode_changed) = {
            let mut transcript = target_transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            if let Some(mode) = transcript.copy_mode_state_mut() {
                mode.set_source_target(Some(source_target.clone()));
                mode.set_show_position(!request.hide_position);
                if request.exit_on_scroll {
                    mode.set_exit_on_scroll(true);
                }
                if request.source.is_some() {
                    mode.refresh_from_screen(source_screen);
                }
                if request.page_up {
                    let _ = mode.execute_command("page-up", &[], &context);
                }
                if request.page_down {
                    let _ = mode.execute_command("page-down", &[], &context);
                }
                if request.mouse_drag_start {
                    let _ = mode.execute_command("begin-selection", &[], &context);
                }
                if request.scrollbar_scroll {
                    let _ = mode.execute_command("scroll-to-mouse", &[], &context);
                }
                (mode.view_mode(), false)
            } else {
                let mut mode = CopyModeState::new(
                    source_screen,
                    Some(source_target),
                    false,
                    &context,
                    request.exit_on_scroll,
                    !request.hide_position,
                );
                if request.page_up {
                    let _ = mode.execute_command("page-up", &[], &context);
                }
                if request.page_down {
                    let _ = mode.execute_command("page-down", &[], &context);
                }
                if request.mouse_drag_start {
                    let _ = mode.execute_command("begin-selection", &[], &context);
                }
                if request.scrollbar_scroll {
                    let _ = mode.execute_command("scroll-to-mouse", &[], &context);
                }
                let view_mode = mode.view_mode();
                transcript.set_copy_mode_state(Some(mode));
                (view_mode, true)
            }
        };

        if mode_changed {
            self.emit(LifecycleEvent::PaneModeChanged {
                target: target.clone(),
            })
            .await;
        }
        self.refresh_attached_session(target.session_name()).await;

        Response::CopyMode(CopyModeResponse {
            target,
            active: true,
            view_mode,
        })
    }

    pub(super) async fn handle_send_keys_copy_mode(
        &self,
        requester_pid: u32,
        request: &rmux_proto::SendKeysExtRequest,
        target: PaneTarget,
        tokens: &[String],
    ) -> Response {
        let Some(command) = tokens.first() else {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("missing copy-mode command".to_owned()),
            });
        };
        let args = tokens.get(1..).unwrap_or(&[]);
        let repeat_count = request.repeat_count.unwrap_or(1).max(1);

        match self
            .execute_copy_mode_command(requester_pid, target, command, args, repeat_count)
            .await
        {
            Ok(()) => Response::SendKeys(SendKeysResponse {
                key_count: tokens.len(),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_attached_copy_mode_key_event(
        &self,
        attach_pid: u32,
        target: PaneTarget,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_copy_mode_key_event(attach_pid, target, event, true)
            .await
    }

    async fn handle_copy_mode_key_event(
        &self,
        requester_pid: u32,
        target: PaneTarget,
        event: PromptInputEvent,
        allow_prompt: bool,
    ) -> Result<bool, RmuxError> {
        let mode_keys = {
            let state = self.state.lock().await;
            if !target_is_in_copy_mode(&state, &target) {
                return Ok(false);
            }
            copy_mode_context(&state, &target, None, None).mode_keys
        };

        match attached_copy_mode_input_action(mode_keys, &event) {
            AttachedCopyModeInputAction::Search(direction) => {
                if !allow_prompt {
                    return Ok(false);
                }
                self.start_copy_mode_search_prompt(requester_pid, target, direction)
                    .await?;
            }
            AttachedCopyModeInputAction::Command(command) => {
                self.execute_copy_mode_command(requester_pid, target, command, &[], 1)
                    .await?;
            }
            AttachedCopyModeInputAction::Ignore => return Ok(false),
        }
        Ok(true)
    }

    pub(super) async fn target_is_in_copy_mode(
        &self,
        target: &PaneTarget,
    ) -> Result<bool, RmuxError> {
        let state = self.state.lock().await;
        Ok(target_is_in_copy_mode(&state, target))
    }

    pub(super) async fn execute_copy_mode_command(
        &self,
        requester_pid: u32,
        target: PaneTarget,
        command: &str,
        args: &[String],
        repeat_count: usize,
    ) -> Result<(), RmuxError> {
        let target_transcript = {
            let state = self.state.lock().await;
            match state.transcript_handle(&target) {
                Ok(transcript) => transcript,
                Err(error) => return Err(error),
            }
        };

        let refresh_screen = if command == "refresh-from-pane" {
            let source_target = {
                let transcript = target_transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned");
                let Some(mode) = transcript.copy_mode_state() else {
                    return Err(RmuxError::Server("pane is not in copy mode".to_owned()));
                };
                mode.source_target()
                    .cloned()
                    .unwrap_or_else(|| target.clone())
            };
            let state = self.state.lock().await;
            match clone_screen_for_target(&state, &source_target) {
                Ok(screen) => Some(screen),
                Err(error) => return Err(error),
            }
        } else {
            None
        };

        let attached_mouse = if matches!(command, "begin-selection" | "scroll-to-mouse") {
            attached_mouse_context(self, requester_pid, &target).await
        } else {
            None
        };
        let context = {
            let state = self.state.lock().await;
            copy_mode_context(&state, &target, refresh_screen, attached_mouse)
        };

        let mut mode_changed = false;
        for _ in 0..repeat_count {
            let outcome = {
                let mut transcript = target_transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned");
                let Some(mode) = transcript.copy_mode_state_mut() else {
                    return Err(RmuxError::Server("pane is not in copy mode".to_owned()));
                };
                match mode.execute_command(command, args, &context) {
                    Ok(outcome) => {
                        if outcome.cancel && transcript.clear_copy_mode() {
                            mode_changed = true;
                        }
                        outcome
                    }
                    Err(error) => return Err(error),
                }
            };
            if let Some(transfer) = outcome.transfer {
                self.apply_copy_mode_transfer(&context, transfer).await?;
            }
            if outcome.cancel {
                break;
            }
        }

        if mode_changed {
            self.emit(LifecycleEvent::PaneModeChanged {
                target: target.clone(),
            })
            .await;
        }
        self.refresh_attached_session(target.session_name()).await;

        Ok(())
    }

    async fn apply_copy_mode_transfer(
        &self,
        context: &CopyModeCommandContext,
        transfer: CopyModeTransfer,
    ) -> Result<(), RmuxError> {
        if let Some(buffer_target) = transfer.buffer_target.clone() {
            self.store_copy_mode_buffer(buffer_target, transfer.append, &transfer.data)
                .await?;
        }
        if let Some(command) = self
            .resolve_copy_mode_pipe_command(transfer.pipe_command.as_ref())
            .await
        {
            run_pipe_command(
                &context.default_shell,
                &command,
                context.working_directory.as_ref(),
                &transfer.data,
            )
            .await?;
        }
        Ok(())
    }

    async fn resolve_copy_mode_pipe_command(
        &self,
        pipe_command: Option<&CopyModePipeCommand>,
    ) -> Option<String> {
        match pipe_command {
            Some(CopyModePipeCommand::Explicit(command)) => Some(command.clone()),
            Some(CopyModePipeCommand::CopyCommandOption) => {
                let state = self.state.lock().await;
                state
                    .options
                    .resolve(None, OptionName::CopyCommand)
                    .filter(|command| !command.is_empty())
                    .map(str::to_owned)
            }
            None => None,
        }
    }

    async fn store_copy_mode_buffer(
        &self,
        target: CopyBufferTarget,
        append: bool,
        data: &[u8],
    ) -> Result<(), RmuxError> {
        let (buffer_name, evicted) = {
            let mut state = self.state.lock().await;
            let buffer_limit = state
                .options
                .resolve(None, OptionName::BufferLimit)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(50);

            let outcome = match target {
                CopyBufferTarget::New(name) => {
                    state
                        .buffers
                        .set(name.as_deref(), data.to_vec(), buffer_limit)?
                }
                CopyBufferTarget::Top if append => {
                    if let Ok((name, existing)) = state
                        .buffers
                        .show(None)
                        .map(|(name, existing)| (name.to_owned(), existing.to_vec()))
                    {
                        let mut combined = Vec::with_capacity(existing.len() + data.len());
                        combined.extend_from_slice(&existing);
                        combined.extend_from_slice(data);
                        state.buffers.set(Some(&name), combined, buffer_limit)?
                    } else {
                        state.buffers.set(None, data.to_vec(), buffer_limit)?
                    }
                }
                CopyBufferTarget::Top => state.buffers.set(None, data.to_vec(), buffer_limit)?,
            };
            (
                outcome.buffer_name().map(str::to_owned),
                outcome.evicted().to_vec(),
            )
        };

        for evicted in evicted {
            self.emit(LifecycleEvent::PasteBufferDeleted {
                buffer_name: evicted,
            })
            .await;
        }
        if let Some(buffer_name) = buffer_name {
            self.emit(LifecycleEvent::PasteBufferChanged { buffer_name })
                .await;
        }

        Ok(())
    }
}

async fn attached_mouse_context(
    handler: &RequestHandler,
    requester_pid: u32,
    target: &PaneTarget,
) -> Option<crate::copy_mode::CopyModeMouseContext> {
    let attach_pid = handler
        .resolve_attached_client_pid(requester_pid, "send-keys")
        .await
        .ok()?;
    let (event, slider_mpos) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach.by_pid.get(&attach_pid)?;
        let event = active.mouse.current_event.as_ref()?.clone();
        (event, active.mouse.slider_mpos)
    };
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .and_then(|pane| copy_mode_mouse_context(&event, pane.geometry(), slider_mpos))
}

fn clone_screen_for_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<rmux_core::Screen, RmuxError> {
    let transcript = state.transcript_handle(target)?;
    let screen = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .clone_screen();
    Ok(screen)
}

fn target_is_in_copy_mode(state: &HandlerState, target: &PaneTarget) -> bool {
    state
        .transcript_handle(target)
        .ok()
        .is_some_and(|transcript| {
            transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .copy_mode_state()
                .is_some()
        })
}

fn copy_mode_context(
    state: &HandlerState,
    target: &PaneTarget,
    refresh_screen: Option<rmux_core::Screen>,
    mouse: Option<crate::copy_mode::CopyModeMouseContext>,
) -> CopyModeCommandContext {
    let pane_profile = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok();
    let default_shell = pane_profile
        .map(|profile| profile.shell().to_string_lossy().into_owned())
        .or_else(|| {
            state
                .options
                .resolve(Some(target.session_name()), OptionName::DefaultShell)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(process_default_shell);
    let pane_cwd = pane_profile.map(|profile| profile.cwd().to_path_buf());
    let working_directory = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .and_then(|pane| state.pane_screen_state(target.session_name(), pane.id()))
        .and_then(|screen_state| (!screen_state.path.is_empty()).then(|| screen_state.path.into()))
        .or(pane_cwd);
    let word_separators = state
        .options
        .resolve(Some(target.session_name()), OptionName::WordSeparators)
        .filter(|value| !value.is_empty())
        .unwrap_or(" -_@")
        .to_owned();

    CopyModeCommandContext {
        mode_keys: ModeKeys::parse(state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::ModeKeys,
        )),
        wrap_search: state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::WrapSearch,
        ) != Some("off"),
        word_separators,
        default_shell,
        working_directory,
        refresh_screen,
        mouse,
    }
}

#[cfg(unix)]
fn process_default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}

#[cfg(windows)]
fn process_default_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
}

#[cfg(not(any(unix, windows)))]
fn process_default_shell() -> String {
    "sh".to_owned()
}
