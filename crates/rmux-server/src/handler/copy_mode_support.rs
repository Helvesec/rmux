use super::attach_support::ActiveAttachIdentity;
use super::pane_support::resolve_input_target;
use super::RequestHandler;
use crate::copy_mode::{CopyModeCommandContext, CopyModePrefixBehavior, CopyModeState};
use crate::limits::bounded_repeat_count;
use crate::mouse::{
    copy_mode_mouse_context, copy_mode_mouse_drag_start_context, AttachedMouseEvent,
};
use crate::pane_terminals::HandlerState;
use rmux_proto::{
    CopyModeRequest, CopyModeResponse, ErrorResponse, PaneTarget, Response, RmuxError,
    SendKeysResponse,
};

pub(in crate::handler) mod cancel_all;
mod command_context;
pub(in crate::handler) mod key_binding;
mod origin;
mod pipe_command;
mod refresh_fanout;
mod search;
mod transfer;

#[cfg(test)]
pub(in crate::handler) use refresh_fanout::install_copy_mode_mutation_pause;

use command_context::copy_mode_context;
use origin::{CopyModeCommandInvocation, CopyModeCommandOrigin, CopyModeMouseContextKind};

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
        let (target, target_session_id) = {
            let state = self.state.lock().await;
            let target = match resolve_input_target(
                &state,
                request.target.as_ref(),
                attached_session.as_ref(),
            ) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let Some(session_id) = state
                .sessions
                .session(target.session_name())
                .map(|session| session.id())
            else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("copy-mode target session disappeared".to_owned()),
                });
            };
            (target, session_id)
        };

        if request.cancel_mode {
            // `-q` addresses the PANE: it leaves every mode that pane is in, not
            // the consultation alone (`cmd-copy-mode.c:69-71` through
            // `window.c:1293-1298`). The unitary pop belongs to `send-keys -X
            // cancel` and is left where it is.
            if let Err(error) = self.leave_every_pane_mode(&target, target_session_id).await {
                return Response::Error(ErrorResponse { error });
            }
            return Response::CopyMode(CopyModeResponse {
                target,
                active: false,
                view_mode: false,
            });
        }

        let source_target = request.source.clone().unwrap_or_else(|| target.clone());
        let line_numbers_enabled = !super::scripting_support::queued_command_has_mouse_origin()
            && !request.mouse_drag_start
            && !request.scrollbar_scroll;
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
                    let mouse_context = if request.mouse_drag_start {
                        copy_mode_mouse_drag_start_context
                    } else {
                        copy_mode_mouse_context
                    };
                    state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .and_then(|window| window.pane(target.pane_index()))
                        .and_then(|pane| {
                            let geometry =
                                crate::mouse::layout_for_session(&state, target.session_name(), 1)
                                    .and_then(|layout| {
                                        layout
                                            .panes
                                            .into_iter()
                                            .find(|candidate| candidate.pane_id == pane.id())
                                            .map(|candidate| candidate.coordinates())
                                    })
                                    .unwrap_or_else(|| pane.geometry().into());
                            mouse_context(event, geometry, *slider_mpos)
                        })
                }),
            );
            (target_transcript, source_screen, context)
        };

        // Everything above was prepared under `state`, which is now released:
        // what follows mutates the transcript alone.
        #[cfg(test)]
        self.pause_before_copy_mode_entry_commit_for_test(&target)
            .await;

        let (view_mode, entered, mut left) = {
            let mut transcript = target_transcript.lock();
            if let Some(mode) = transcript.copy_mode_state_mut() {
                // Re-entry: tmux returns before `window_copy_init` when the pane
                // is already in copy mode (`window.c` `window_pane_set_mode`), so
                // the mode keeps its backing screen, the pane `refresh-from-pane`
                // reads and the `-H` and `-e` it was entered with. Only the line
                // numbers and the entry actions act (`cmd-copy-mode.c`). `-s` is
                // still resolved above, so an unknown source fails as in tmux.
                mode.set_line_numbers_enabled(line_numbers_enabled);
                let view_mode = mode.view_mode();
                let leaves = execute_copy_mode_entry_actions(
                    mode,
                    &request,
                    &context,
                    CopyModeEntry::Reentry,
                );
                (view_mode, false, leaves && transcript.clear_copy_mode())
            } else {
                let mut mode = CopyModeState::new(
                    source_screen,
                    Some(source_target),
                    false,
                    &context,
                    request.exit_on_scroll,
                    !request.hide_position,
                );
                mode.set_line_numbers_enabled(line_numbers_enabled);
                let view_mode = mode.view_mode();
                transcript.set_copy_mode_state(Some(mode));
                (view_mode, true, false)
            }
        };

        if entered {
            if let Err(error) = self.resize_for_copy_mode_transition(&target).await {
                return Response::Error(ErrorResponse { error });
            }
            let mut transcript = target_transcript.lock();
            let leaves = transcript.copy_mode_state_mut().is_some_and(|mode| {
                execute_copy_mode_entry_actions(mode, &request, &context, CopyModeEntry::Fresh)
            });
            left = leaves && transcript.clear_copy_mode();
        }
        if left {
            if let Err(error) = self.resize_for_copy_mode_transition(&target).await {
                return Response::Error(ErrorResponse { error });
            }
        }
        let mode_changed = entered || left;

        let identities = match self
            .prepare_copy_mode_refresh_fanout(&target, target_session_id, mode_changed)
            .await
        {
            Ok(identities) => identities,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        self.refresh_copy_mode_session_identities(identities).await;

        Response::CopyMode(CopyModeResponse {
            target,
            active: !left,
            view_mode,
        })
    }

    /// Only asks for a resize that entering or leaving the consultation can
    /// actually change. tmux stops a pane resize whose logical size did not
    /// move (`window.c:1197`); here it is the request itself that costs,
    /// because on Windows an identical `ResizePseudoConsole` is answered with a
    /// full repaint, and those bytes reach the pane's transcript as if its own
    /// tool had written them while the consultation masked it. The question and
    /// the resize share one hold of the state, so nothing decides on a view
    /// older than what it acts on.
    async fn resize_for_copy_mode_transition(&self, target: &PaneTarget) -> Result<(), RmuxError> {
        let mut state = self.state.lock().await;
        if state.copy_mode_transition_changes_terminal_geometry(target) {
            state.resize_terminals(target.session_name())?;
        }
        Ok(())
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
        let repeat_count = bounded_repeat_count(request.repeat_count);

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
        let mouse_event = super::scripting_support::queued_command_mouse_event();
        self.execute_copy_mode_command_with_identity(
            requester_pid,
            None,
            CopyModeCommandInvocation {
                target,
                command,
                args,
                repeat_count,
                origin: mouse_event.into(),
            },
        )
        .await
    }

    pub(super) async fn execute_copy_mode_command_with_mouse_event(
        &self,
        requester_pid: u32,
        target: PaneTarget,
        command: &str,
        args: &[String],
        repeat_count: usize,
        mouse_event: AttachedMouseEvent,
    ) -> Result<(), RmuxError> {
        self.execute_copy_mode_command_with_identity(
            requester_pid,
            None,
            CopyModeCommandInvocation {
                target,
                command,
                args,
                repeat_count,
                origin: CopyModeCommandOrigin::Mouse(mouse_event),
            },
        )
        .await
    }

    pub(super) async fn execute_copy_mode_command_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        target: PaneTarget,
        command: &str,
        args: &[String],
        repeat_count: usize,
    ) -> Result<(), RmuxError> {
        let mouse_event = super::scripting_support::queued_command_mouse_event();
        self.execute_copy_mode_command_with_identity(
            identity.attach_pid(),
            Some(identity),
            CopyModeCommandInvocation {
                target,
                command,
                args,
                repeat_count,
                origin: mouse_event.into(),
            },
        )
        .await
    }

    pub(super) async fn execute_copy_mode_command_for_identity_with_mouse_event(
        &self,
        identity: ActiveAttachIdentity,
        target: PaneTarget,
        command: &str,
        args: &[String],
        repeat_count: usize,
        mouse_event: AttachedMouseEvent,
    ) -> Result<(), RmuxError> {
        self.execute_copy_mode_command_with_identity(
            identity.attach_pid(),
            Some(identity),
            CopyModeCommandInvocation {
                target,
                command,
                args,
                repeat_count,
                origin: CopyModeCommandOrigin::Mouse(mouse_event),
            },
        )
        .await
    }

    async fn execute_copy_mode_command_with_identity(
        &self,
        requester_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        invocation: CopyModeCommandInvocation<'_>,
    ) -> Result<(), RmuxError> {
        let CopyModeCommandInvocation {
            target,
            command,
            args,
            repeat_count,
            origin,
        } = invocation;
        let expected_session_id = match identity {
            Some(identity) => {
                let (session_name, session_id) = self
                    .attached_session_identity_for_identity(identity)
                    .await?;
                if target.session_name() != &session_name {
                    return Err(RmuxError::Server(
                        "attached copy-mode target changed session".to_owned(),
                    ));
                }
                session_id
            }
            None => {
                let state = self.state.lock().await;
                state
                    .sessions
                    .session(target.session_name())
                    .map(|session| session.id())
                    .ok_or_else(|| {
                        RmuxError::Server("copy-mode target session disappeared".to_owned())
                    })?
            }
        };
        let target_transcript = {
            let state = self.state.lock().await;
            ensure_copy_mode_session_identity(&state, &target, Some(expected_session_id))?;
            match state.transcript_handle(&target) {
                Ok(transcript) => transcript,
                Err(error) => return Err(error),
            }
        };

        let refresh_screen = if command == "refresh-from-pane" {
            let source_target = {
                let transcript = target_transcript.lock();
                let Some(mode) = transcript.copy_mode_state() else {
                    return Err(RmuxError::Server("pane is not in copy mode".to_owned()));
                };
                mode.source_target()
                    .cloned()
                    .unwrap_or_else(|| target.clone())
            };
            let state = self.state.lock().await;
            ensure_copy_mode_session_identity(&state, &target, Some(expected_session_id))?;
            match clone_screen_for_target(&state, &source_target) {
                Ok(screen) => Some(screen),
                Err(error) => return Err(error),
            }
        } else {
            None
        };

        let attached_mouse = match (origin, command) {
            (CopyModeCommandOrigin::Mouse(event), "begin-selection") => {
                origin::mouse_context_for_origin(
                    self,
                    requester_pid,
                    identity,
                    &target,
                    &event,
                    CopyModeMouseContextKind::DragSelectionStart,
                )
                .await
            }
            (CopyModeCommandOrigin::Mouse(event), _) => {
                origin::mouse_context_for_origin(
                    self,
                    requester_pid,
                    identity,
                    &target,
                    &event,
                    CopyModeMouseContextKind::Position,
                )
                .await
            }
            (CopyModeCommandOrigin::NonMouse, _) => None,
        };
        let mut context = {
            let state = self.state.lock().await;
            ensure_copy_mode_session_identity(&state, &target, Some(expected_session_id))?;
            copy_mode_context(&state, &target, refresh_screen, attached_mouse)
        };

        let repeat_count = crate::limits::clamp_repeat_count(repeat_count);
        let prefix_behavior = CopyModeState::prefix_behavior(command);
        let execution_count = match prefix_behavior {
            CopyModePrefixBehavior::Repeat => repeat_count,
            CopyModePrefixBehavior::Count => 1,
        };
        let command_prefix = match prefix_behavior {
            CopyModePrefixBehavior::Repeat => 1,
            CopyModePrefixBehavior::Count => repeat_count,
        };
        let mut mode_changed = false;
        for _ in 0..execution_count {
            let search_off_lock = if CopyModeState::command_runs_search(command) {
                let transcript = target_transcript.lock();
                let Some(mode) = transcript.copy_mode_state() else {
                    return Err(RmuxError::Server("pane is not in copy mode".to_owned()));
                };
                mode.search_should_run_off_lock(args)
            } else {
                false
            };

            let (outcome, stop_repeats) = if search_off_lock {
                let result = self
                    .execute_copy_mode_search_command(&target_transcript, command, args, &context)
                    .await?;
                (result.outcome, result.stop_repeats)
            } else {
                let mut transcript = target_transcript.lock();
                let Some(mode) = transcript.copy_mode_state_mut() else {
                    return Err(RmuxError::Server("pane is not in copy mode".to_owned()));
                };
                match mode.execute_command_with_prefix(command, args, &context, command_prefix) {
                    Ok(outcome) => {
                        if outcome.cancel && transcript.clear_copy_mode() {
                            mode_changed = true;
                        }
                        (outcome, false)
                    }
                    Err(error) => return Err(error),
                }
            };
            if let Some(mouse) = context.mouse.as_mut() {
                mouse.move_cursor_before_command = false;
            }
            if let Some(transfer) = outcome.transfer {
                self.apply_copy_mode_transfer(requester_pid, identity, &context, transfer)
                    .await?;
            }
            if outcome.cancel || stop_repeats {
                break;
            }
        }

        #[cfg(test)]
        refresh_fanout::pause_after_copy_mode_mutation(requester_pid).await;

        if mode_changed {
            let mut state = self.state.lock().await;
            ensure_copy_mode_session_identity(&state, &target, Some(expected_session_id))?;
            state.resize_terminals(target.session_name())?;
        }

        let refresh_identities = self
            .prepare_copy_mode_refresh_fanout(&target, expected_session_id, mode_changed)
            .await?;
        self.refresh_copy_mode_session_identities(refresh_identities)
            .await;

        Ok(())
    }
}

/// Whether `copy-mode` opened the consultation or found it already open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyModeEntry {
    Fresh,
    Reentry,
}

/// Runs the entry actions of `copy-mode` and answers whether the pane must
/// leave the mode. `-M` starts a drag only when this command opened the mode
/// (`cmd-copy-mode.c`). `-d` pages down with this command's own `-e`, not the
/// mode's (`window_copy_pagedown(wp, 0, args_has(args, 'e'))`), and reaching
/// the bottom with it leaves the mode (`window_pane_reset_mode`).
fn execute_copy_mode_entry_actions(
    mode: &mut CopyModeState,
    request: &CopyModeRequest,
    context: &CopyModeCommandContext,
    entry: CopyModeEntry,
) -> bool {
    if request.page_up {
        let _ = mode.execute_command("page-up", &[], context);
    }
    if request.page_down {
        let _ = mode.execute_command("page-down", &[], context);
        if request.exit_on_scroll && mode.at_bottom() {
            return true;
        }
    }
    if request.mouse_drag_start && entry == CopyModeEntry::Fresh {
        let _ = mode.execute_command("begin-selection", &[], context);
    }
    if request.scrollbar_scroll {
        let _ = mode.execute_command("scroll-to-mouse", &[], context);
    }
    false
}

fn clone_screen_for_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<rmux_core::Screen, RmuxError> {
    let transcript = state.transcript_handle(target)?;
    let screen = transcript.lock().clone_presentation_screen();
    Ok(screen)
}

fn target_is_in_copy_mode(state: &HandlerState, target: &PaneTarget) -> bool {
    state
        .transcript_handle(target)
        .ok()
        .is_some_and(|transcript| transcript.lock().copy_mode_state().is_some())
}

fn ensure_copy_mode_session_identity(
    state: &HandlerState,
    target: &PaneTarget,
    expected_session_id: Option<rmux_proto::SessionId>,
) -> Result<(), RmuxError> {
    if expected_session_id.is_some_and(|session_id| {
        state
            .sessions
            .session(target.session_name())
            .is_none_or(|session| session.id() != session_id)
    }) {
        return Err(RmuxError::Server("attached session disappeared".to_owned()));
    }
    Ok(())
}
