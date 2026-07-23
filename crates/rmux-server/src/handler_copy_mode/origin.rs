use super::key_binding::DirectCopyModeCommand;
use super::{ensure_copy_mode_session_identity, ActiveAttachIdentity, RequestHandler};
use crate::copy_mode::{CopyModeLineNumberLayout, CopyModeMouseContext};
use crate::mouse::{
    copy_mode_mouse_context_with_line_numbers,
    copy_mode_mouse_drag_start_context_with_line_numbers, AttachedMouseEvent,
};
use rmux_proto::{OptionName, PaneTarget};

#[derive(Debug, Clone)]
pub(super) enum CopyModeCommandOrigin {
    NonMouse,
    Mouse(AttachedMouseEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CopyModeMouseContextKind {
    Position,
    DragSelectionStart,
}

impl From<Option<AttachedMouseEvent>> for CopyModeCommandOrigin {
    fn from(event: Option<AttachedMouseEvent>) -> Self {
        match event {
            Some(event) => Self::Mouse(event),
            None => Self::NonMouse,
        }
    }
}

pub(super) struct CopyModeCommandInvocation<'a> {
    pub(super) target: PaneTarget,
    pub(super) command: &'a str,
    pub(super) args: &'a [String],
    pub(super) repeat_count: usize,
    pub(super) origin: CopyModeCommandOrigin,
}

impl RequestHandler {
    pub(in crate::handler) async fn execute_direct_copy_mode_binding(
        &self,
        requester_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        target: PaneTarget,
        command: &DirectCopyModeCommand,
        mouse_event: Option<AttachedMouseEvent>,
    ) -> Result<(), rmux_proto::RmuxError> {
        match (identity, mouse_event) {
            (Some(identity), Some(mouse_event)) => {
                self.execute_copy_mode_command_for_identity_with_mouse_event(
                    identity,
                    target,
                    &command.command,
                    &command.args,
                    command.repeat_count,
                    mouse_event,
                )
                .await
            }
            (Some(identity), None) => {
                self.execute_copy_mode_command_for_identity(
                    identity,
                    target,
                    &command.command,
                    &command.args,
                    command.repeat_count,
                )
                .await
            }
            (None, Some(mouse_event)) => {
                self.execute_copy_mode_command_with_mouse_event(
                    requester_pid,
                    target,
                    &command.command,
                    &command.args,
                    command.repeat_count,
                    mouse_event,
                )
                .await
            }
            (None, None) => {
                self.execute_copy_mode_command(
                    requester_pid,
                    target,
                    &command.command,
                    &command.args,
                    command.repeat_count,
                )
                .await
            }
        }
    }
}

pub(super) async fn mouse_context_for_origin(
    handler: &RequestHandler,
    requester_pid: u32,
    identity: Option<ActiveAttachIdentity>,
    target: &PaneTarget,
    event: &AttachedMouseEvent,
    kind: CopyModeMouseContextKind,
) -> Option<CopyModeMouseContext> {
    let attach_pid = match identity {
        Some(identity) => identity.attach_pid(),
        None => handler
            .resolve_attached_client_pid(requester_pid, "send-keys")
            .await
            .ok()?,
    };
    let (slider_mpos, expected_session_id) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach.by_pid.get(&attach_pid).filter(|active| {
            identity.is_none_or(|identity| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        })?;
        (
            active.mouse.slider_mpos,
            identity.map(|_| active.session_id),
        )
    };
    let state = handler.state.lock().await;
    ensure_copy_mode_session_identity(&state, target, expected_session_id).ok()?;
    let session = state.sessions.session(target.session_name())?;
    let pane = session
        .window_at(target.window_index())?
        .pane(target.pane_index())?;
    let summary = state.pane_copy_mode_summary(target.session_name(), pane.id())?;
    let line_numbers = CopyModeLineNumberLayout::resolve(
        state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::CopyModeLineNumbers,
        ),
        summary.line_numbers_enabled,
        summary.history_size,
        summary.backing_rows,
        summary.scroll_position,
        summary.cursor_y,
    );
    let pane_geometry = crate::mouse::layout_for_session(&state, target.session_name(), 1)
        .and_then(|layout| {
            layout
                .panes
                .into_iter()
                .find(|candidate| candidate.pane_id == pane.id())
                .map(|candidate| candidate.geometry)
        })
        .unwrap_or_else(|| pane.geometry());
    let mut context = match kind {
        CopyModeMouseContextKind::Position => copy_mode_mouse_context_with_line_numbers(
            event,
            pane_geometry,
            slider_mpos,
            line_numbers,
        ),
        CopyModeMouseContextKind::DragSelectionStart => {
            copy_mode_mouse_drag_start_context_with_line_numbers(
                event,
                pane_geometry,
                slider_mpos,
                line_numbers,
            )
        }
    }?;
    context.move_cursor_before_command = !event.is_wheel();
    Some(context)
}
