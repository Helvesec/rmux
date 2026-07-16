use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rmux_proto::{
    AttachShellCommand, AttachedKeystroke, KeyDispatched, OptionName, PaneTarget, TerminalSize,
};
use tokio::time::sleep;

use super::RequestHandler;
use crate::handler_support::attached_client_required;
use crate::outer_terminal::{CursorScope, OuterTerminal, OuterTerminalContext};
use crate::pane_io::{AttachControl, AttachTarget, LivePaneRender, OverlayFrame};
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::renderer;
use crate::terminal::TerminalProfile;

pub(super) const ATTACH_CONTROL_BACKLOG_LIMIT: usize = 64;

#[path = "handler_attach/key_table.rs"]
mod key_table;
#[path = "handler_attach/refresh.rs"]
mod refresh;
#[path = "handler_attach/registration.rs"]
mod registration;
#[path = "handler_attach/resize_policy.rs"]
mod resize_policy;
#[path = "handler_attach/state.rs"]
mod state;

pub(crate) use crate::client_flags::ClientFlags;
pub(in crate::handler) use resize_policy::AttachedWindowSizePolicy;
pub(crate) use state::AttachRegistration;
pub(super) use state::{
    ActiveAttach, ActiveAttachState, DisplayPanesClientState, DisplayPanesLabel,
};

impl RequestHandler {
    pub(crate) async fn handle_attached_keystroke(
        &self,
        attach_pid: u32,
        keystroke: &AttachedKeystroke,
        consumed: bool,
    ) -> Result<KeyDispatched, rmux_proto::RmuxError> {
        let active_attach = self.active_attach.lock().await;
        if !active_attach.by_pid.contains_key(&attach_pid) {
            return Err(rmux_proto::RmuxError::Server(
                "attached client disappeared".to_owned(),
            ));
        }
        let byte_len = u32::try_from(keystroke.bytes().len()).map_err(|_| {
            rmux_proto::RmuxError::Server("attached keystroke length overflow".to_owned())
        })?;
        if consumed {
            Ok(KeyDispatched::new(byte_len))
        } else {
            Ok(KeyDispatched::forwarded(byte_len))
        }
    }

    pub(super) async fn resolve_attached_client_pid(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<u32, rmux_proto::RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach.resolve_attached_client_pid(requester_pid, command_name)
    }

    pub(super) async fn terminal_context_for_attached_client(
        &self,
        attach_pid: u32,
    ) -> Option<OuterTerminalContext> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .map(|active| active.terminal_context.clone())
    }

    pub(super) async fn terminal_context_and_size_for_attached_client(
        &self,
        attach_pid: u32,
    ) -> Option<(
        OuterTerminalContext,
        TerminalSize,
        Option<rmux_proto::TerminalPixels>,
        bool,
        ClientFlags,
    )> {
        let active_attach = self.active_attach.lock().await;
        active_attach.by_pid.get(&attach_pid).map(|active| {
            (
                active.terminal_context.clone(),
                active.client_size,
                active.client_pixels,
                active.render_stream,
                active.flags,
            )
        })
    }

    pub(super) async fn attached_session_name_for_command(
        &self,
        attach_pid: u32,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .map(|active| active.session_name.clone())
            .ok_or_else(|| attached_client_required(command_name))
    }

    pub(super) async fn attach_shell_command_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
        command: String,
    ) -> Result<AttachShellCommand, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        let session_id = state
            .sessions
            .session(session_name)
            .map(|session| session.id().as_u32());
        let profile = TerminalProfile::for_run_shell(
            &state.environment,
            &state.options,
            Some(session_name),
            session_id,
            &self.socket_path(),
            !self.config_loading_active(),
            None,
        )?;
        Ok(profile.attach_shell_command(command))
    }

    pub(super) async fn clipboard_attach_for_requester(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Option<(u32, OuterTerminalContext)> {
        let active_attach = self.active_attach.lock().await;
        let attach_pid = active_attach
            .resolve_attached_client_pid(requester_pid, command_name)
            .ok()?;
        let active = active_attach.by_pid.get(&attach_pid)?;
        Some((attach_pid, active.terminal_context.clone()))
    }

    pub(super) async fn send_attach_control(
        &self,
        attach_pid: u32,
        command: AttachControl,
        command_name: &str,
        next_session_name: Option<rmux_proto::SessionName>,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        let is_switch = matches!(command, AttachControl::Switch(_));
        let switch_changes_session = if is_switch {
            self.attach_switch_changes_session(attach_pid, next_session_name.as_ref(), command_name)
                .await?
        } else {
            false
        };
        let mode_tree_refresh_sessions = if switch_changes_session {
            self.dismiss_mode_tree(attach_pid).await?
        } else {
            Vec::new()
        };
        let clear_prompt = matches!(
            command,
            AttachControl::Switch(_)
                | AttachControl::Detach
                | AttachControl::Exited
                | AttachControl::DetachKill
                | AttachControl::DetachExecShellCommand(_)
        );
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(attached_client_required(command_name));
        };
        let previous_session_name = active.session_name.clone();
        let mut overlay_to_terminate = None;

        if is_switch
            && next_session_name
                .as_ref()
                .is_some_and(|session_name| session_name != &active.session_name)
        {
            overlay_to_terminate = reset_interactive_attach_state_for_session_switch(active);
        }
        let closing_control = matches!(
            command,
            AttachControl::Detach
                | AttachControl::Exited
                | AttachControl::DetachKill
                | AttachControl::DetachExecShellCommand(_)
        );
        let render_stream_switch_refresh = active.render_stream
            && matches!(
                &command,
                AttachControl::Switch(target) if target.is_coalescible_render_refresh()
            );
        if render_stream_switch_refresh {
            if let Some(session_name) = next_session_name {
                if session_name != active.session_name {
                    active.last_session = Some(active.session_name.clone());
                }
                active.session_name = session_name;
            }
            if !active.render_refresh_pending {
                active.render_refresh_pending = true;
                if active.control_tx.send(AttachControl::Refresh).is_err() {
                    active_attach.remove_attached_client(attach_pid);
                    self.bump_active_attach_epoch();
                    return Err(attached_client_required(command_name));
                }
            }
            drop(active_attach);
            if clear_prompt {
                self.clear_prompt_for_attach(attach_pid).await;
            }
            for session_name in mode_tree_refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
            terminate_overlay_job(overlay_to_terminate);
            return Ok(previous_session_name);
        }
        if is_switch {
            active.render_generation = active.render_generation.saturating_add(1);
        }
        let tracked_control = matches!(command, AttachControl::Switch(_) | AttachControl::Refresh);
        if tracked_control
            && active.control_backlog.load(Ordering::Acquire) >= ATTACH_CONTROL_BACKLOG_LIMIT
        {
            let _ = active.control_tx.send(AttachControl::Detach);
            active.closing.store(true, Ordering::SeqCst);
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return Err(rmux_proto::RmuxError::Server(
                "attached client is not draining updates".to_owned(),
            ));
        }
        if tracked_control {
            active.control_backlog.fetch_add(1, Ordering::AcqRel);
        }
        if active.control_tx.send(command).is_err() {
            if tracked_control {
                let _ = active.control_backlog.fetch_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    |value| value.checked_sub(1),
                );
            }
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return Err(attached_client_required(command_name));
        }
        if closing_control {
            active.closing.store(true, Ordering::SeqCst);
        }
        if let Some(session_name) = next_session_name {
            if session_name != active.session_name {
                active.last_session = Some(active.session_name.clone());
            }
            active.session_name = session_name;
        }
        drop(active_attach);

        if clear_prompt {
            self.clear_prompt_for_attach(attach_pid).await;
        }
        for session_name in mode_tree_refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        terminate_overlay_job(overlay_to_terminate);

        Ok(previous_session_name)
    }

    async fn attach_switch_changes_session(
        &self,
        attach_pid: u32,
        next_session_name: Option<&rmux_proto::SessionName>,
        command_name: &str,
    ) -> Result<bool, rmux_proto::RmuxError> {
        let Some(next_session_name) = next_session_name else {
            return Ok(false);
        };
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .ok_or_else(|| attached_client_required(command_name))?;
        Ok(next_session_name != &active.session_name)
    }

    pub(super) async fn exit_attached_session(&self, session_name: &rmux_proto::SessionName) {
        self.close_attached_session(session_name, || AttachControl::Exited)
            .await;
    }

    async fn close_attached_session<F>(
        &self,
        session_name: &rmux_proto::SessionName,
        mut control: F,
    ) where
        F: FnMut() -> AttachControl,
    {
        let mut overlay_jobs = Vec::new();
        let mut active_attach = self.active_attach.lock().await;
        for active in active_attach.by_pid.values_mut() {
            if active.last_session.as_ref() == Some(session_name) {
                active.last_session = None;
            }
        }
        active_attach.by_pid.retain(|_, active| {
            if &active.session_name != session_name {
                return true;
            }

            overlay_jobs.push(active.overlay.take());
            let _ = active.control_tx.send(control());
            active.closing.store(true, Ordering::SeqCst);
            false
        });
        active_attach.forget_session_windows(session_name);
        drop(active_attach);
        self.bump_active_attach_epoch();
        for overlay in overlay_jobs {
            terminate_overlay_job(overlay);
        }
    }

    pub(super) async fn send_attached_overlay(
        &self,
        session_name: &rmux_proto::SessionName,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
        duration: Duration,
    ) -> bool {
        let handler = self.clone();
        let session_name = session_name.clone();
        let mut active_attach = self.active_attach.lock().await;
        let mut delivered = false;
        let mut removed_pids = Vec::new();

        active_attach.by_pid.retain(|pid, active| {
            if active.session_name != session_name || active.suspended {
                return true;
            }

            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let render_generation = active.render_generation;
            let overlay_generation = active.overlay_generation;
            if active
                .control_tx
                .send(AttachControl::Overlay(OverlayFrame::new(
                    overlay_frame.clone(),
                    render_generation,
                    overlay_generation,
                )))
                .is_err()
            {
                removed_pids.push(*pid);
                return false;
            }

            let control_tx = active.control_tx.clone();
            let clear_frame = clear_frame.clone();
            let handler = handler.clone();
            let session_name = session_name.clone();
            tokio::spawn(async move {
                sleep(duration).await;
                let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::new(
                    clear_frame,
                    render_generation,
                    overlay_generation,
                )));
                handler
                    .refresh_persistent_overlays_for_session(&session_name)
                    .await;
            });
            delivered = true;
            true
        });
        let removed_any = !removed_pids.is_empty();
        for pid in removed_pids {
            active_attach.forget_attached_client_windows(pid);
        }
        if removed_any {
            drop(active_attach);
            self.bump_active_attach_epoch();
        }

        delivered
    }

    pub(super) async fn send_attached_overlay_to_client(
        &self,
        attach_pid: u32,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
        duration: Duration,
    ) -> bool {
        let handler = self.clone();
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return false;
        };
        if active.suspended {
            return false;
        }

        let session_name = active.session_name.clone();
        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let render_generation = active.render_generation;
        let overlay_generation = active.overlay_generation;
        if active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::new(
                overlay_frame,
                render_generation,
                overlay_generation,
            )))
            .is_err()
        {
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return false;
        }

        let control_tx = active.control_tx.clone();
        tokio::spawn(async move {
            sleep(duration).await;
            let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::new(
                clear_frame,
                render_generation,
                overlay_generation,
            )));
            handler
                .refresh_persistent_overlays_for_session(&session_name)
                .await;
        });
        true
    }
}

fn reset_interactive_attach_state_for_session_switch(
    active: &mut ActiveAttach,
) -> Option<super::overlay_support::ClientOverlayState> {
    active.prompt = None;
    active.display_panes = None;
    active.display_panes_state_id = active.display_panes_state_id.saturating_add(1);
    active.mode_tree = None;
    active.mode_tree_frame = None;
    active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
    active
        .persistent_overlay_epoch
        .store(active.mode_tree_state_id, Ordering::SeqCst);
    active.overlay_generation = active.overlay_generation.saturating_add(1);
    active.overlay_state_id = active.overlay_state_id.saturating_add(1);
    active.overlay.take()
}

fn terminate_overlay_job(overlay: Option<super::overlay_support::ClientOverlayState>) {
    if let Some(super::overlay_support::ClientOverlayState::Popup(popup)) = overlay {
        if let Some(job) = popup.job {
            job.terminate();
        }
    }
}

pub(super) fn attach_target_for_session(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    terminal_context: &OuterTerminalContext,
    socket_path: &Path,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: None,
            key_table: None,
            terminal_context,
            render_size: None,
            window_index: None,
            master: AttachTargetMaster::Clone,
            socket_path,
        },
    )
}

#[cfg(feature = "web")]
pub(super) fn attach_render_target_for_session(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    terminal_context: &OuterTerminalContext,
    socket_path: &Path,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: None,
            key_table: None,
            terminal_context,
            render_size: None,
            window_index: None,
            master: AttachTargetMaster::Omit,
            socket_path,
        },
    )
}

#[cfg(feature = "web")]
pub(super) fn attach_render_target_for_session_window(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: Option<u32>,
    attached_count: usize,
    terminal_context: &OuterTerminalContext,
    socket_path: &Path,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: None,
            key_table: None,
            terminal_context,
            render_size: None,
            window_index,
            master: AttachTargetMaster::Omit,
            socket_path,
        },
    )
}

pub(super) fn attach_render_target_for_session_with_prompt(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    request: AttachRenderTargetRequest<'_>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: request.prompt,
            key_table: request.key_table,
            terminal_context: request.terminal_context,
            render_size: request.render_size,
            window_index: None,
            master: AttachTargetMaster::Omit,
            socket_path: request.socket_path,
        },
    )
}

pub(super) struct AttachRenderTargetRequest<'a> {
    pub(super) prompt: Option<&'a renderer::RenderedPrompt>,
    pub(super) key_table: Option<&'a str>,
    pub(super) terminal_context: &'a OuterTerminalContext,
    pub(super) render_size: Option<TerminalSize>,
    pub(super) socket_path: &'a Path,
}

#[derive(Clone, Copy)]
enum AttachTargetMaster {
    Clone,
    Omit,
}

struct AttachTargetRenderOptions<'a> {
    prompt: Option<&'a renderer::RenderedPrompt>,
    key_table: Option<&'a str>,
    terminal_context: &'a OuterTerminalContext,
    render_size: Option<TerminalSize>,
    window_index: Option<u32>,
    master: AttachTargetMaster,
    socket_path: &'a Path,
}

fn attach_target_for_session_with_prompt(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    options: AttachTargetRenderOptions<'_>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    let canonical_session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let session =
        attach_render_session(canonical_session, options.render_size, options.window_index);
    let session = session.as_ref();
    let outer_terminal = OuterTerminal::resolve_for_session(
        &state.options,
        Some(session_name),
        options.terminal_context.clone(),
    );
    let pane_output_sender = state.pane_output_for_target(
        session_name,
        session.active_window_index(),
        session.active_pane_index(),
    )?;
    // Reserve the live receiver at the same sequence boundary used by the
    // render target. Output emitted before the transport upgrade is then
    // replayable without retaining live-only passthroughs for detached panes.
    let (pane_output_start_sequence, pane_output) = pane_output_sender.subscribe_live_from_now();
    let active_pane = session.window().active_pane().cloned();
    let pane_state = session
        .active_pane_id()
        .and_then(|pane_id| state.pane_screen_state(session_name, pane_id));
    let cursor_scope = match options.prompt {
        Some(prompt) if prompt.command_prompt => CursorScope::CommandPrompt,
        Some(_) => CursorScope::Prompt,
        None => CursorScope::Pane,
    };
    let cursor_style = outer_terminal.resolve_cursor_style(
        session,
        &state.options,
        pane_state.as_ref(),
        cursor_scope,
    );
    let mut render_frame =
        outer_terminal.render_prelude(session, &state.options, pane_state.as_ref(), cursor_scope);
    render_frame.extend_from_slice(
        renderer::render_with_attached_count_prompt_and_pane_title(
            session,
            &state.options,
            attached_count,
            renderer::StatusRenderContext {
                prompt: options.prompt,
                pane_title: pane_state
                    .as_ref()
                    .map(|pane_state| pane_state.title.as_str())
                    .filter(|title| !title.is_empty()),
                state: Some(state),
                key_table: options.key_table,
                socket_path: Some(options.socket_path),
            },
        )
        .as_slice(),
    );
    for pane in session.window().panes() {
        let copy_screen = state.pane_copy_mode_render_screen(session_name, pane.id());
        if let Some(screen) = copy_screen.as_ref() {
            let pane_frame = if options.prompt.is_some() {
                renderer::render_pane_screen_preserving_prompt_cursor(
                    session,
                    &state.options,
                    pane,
                    screen,
                )
            } else {
                renderer::render_pane_screen(session, &state.options, pane, screen)
            };
            render_frame.extend_from_slice(pane_frame.as_slice());
        } else if let Some(screen) = state.pane_screen(session_name, pane.id()) {
            let pane_frame = if options.prompt.is_some() {
                renderer::render_pane_screen_preserving_prompt_cursor(
                    session,
                    &state.options,
                    pane,
                    &screen,
                )
            } else {
                renderer::render_pane_screen(session, &state.options, pane, &screen)
            };
            render_frame.extend_from_slice(pane_frame.as_slice());
        }
        if pane.index() == session.active_pane_index() && copy_screen.is_some() {
            if let (Some(summary), Some(stats)) = (
                state.pane_copy_mode_summary(session_name, pane.id()),
                state.pane_history_size_stats(session_name, pane.id()),
            ) {
                render_frame.extend_from_slice(
                    renderer::render_copy_mode_position(
                        session,
                        &state.options,
                        session.active_window_index(),
                        pane,
                        &summary,
                        stats.size,
                    )
                    .as_slice(),
                );
            }
        }
    }
    render_frame.extend_from_slice(
        renderer::render_pane_border_status_lines(session, &state.options, Some(state)).as_slice(),
    );
    let live_pane =
        live_pane_render_for_target(state, session, &state.options, session_name, options.prompt);
    if options.prompt.is_none() {
        if let Some(active_pane) = active_pane.clone() {
            if let Some(screen) = state.pane_copy_mode_render_screen(session_name, active_pane.id())
            {
                render_frame.extend_from_slice(
                    renderer::render_pane_cursor(session, &state.options, &active_pane, &screen)
                        .as_slice(),
                );
            } else if let Some(screen) = state.pane_screen(session_name, active_pane.id()) {
                render_frame.extend_from_slice(
                    renderer::render_pane_cursor(session, &state.options, &active_pane, &screen)
                        .as_slice(),
                );
            }
        }
    }

    let active_pane_geometry = active_pane.as_ref().map_or_else(
        || rmux_core::PaneGeometry::new(0, 0, 0, 0),
        |pane| {
            renderer::visible_pane_terminal_geometry(session, &state.options, pane)
                .unwrap_or_else(|| rmux_core::PaneGeometry::new(0, 0, 0, 0))
        },
    );
    let active_pane_is_starting = {
        #[cfg(windows)]
        {
            state.active_pane_is_starting(session_name)
        }
        #[cfg(not(windows))]
        {
            false
        }
    };
    let terminal_passthrough_allowed = !active_pane_is_starting
        && active_pane.as_ref().is_some_and(|pane| {
            !state.pane_in_mode(session_name, pane.id())
                && pane_passthrough_enabled(session, &state.options, pane)
        });
    let kitty_graphics_passthrough =
        terminal_passthrough_allowed && outer_terminal.supports_kitty_graphics();
    let sixel_passthrough = terminal_passthrough_allowed && outer_terminal.supports_sixel();

    let input_target = PaneTarget::with_window(
        session_name.clone(),
        session.active_window_index(),
        active_pane.as_ref().map_or(0, rmux_core::Pane::index),
    );

    Ok(AttachTarget {
        session_name: session_name.clone(),
        input_target,
        pane_master: match options.master {
            AttachTargetMaster::Clone if !active_pane_is_starting => {
                Some(state.active_pane_master(session_name)?)
            }
            AttachTargetMaster::Clone | AttachTargetMaster::Omit => None,
        },
        pane_output,
        pane_output_start_sequence,
        render_frame,
        outer_terminal,
        cursor_style,
        active_pane_geometry,
        raw_passthrough: terminal_passthrough_allowed,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id: None,
        live_pane,
    })
}

pub(super) fn sized_session(
    session: &rmux_core::Session,
    size: Option<TerminalSize>,
) -> Cow<'_, rmux_core::Session> {
    let Some(size) = size.filter(|size| size.cols > 0 && size.rows > 0) else {
        return Cow::Borrowed(session);
    };
    if size == session.window().size() {
        return Cow::Borrowed(session);
    }
    let mut resized = session.clone();
    resized.resize_terminal(size);
    Cow::Owned(resized)
}

fn attach_render_session(
    session: &rmux_core::Session,
    size: Option<TerminalSize>,
    window_index: Option<u32>,
) -> Cow<'_, rmux_core::Session> {
    let sized = sized_session(session, size);
    let Some(window_index) = window_index else {
        return sized;
    };
    if sized.active_window_index() == window_index || !sized.windows().contains_key(&window_index) {
        return sized;
    }

    let mut selected = sized.into_owned();
    selected
        .select_window(window_index)
        .expect("selected web render window was validated above");
    Cow::Owned(selected)
}

fn pane_passthrough_enabled(
    session: &rmux_core::Session,
    options: &rmux_core::OptionStore,
    pane: &rmux_core::Pane,
) -> bool {
    matches!(
        options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            OptionName::AllowPassthrough,
        ),
        Some("on" | "all")
    )
}

fn live_pane_render_for_target(
    state: &HandlerState,
    session: &rmux_core::Session,
    options: &rmux_core::OptionStore,
    session_name: &rmux_proto::SessionName,
    prompt: Option<&renderer::RenderedPrompt>,
) -> Option<Box<LivePaneRender>> {
    if prompt.is_some() {
        return None;
    }
    let pane = session.window().active_pane()?.clone();
    if state.pane_in_mode(session_name, pane.id()) {
        return None;
    }
    let target = PaneTarget::with_window(
        session_name.clone(),
        session.active_window_index(),
        pane.index(),
    );
    let transcript = state.transcript_handle(&target).ok()?;
    LivePaneRender::new_from_transcript(transcript, session.clone(), options.clone(), pane)
}

pub(super) fn option_affects_attached_rendering(option: rmux_proto::OptionName) -> bool {
    matches!(
        option,
        rmux_proto::OptionName::ExtendedKeys
            | rmux_proto::OptionName::AllowPassthrough
            | rmux_proto::OptionName::FocusEvents
            | rmux_proto::OptionName::Mouse
            | rmux_proto::OptionName::SetClipboard
            | rmux_proto::OptionName::TerminalFeatures
            | rmux_proto::OptionName::TerminalOverrides
    ) || rmux_core::option_affects_rendering(option)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    use rmux_core::{command_parser::CommandParser, OptionStore, PaneGeometry};
    use rmux_os::identity::UserIdentity;
    use rmux_proto::{NewSessionRequest, PaneTarget, Request, Response, SessionName, TerminalSize};
    use tokio::sync::mpsc;

    use super::{
        reset_interactive_attach_state_for_session_switch, ActiveAttach, AttachRegistration,
        RequestHandler, ATTACH_CONTROL_BACKLOG_LIMIT,
    };
    use crate::client_flags::ClientFlags;
    use crate::handler::scripting_support::QueueExecutionContext;
    use crate::mouse::ClientMouseState;
    use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
    use crate::pane_io::{pane_output_channel, AttachControl, AttachTarget};
    use crate::server_access::current_owner_uid;

    #[tokio::test]
    async fn attach_control_backlog_limit_removes_slow_client() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session name");
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let control_backlog = Arc::new(AtomicUsize::new(ATTACH_CONTROL_BACKLOG_LIMIT));
        let uid = current_owner_uid();

        handler
            .register_attach_with_access(
                77,
                session_name.clone(),
                AttachRegistration {
                    control_tx,
                    control_backlog: control_backlog.clone(),
                    closing: Arc::new(AtomicBool::new(false)),
                    persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                    terminal_context: OuterTerminalContext::default(),
                    flags: ClientFlags::default(),
                    render_stream: true,
                    uid,
                    user: UserIdentity::Uid(uid),
                    can_write: true,
                    client_size: Some(TerminalSize { cols: 80, rows: 24 }),
                },
            )
            .await;

        let error = handler
            .send_attach_control(77, AttachControl::Refresh, "refresh-client", None)
            .await
            .expect_err("overloaded attach client should reject refresh");

        assert!(error.to_string().contains("not draining updates"));
        assert_eq!(
            control_backlog.load(Ordering::Acquire),
            ATTACH_CONTROL_BACKLOG_LIMIT
        );
        assert!(!handler.active_attach.lock().await.by_pid.contains_key(&77));
    }

    #[tokio::test]
    async fn render_stream_refresh_substitution_does_not_advance_render_generation() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session name");
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let control_backlog = Arc::new(AtomicUsize::new(0));
        let uid = current_owner_uid();

        handler
            .register_attach_with_access(
                77,
                session_name.clone(),
                AttachRegistration {
                    control_tx,
                    control_backlog: control_backlog.clone(),
                    closing: Arc::new(AtomicBool::new(false)),
                    persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                    terminal_context: OuterTerminalContext::default(),
                    flags: ClientFlags::default(),
                    render_stream: true,
                    uid,
                    user: UserIdentity::Uid(uid),
                    can_write: true,
                    client_size: Some(TerminalSize { cols: 80, rows: 24 }),
                },
            )
            .await;

        let pane_output = pane_output_channel();
        let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
        let target = AttachTarget {
            session_name: session_name.clone(),
            input_target: PaneTarget::new(session_name.clone(), 0),
            pane_master: None,
            pane_output,
            pane_output_start_sequence,
            render_frame: b"BASE".to_vec(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::default(),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
        };

        handler
            .send_attach_control(
                77,
                AttachControl::switch(target),
                "switch-client",
                Some(session_name.clone()),
            )
            .await
            .expect("render-stream refresh substitution should be accepted");

        assert!(matches!(control_rx.try_recv(), Ok(AttachControl::Refresh)));
        assert_eq!(control_backlog.load(Ordering::Acquire), 0);
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach.by_pid.get(&77).expect("attach is active");
        assert_eq!(
            active.render_generation, 0,
            "server generation must only count Switch controls actually sent to the client"
        );
        assert!(active.render_refresh_pending);
    }

    #[tokio::test]
    async fn session_switch_dismisses_mode_tree_pane_mode() {
        let handler = RequestHandler::new();
        let alpha = SessionName::new("alpha").expect("valid session name");
        let beta = SessionName::new("beta").expect("valid session name");
        for session_name in [&alpha, &beta] {
            assert!(matches!(
                handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session_name.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
                Response::NewSession(_)
            ));
        }

        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        handler.register_attach(77, alpha.clone(), control_tx).await;

        let parsed = CommandParser::new()
            .parse_arguments(["choose-tree"])
            .expect("choose-tree parses");
        let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
            .expect("mode tree parse succeeds")
            .expect("choose-tree is a mode tree command");
        handler
            .execute_queued_mode_tree(77, command, &QueueExecutionContext::without_caller_cwd())
            .await
            .expect("mode tree opens");

        let alpha_pane_id = {
            let state = handler.state.lock().await;
            let session = state.sessions.session(&alpha).expect("alpha exists");
            let pane = session
                .window_at(0)
                .expect("alpha window exists")
                .pane(0)
                .expect("alpha pane exists");
            assert_eq!(state.pane_mode_name(&alpha, pane.id()), Some("tree-mode"));
            pane.id()
        };

        let pane_output = pane_output_channel();
        let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
        let target = AttachTarget {
            session_name: beta.clone(),
            input_target: PaneTarget::new(beta.clone(), 0),
            pane_master: None,
            pane_output,
            pane_output_start_sequence,
            render_frame: b"BETA".to_vec(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::default(),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
        };

        handler
            .send_attach_control(
                77,
                AttachControl::switch(target),
                "switch-client",
                Some(beta),
            )
            .await
            .expect("session switch succeeds");

        {
            let state = handler.state.lock().await;
            assert!(
                !state.pane_in_mode(&alpha, alpha_pane_id),
                "switching away from a mode-tree session must clear the host pane mode"
            );
            assert_eq!(state.pane_mode_name(&alpha, alpha_pane_id), None);
        }
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&77)
            .expect("attach remains active");
        assert_eq!(active.session_name.as_str(), "beta");
        assert!(active.mode_tree.is_none());
        assert!(active.mode_tree_frame.is_none());
    }

    #[test]
    fn session_switch_resets_stale_interactive_overlay_state() {
        let session_name = SessionName::new("alpha").expect("valid session name");
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let persistent_overlay_epoch = Arc::new(AtomicU64::new(7));
        let uid = current_owner_uid();
        let mut active = ActiveAttach {
            id: 1,
            session_name,
            last_session: None,
            flags: ClientFlags::default(),
            pan_window: Some(2),
            pan_ox: 3,
            pan_oy: 4,
            control_tx,
            control_backlog: Arc::new(AtomicUsize::new(0)),
            render_stream: true,
            render_refresh_pending: false,
            uid,
            user: UserIdentity::Uid(uid),
            can_write: true,
            suspended: false,
            closing: Arc::new(AtomicBool::new(false)),
            terminal_context: OuterTerminalContext::default(),
            client_size: TerminalSize { cols: 80, rows: 24 },
            client_pixels: None,
            size_sequence: 0,
            persistent_overlay_epoch: persistent_overlay_epoch.clone(),
            render_generation: 5,
            overlay_generation: 11,
            overlay_state_id: 13,
            display_panes_state_id: 17,
            key_table_name: None,
            key_table_set_at: None,
            repeat_deadline: None,
            repeat_active: false,
            last_key: None,
            mouse: ClientMouseState {
                slider_mpos: -1,
                ..ClientMouseState::default()
            },
            prompt: None,
            mode_tree_state_id: 7,
            mode_tree: None,
            mode_tree_frame: Some(b"stale-tree-frame".to_vec()),
            overlay: None,
            display_panes: None,
        };

        let overlay = reset_interactive_attach_state_for_session_switch(&mut active);

        assert!(overlay.is_none());
        assert!(active.prompt.is_none());
        assert!(active.mode_tree.is_none());
        assert!(active.mode_tree_frame.is_none());
        assert!(active.overlay.is_none());
        assert!(active.display_panes.is_none());
        assert_eq!(active.mode_tree_state_id, 8);
        assert_eq!(persistent_overlay_epoch.load(Ordering::SeqCst), 8);
        assert_eq!(active.overlay_generation, 12);
        assert_eq!(active.overlay_state_id, 14);
        assert_eq!(active.display_panes_state_id, 18);
        assert_eq!(active.render_generation, 5);
    }
}
