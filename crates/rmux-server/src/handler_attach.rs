use std::borrow::Cow;
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

#[path = "handler_attach/key_table.rs"]
mod key_table;
#[path = "handler_attach/refresh.rs"]
mod refresh;
#[path = "handler_attach/registration.rs"]
mod registration;
#[path = "handler_attach/state.rs"]
mod state;

pub(crate) use crate::client_flags::ClientFlags;
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
    )> {
        let active_attach = self.active_attach.lock().await;
        active_attach.by_pid.get(&attach_pid).map(|active| {
            (
                active.terminal_context.clone(),
                active.client_size,
                active.client_pixels,
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

        if matches!(command, AttachControl::Switch(_)) {
            active.render_generation = active.render_generation.saturating_add(1);
        }
        if matches!(
            command,
            AttachControl::Detach
                | AttachControl::Exited
                | AttachControl::DetachKill
                | AttachControl::DetachExecShellCommand(_)
        ) {
            active.closing.store(true, Ordering::SeqCst);
        }
        if active.control_tx.send(command).is_err() {
            active_attach.by_pid.remove(&attach_pid);
            return Err(attached_client_required(command_name));
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

        Ok(previous_session_name)
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
            active.closing.store(true, Ordering::SeqCst);
            let _ = active.control_tx.send(control());
            false
        });
        drop(active_attach);
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

        active_attach.by_pid.retain(|_, active| {
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
            active_attach.by_pid.remove(&attach_pid);
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
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        None,
        None,
        terminal_context,
        None,
    )
}

fn attach_target_for_session_with_prompt(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    prompt: Option<&renderer::RenderedPrompt>,
    key_table: Option<&str>,
    terminal_context: &OuterTerminalContext,
    render_size: Option<TerminalSize>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    let canonical_session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let session = sized_session(canonical_session, render_size);
    let session = session.as_ref();
    let outer_terminal = OuterTerminal::resolve_for_session(
        &state.options,
        Some(session_name),
        terminal_context.clone(),
    );
    let active_pane = session.window().active_pane().cloned();
    let pane_state = session
        .active_pane_id()
        .and_then(|pane_id| state.pane_screen_state(session_name, pane_id));
    let cursor_scope = match prompt {
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
            prompt,
            pane_state
                .as_ref()
                .map(|pane_state| pane_state.title.as_str())
                .filter(|title| !title.is_empty()),
            Some(state),
            key_table,
        )
        .as_slice(),
    );
    for pane in session.window().panes() {
        let copy_screen = state.pane_copy_mode_render_screen(session_name, pane.id());
        let screen = copy_screen
            .clone()
            .or_else(|| state.pane_render_screen(session_name, pane.id()));
        if let Some(screen) = screen {
            render_frame.extend_from_slice(
                renderer::render_pane_screen(session, &state.options, pane, &screen).as_slice(),
            );
        }
        if pane.index() == session.active_pane_index() && copy_screen.is_some() {
            if let (Some(summary), Some(stats)) = (
                state.pane_copy_mode_summary(session_name, pane.id()),
                state.pane_history_stats(session_name, pane.id()),
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
        live_pane_render_for_target(state, session, &state.options, session_name, prompt);
    if prompt.is_none() {
        if let Some(active_pane) = active_pane.clone() {
            let active_screen = state
                .pane_copy_mode_render_screen(session_name, active_pane.id())
                .or_else(|| state.pane_render_screen(session_name, active_pane.id()));
            if let Some(screen) = active_screen.as_ref() {
                render_frame.extend_from_slice(
                    renderer::render_pane_cursor(session, &state.options, &active_pane, screen)
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
    let terminal_passthrough_allowed = active_pane.as_ref().is_some_and(|pane| {
        !state.pane_in_mode(session_name, pane.id())
            && pane_passthrough_enabled(session, &state.options, pane)
    });
    let kitty_graphics_passthrough =
        terminal_passthrough_allowed && outer_terminal.supports_kitty_graphics();
    let sixel_passthrough = terminal_passthrough_allowed && outer_terminal.supports_sixel();

    Ok(AttachTarget {
        session_name: session_name.clone(),
        pane_master: state.active_pane_master(session_name)?,
        pane_output: state.active_pane_output(session_name)?,
        render_frame,
        outer_terminal,
        cursor_style,
        active_pane_geometry,
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
    let screen = state.pane_render_screen(session_name, pane.id())?;
    let target = PaneTarget::with_window(
        session_name.clone(),
        session.active_window_index(),
        pane.index(),
    );
    let transcript = state.transcript_handle(&target).ok()?;
    LivePaneRender::new(transcript, session.clone(), options.clone(), pane, &screen)
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
