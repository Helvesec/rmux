use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    AttachShellCommand, AttachedKeystroke, KeyDispatched, OptionName, PaneTarget, SessionId,
    SessionName, TerminalSize,
};

use super::{client_support::SwitchTargetSelection, RequestHandler};
use crate::client_format::{sort_client_loop_snapshots, ClientLoopSnapshot};
use crate::handler::client_runtime_support::ListClientSnapshot;
use crate::handler_support::attached_client_required;
use crate::key_table::effective_client_key_table_name;
use crate::outer_terminal::{
    ClientPathUpdate, ClientTitleState, ClientTitleUpdate, CursorScope, OuterTerminal,
    OuterTerminalContext,
};
use crate::pane_io::{AttachControl, AttachTarget, LivePaneRender, OverlayFrame};
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::pane_transcript::PresentationCapture;
use crate::renderer;
use crate::terminal::TerminalProfile;

// 64 × 128 KiB = 8 MiB shared by every queued attach-control payload. Small
// controls consume one unit so the message count is bounded by the same limit.
// Saturation permits only one additional fixed-size, accounted Detach sentinel.
pub(super) const ATTACH_CONTROL_BACKLOG_LIMIT: usize = 64;

#[derive(Default)]
struct AttachControlIdentityExpectation {
    attach_id: Option<u64>,
    current_session_id: Option<SessionId>,
    next_session: Option<(SessionName, SessionId)>,
    target_selection: Option<SwitchTargetSelection>,
    mode_tree_requester: Option<super::mode_tree_support::ModeTreeActionIdentity>,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct AttachControlIdentityPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static ATTACH_CONTROL_IDENTITY_PAUSE: std::sync::Mutex<
    Vec<(u32, std::sync::Arc<AttachControlIdentityPause>)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(in crate::handler) fn install_attach_control_identity_pause(
    attach_pid: u32,
) -> std::sync::Arc<AttachControlIdentityPause> {
    let pause = std::sync::Arc::new(AttachControlIdentityPause::default());
    let mut installed = ATTACH_CONTROL_IDENTITY_PAUSE
        .lock()
        .expect("attach control identity pause lock");
    if let Some((_, current)) = installed
        .iter_mut()
        .find(|(paused_pid, _)| *paused_pid == attach_pid)
    {
        *current = pause.clone();
    } else {
        installed.push((attach_pid, pause.clone()));
    }
    pause
}

#[cfg(test)]
async fn pause_after_attach_control_identity_capture(attach_pid: u32) {
    let pause = {
        let mut installed = ATTACH_CONTROL_IDENTITY_PAUSE
            .lock()
            .expect("attach control identity pause lock");
        installed
            .iter()
            .position(|(paused_pid, _)| *paused_pid == attach_pid)
            .map(|index| installed.swap_remove(index).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct TransientRestoreCommitPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static TRANSIENT_RESTORE_COMMIT_PAUSE: std::sync::Mutex<
    Vec<(u32, std::sync::Arc<TransientRestoreCommitPause>)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(in crate::handler) fn install_transient_restore_commit_pause(
    attach_pid: u32,
) -> std::sync::Arc<TransientRestoreCommitPause> {
    let pause = std::sync::Arc::new(TransientRestoreCommitPause::default());
    let mut installed = TRANSIENT_RESTORE_COMMIT_PAUSE
        .lock()
        .expect("transient restore pause lock");
    if let Some((_, current)) = installed
        .iter_mut()
        .find(|(paused_pid, _)| *paused_pid == attach_pid)
    {
        *current = pause.clone();
    } else {
        installed.push((attach_pid, pause.clone()));
    }
    pause
}

#[cfg(test)]
async fn pause_before_transient_restore_commit(attach_pid: u32) {
    let pause = {
        let mut installed = TRANSIENT_RESTORE_COMMIT_PAUSE
            .lock()
            .expect("transient restore pause lock");
        installed
            .iter()
            .position(|(paused_pid, _)| *paused_pid == attach_pid)
            .map(|index| installed.swap_remove(index).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

/// Holds a generic redraw between rendering its frame and delivering it, so a
/// regression can install the same-pid replacement the delivery must reject.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct GenericRenderDeliveryPause {
    pub(crate) reached: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}

#[cfg(test)]
static GENERIC_RENDER_DELIVERY_PAUSE: std::sync::Mutex<
    Vec<(u32, std::sync::Arc<GenericRenderDeliveryPause>)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn install_generic_render_delivery_pause(
    attach_pid: u32,
) -> std::sync::Arc<GenericRenderDeliveryPause> {
    let pause = std::sync::Arc::new(GenericRenderDeliveryPause::default());
    let mut installed = GENERIC_RENDER_DELIVERY_PAUSE
        .lock()
        .expect("generic render delivery pause lock");
    if let Some((_, current)) = installed
        .iter_mut()
        .find(|(paused_pid, _)| *paused_pid == attach_pid)
    {
        *current = pause.clone();
    } else {
        installed.push((attach_pid, pause.clone()));
    }
    pause
}

#[cfg(test)]
async fn pause_before_generic_render_delivery(attach_pid: u32) {
    let pause = {
        let mut installed = GENERIC_RENDER_DELIVERY_PAUSE
            .lock()
            .expect("generic render delivery pause lock");
        installed
            .iter()
            .position(|(paused_pid, _)| *paused_pid == attach_pid)
            .map(|index| installed.swap_remove(index).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

mod control_delivery;
mod detach_lifecycle;
mod overlay_delivery;
mod render_target;

use detach_lifecycle::{reset_interactive_attach_state_for_session_switch, terminate_overlay_job};
pub(super) use overlay_delivery::render_status_message_for_attached_size;
pub(super) use render_target::attach_target_for_session;
use render_target::attach_target_for_session_with_prompt;
#[cfg(feature = "web")]
pub(super) use render_target::attach_target_for_web_session;

mod input_path;
mod key_table;
mod refresh;
mod refresh_identity;
mod registration;
pub(crate) use registration::AttachRegistrationTarget;
mod resize_policy;
mod session_destroy;
mod state;
mod status_refresh;
mod switch_commit;
mod transient_message;

pub(crate) use crate::client_flags::ClientFlags;
#[cfg(test)]
pub(in crate::handler) use input_path::test_barrier;
pub(in crate::handler) use input_path::{
    check_queued_input_target, on_attached_input_path, replays_queued_input,
    runs_on_attached_input_path, FrameCursor, FramePart, InputArrival, InputPathWork, OrderedInput,
};
pub(in crate::handler) use key_table::AttachedKeyTableCommit;
pub(in crate::handler) use resize_policy::{
    control_window_size_limit, linked_window_client_content_size,
    prepare_applied_window_resize_events, resize_control_session_for_client,
    surviving_attached_resize_targets, switch_control_session_for_client, AttachedWindowSizePolicy,
    ControlResizeClient, IncomingSizeClient,
};
pub(in crate::handler) use session_destroy::{
    PreparedAttachedDestroySwitches, SessionDetachOnDestroy,
};
pub(super) use state::{
    ActiveAttach, ActiveAttachState, AttachGeneration, AttachedClientControlOutcome,
    DisplayPanesClientState, DisplayPanesLabel,
};
pub(crate) use state::{ActiveAttachIdentity, AttachRegistration};
pub(in crate::handler) use switch_commit::{
    AttachedSwitchCommitRequest, AttachedSwitchCommittedTarget,
};
pub(in crate::handler) use transient_message::{
    append_transient_message_frame, cancel_transient_message, compose_transient_message_refresh,
    transient_message_render_snapshot, TransientMessageInput, TransientMessageInputPolicy,
    TransientMessageRenderSnapshot, TransientMessageRestoreGuard,
};

impl RequestHandler {
    #[cfg_attr(not(test), allow(dead_code))]
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

    pub(crate) async fn handle_attached_keystroke_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        keystroke: &AttachedKeystroke,
        consumed: bool,
    ) -> Result<KeyDispatched, rmux_proto::RmuxError> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| {
                rmux_proto::RmuxError::Server("attached client disappeared".to_owned())
            })?;
        let _ = active;
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

    pub(super) async fn terminal_context_and_size_for_attached_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
    ) -> Option<(
        OuterTerminalContext,
        TerminalSize,
        Option<rmux_proto::TerminalPixels>,
        bool,
        ClientFlags,
    )> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| active.id == expected_attach_id)
            .map(|active| {
                (
                    active.terminal_context.clone(),
                    active.client_size,
                    active.client_pixels,
                    active.render_stream,
                    active.flags,
                )
            })
    }

    #[cfg(test)]
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
}

pub(super) struct AttachSessionRenderOptions<'a> {
    pub(super) attached_count: usize,
    pub(super) terminal_context: &'a OuterTerminalContext,
    pub(super) previous_title: Option<&'a ClientTitleState>,
    pub(super) client: Option<&'a ListClientSnapshot>,
    pub(super) socket_path: &'a Path,
    pub(super) client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
}

pub(super) struct AttachSessionSwitchRenderOptions<'a> {
    pub(super) attached_count: usize,
    pub(super) key_table: Option<&'a str>,
    pub(super) previous_title: Option<&'a ClientTitleState>,
    pub(super) client: Option<&'a ListClientSnapshot>,
    pub(super) terminal_context: &'a OuterTerminalContext,
    pub(super) socket_path: &'a Path,
    pub(super) render_stream: bool,
    pub(super) selection: Option<&'a SwitchTargetSelection>,
    pub(super) window_size_override: Option<AttachWindowSizeOverride>,
    pub(super) client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
}

#[derive(Clone, Copy)]
pub(super) struct AttachWindowSizeOverride {
    window_index: u32,
    terminal_size: TerminalSize,
    content_size: TerminalSize,
}

impl AttachWindowSizeOverride {
    const fn new(
        window_index: u32,
        terminal_size: TerminalSize,
        content_size: TerminalSize,
    ) -> Self {
        Self {
            window_index,
            terminal_size,
            content_size,
        }
    }
}

pub(super) fn attach_target_for_session_switch(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    options: AttachSessionSwitchRenderOptions<'_>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    let AttachSessionSwitchRenderOptions {
        attached_count,
        key_table,
        previous_title,
        client,
        terminal_context,
        socket_path,
        render_stream,
        selection,
        window_size_override,
        client_loop_snapshots,
    } = options;
    #[cfg(feature = "web")]
    let master = if render_stream {
        AttachTargetMaster::Omit
    } else {
        AttachTargetMaster::Clone
    };
    #[cfg(not(feature = "web"))]
    let master = {
        let _ = render_stream;
        AttachTargetMaster::Clone
    };
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: None,
            key_table,
            previous_title,
            client,
            terminal_context,
            render_size: None,
            window_index: None,
            selection,
            window_size_override,
            client_loop_snapshots,
            master,
            bounded_recovery: render_stream,
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
            previous_title: None,
            client: None,
            terminal_context,
            render_size: None,
            window_index,
            selection: None,
            window_size_override: None,
            client_loop_snapshots: None,
            master: AttachTargetMaster::Omit,
            bounded_recovery: true,
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
            previous_title: request.previous_title,
            client: request.client,
            terminal_context: request.terminal_context,
            render_size: request.render_size,
            window_index: None,
            selection: None,
            window_size_override: None,
            client_loop_snapshots: request.client_loop_snapshots,
            master: AttachTargetMaster::Omit,
            bounded_recovery: false,
            socket_path: request.socket_path,
        },
    )
}

pub(super) struct AttachRenderTargetRequest<'a> {
    pub(super) prompt: Option<&'a renderer::RenderedPrompt>,
    pub(super) key_table: Option<&'a str>,
    pub(super) previous_title: Option<&'a ClientTitleState>,
    /// The client this frame is for, so `set-titles-string` expands its own
    /// `#{client_*}` values rather than leaving them empty (issue #182).
    pub(super) client: Option<&'a ListClientSnapshot>,
    pub(super) terminal_context: &'a OuterTerminalContext,
    pub(super) render_size: Option<TerminalSize>,
    pub(super) socket_path: &'a Path,
    pub(super) client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
}

/// Everything a render needs from one attached client.
///
/// Taken under the client lock alone so the state lock can be acquired
/// afterwards without holding both, which is why it owns its values.
pub(super) struct ClientRenderSnapshot {
    /// The exact attach generation these values were taken from.
    ///
    /// Registration lets a replacement take the same pid, and the render
    /// between the two attach-lock holds runs under other locks entirely, so
    /// delivery has to prove it is still handing the frame to the client the
    /// frame describes (issue #182).
    pub(super) identity: ActiveAttachIdentity,
    pub(super) prompt: Option<renderer::RenderedPrompt>,
    pub(super) terminal_context: OuterTerminalContext,
    pub(super) client_size: TerminalSize,
    pub(super) mode_tree_state_id: u64,
    pub(super) mode_tree_active: bool,
    pub(super) key_table: Option<String>,
    pub(super) transient_message: Option<TransientMessageRenderSnapshot>,
    pub(super) previous_title: ClientTitleState,
    pub(super) client: ListClientSnapshot,
}

impl ClientRenderSnapshot {
    pub(super) fn capture(attach_pid: u32, active: &ActiveAttach) -> Self {
        Self {
            identity: active.identity(attach_pid),
            prompt: active
                .prompt
                .as_ref()
                .map(super::prompt_support::ClientPromptState::rendered_prompt),
            terminal_context: active.terminal_context.clone(),
            client_size: active.client_size,
            mode_tree_state_id: active.mode_tree_state_id,
            mode_tree_active: active.mode_tree.is_some(),
            key_table: active.key_table_name.clone(),
            transient_message: transient_message_render_snapshot(active),
            previous_title: active.client_title.clone(),
            client: ListClientSnapshot::from_attached_client(attach_pid, active),
        }
    }

    /// The render request this client's next frame is built from.
    pub(super) fn render_request<'a>(
        &'a self,
        socket_path: &'a Path,
        client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
    ) -> AttachRenderTargetRequest<'a> {
        AttachRenderTargetRequest {
            prompt: self.prompt.as_ref(),
            key_table: self.key_table.as_deref(),
            previous_title: Some(&self.previous_title),
            client: Some(&self.client),
            terminal_context: &self.terminal_context,
            render_size: Some(self.client_size),
            socket_path,
            client_loop_snapshots,
        }
    }

    /// Stamps the persistent-overlay state a client with an open overlay
    /// requires, so a frame rendered before a barrier is not drawn after it.
    pub(super) fn stamp_persistent_overlay_state(&self, target: &mut AttachTarget) {
        if self.mode_tree_active {
            target.persistent_overlay_state_id = Some(self.mode_tree_state_id);
        }
    }
}

#[derive(Clone, Copy)]
enum AttachTargetMaster {
    Clone,
    Omit,
}

struct AttachTargetRenderOptions<'a> {
    prompt: Option<&'a renderer::RenderedPrompt>,
    key_table: Option<&'a str>,
    /// What this client's outer terminal already shows, so an unchanged
    /// expansion is not re-emitted on every refresh.
    previous_title: Option<&'a ClientTitleState>,
    /// The client this frame is for, source of the `#{client_*}` bindings the
    /// title expands against.
    client: Option<&'a ListClientSnapshot>,
    terminal_context: &'a OuterTerminalContext,
    render_size: Option<TerminalSize>,
    window_index: Option<u32>,
    selection: Option<&'a SwitchTargetSelection>,
    window_size_override: Option<AttachWindowSizeOverride>,
    client_loop_snapshots: Option<&'a [ClientLoopSnapshot]>,
    master: AttachTargetMaster,
    bounded_recovery: bool,
    socket_path: &'a Path,
}

pub(super) fn sized_session(
    session: rmux_core::ManagedSessionView<'_>,
    size: Option<TerminalSize>,
) -> rmux_core::ManagedSessionPresentation<'_> {
    let mut rendered = rmux_core::ManagedSessionPresentation::new(session);
    if let Some(size) = size.filter(|size| size.cols > 0 && size.rows > 0) {
        rendered.set_terminal_size(size);
    }
    rendered
}

fn attach_render_session<'a>(
    session: rmux_core::ManagedSessionView<'a>,
    size: Option<TerminalSize>,
    window_index: Option<u32>,
    selection: Option<&SwitchTargetSelection>,
    window_size_override: Option<AttachWindowSizeOverride>,
) -> Result<rmux_core::ManagedSessionPresentation<'a>, rmux_proto::RmuxError> {
    let mut rendered = sized_session(session, size);
    if let Some(size_override) = window_size_override {
        rendered.set_terminal_size(size_override.terminal_size);
        rendered.resize_window(size_override.window_index, size_override.content_size)?;
    }
    if let Some(selection) = selection {
        if selection.session_name() != session.name() {
            return Err(rmux_proto::RmuxError::Server(
                "switch render selection changed sessions before commit".to_owned(),
            ));
        }
        selection.apply_to_presentation(&mut rendered)?;
    } else if let Some(window_index) = window_index {
        if rendered.view().active_window_index() != window_index
            && rendered.view().window_at(window_index).is_some()
        {
            rendered.select_window(window_index)?;
        }
    }
    Ok(rendered)
}

pub(super) fn option_affects_attached_rendering(option: rmux_proto::OptionName) -> bool {
    matches!(
        option,
        rmux_proto::OptionName::ExtendedKeys
            | rmux_proto::OptionName::AllowPassthrough
            | rmux_proto::OptionName::FocusEvents
            | rmux_proto::OptionName::FocusFollowsMouse
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
    use std::time::Duration;

    use rmux_core::{command_parser::CommandParser, OptionStore, PaneGeometry};
    use rmux_os::identity::UserIdentity;
    use rmux_proto::{
        KillSessionRequest, NewSessionRequest, OptionName, Request, Response, ScopeSelector,
        SessionName, SetOptionMode, TerminalSize,
    };
    use tokio::sync::mpsc;

    use super::{
        attach_target_for_session, install_attach_control_identity_pause,
        reset_interactive_attach_state_for_session_switch, state::AttachClientSizeProvenance,
        ActiveAttach, AttachRegistration, AttachSessionRenderOptions, RequestHandler,
        SessionDetachOnDestroy, ATTACH_CONTROL_BACKLOG_LIMIT,
    };
    use crate::client_flags::ClientFlags;
    use crate::handler::scripting_support::QueueExecutionContext;
    use crate::mouse::ClientMouseState;
    use crate::outer_terminal::{ClientTitleState, OuterTerminal, OuterTerminalContext};
    use crate::pane_io::{pane_output_channel, AttachControl, AttachTarget};
    use crate::server_access::current_owner_uid;

    #[test]
    fn focus_follows_mouse_changes_refresh_attached_terminals() {
        assert!(super::option_affects_attached_rendering(
            OptionName::FocusFollowsMouse
        ));
    }

    #[tokio::test]
    async fn attach_control_backlog_limit_removes_slow_client() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session name");
        create_test_session(&handler, &session_name).await;
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let control_backlog = Arc::new(AtomicUsize::new(ATTACH_CONTROL_BACKLOG_LIMIT));
        let uid = current_owner_uid();

        handler
            .register_attach_with_access(
                77,
                session_name.clone(),
                None,
                AttachRegistration {
                    control_tx,
                    control_backlog: control_backlog.clone(),
                    closing: Arc::new(AtomicBool::new(false)),
                    persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                    terminal_context: OuterTerminalContext::default(),
                    client_title: None,
                    flags: ClientFlags::default(),
                    render_stream: true,
                    uid,
                    user: UserIdentity::Uid(uid),
                    can_write: true,
                    client_size: Some(TerminalSize { cols: 80, rows: 24 }),
                },
            )
            .await
            .expect("attach registration succeeds");

        let error = handler
            .send_attach_control(77, AttachControl::Refresh, "refresh-client")
            .await
            .expect_err("overloaded attach client should reject refresh");

        assert!(error.to_string().contains("not draining updates"));
        assert_eq!(
            control_backlog.load(Ordering::Acquire),
            ATTACH_CONTROL_BACKLOG_LIMIT + 1,
            "the bounded queue includes one accounted terminal detach sentinel"
        );
        assert!(!handler.active_attach.lock().await.by_pid.contains_key(&77));
    }

    #[tokio::test]
    async fn render_stream_refresh_substitution_does_not_advance_render_generation() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session name");
        create_test_session(&handler, &session_name).await;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let control_backlog = Arc::new(AtomicUsize::new(0));
        let uid = current_owner_uid();

        handler
            .register_attach_with_access(
                77,
                session_name.clone(),
                None,
                AttachRegistration {
                    control_tx,
                    control_backlog: control_backlog.clone(),
                    closing: Arc::new(AtomicBool::new(false)),
                    persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                    terminal_context: OuterTerminalContext::default(),
                    client_title: None,
                    flags: ClientFlags::default(),
                    render_stream: true,
                    uid,
                    user: UserIdentity::Uid(uid),
                    can_write: true,
                    client_size: Some(TerminalSize { cols: 80, rows: 24 }),
                },
            )
            .await
            .expect("attach registration succeeds");

        let pane_output = pane_output_channel();
        let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
        let target = AttachTarget {
            session_name: session_name.clone(),
            pane_master: None,
            pane_output,
            pane_output_start_sequence,
            render_frame: b"BASE".to_vec(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::default(),
            ),
            client_title: None,
            cursor_style: 0.into(),
            active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
        };
        let target_session_id = test_session_id(&handler, &session_name).await;

        handler
            .send_attach_control_for_session_identity(
                77,
                AttachControl::switch(target),
                "switch-client",
                session_name.clone(),
                target_session_id,
            )
            .await
            .expect("render-stream refresh substitution should be accepted");

        let refresh = control_rx.try_recv().expect("refresh control is queued");
        assert!(matches!(refresh, AttachControl::Refresh));
        crate::pane_io::release_attach_control_backlog(
            &control_backlog,
            refresh.received_backlog_units(),
        );
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
            crate::session_fixture::assert_new_session(
                &handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session_name.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
            );
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
            pane_master: None,
            pane_output,
            pane_output_start_sequence,
            render_frame: b"BETA".to_vec(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::default(),
            ),
            client_title: None,
            cursor_style: 0.into(),
            active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
        };
        let target_session_id = test_session_id(&handler, &beta).await;

        handler
            .send_attach_control_for_session_identity(
                77,
                AttachControl::switch(target),
                "switch-client",
                beta,
                target_session_id,
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

    #[tokio::test]
    async fn session_switch_fails_closed_when_target_name_is_recreated_after_capture() {
        let handler = RequestHandler::new();
        let alpha = SessionName::new("switch-identity-alpha").expect("valid session name");
        let beta = SessionName::new("switch-identity-beta").expect("valid session name");
        create_test_session(&handler, &alpha).await;
        create_test_session(&handler, &beta).await;

        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(91_337, alpha.clone(), control_tx)
            .await;
        let alpha_id = handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&91_337)
            .expect("attach exists")
            .session_id;

        let pane_output = pane_output_channel();
        let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
        let target = AttachTarget {
            session_name: beta.clone(),
            pane_master: None,
            pane_output,
            pane_output_start_sequence,
            render_frame: b"BETA".to_vec(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::default(),
            ),
            client_title: None,
            cursor_style: 0.into(),
            active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
        };
        let target_session_id = test_session_id(&handler, &beta).await;
        let pause = install_attach_control_identity_pause(91_337);
        let switch_handler = handler.clone();
        let switch_beta = beta.clone();
        let switch = tokio::spawn(async move {
            switch_handler
                .send_attach_control_for_session_identity(
                    91_337,
                    AttachControl::switch(target),
                    "switch-client",
                    switch_beta,
                    target_session_id,
                )
                .await
        });

        pause.reached.notified().await;
        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: beta.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_test_session(&handler, &beta).await;
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Err(rmux_proto::RmuxError::SessionNotFound(beta.to_string()))
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach.by_pid.get(&91_337).expect("attach survives");
        assert_eq!(
            (&active.session_name, active.session_id),
            (&alpha, alpha_id)
        );
    }

    #[tokio::test]
    async fn exact_detach_skips_same_attach_generation_after_session_switch() {
        let handler = RequestHandler::new();
        let alpha = SessionName::new("detach-current-alpha").expect("valid session name");
        let beta = SessionName::new("detach-current-beta").expect("valid session name");
        create_test_session(&handler, &alpha).await;
        create_test_session(&handler, &beta).await;

        let attach_pid = 91_341;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let attach_id = handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        let terminal_context = handler
            .terminal_context_for_attached_client(attach_pid)
            .await
            .expect("attach terminal context exists");
        let (alpha_id, beta_id, beta_target) = {
            let state = handler.state.lock().await;
            let alpha_id = state.sessions.session(&alpha).expect("alpha exists").id();
            let beta_id = state.sessions.session(&beta).expect("beta exists").id();
            let beta_target = attach_target_for_session(
                &state,
                &beta,
                AttachSessionRenderOptions {
                    attached_count: 1,
                    terminal_context: &terminal_context,
                    previous_title: None,
                    client: None,
                    socket_path: &handler.socket_path(),
                    client_loop_snapshots: None,
                },
            )
            .expect("beta target builds");
            (alpha_id, beta_id, beta_target)
        };

        let pause = install_attach_control_identity_pause(attach_pid);
        let detach_handler = handler.clone();
        let detach_alpha = alpha.clone();
        let detach = tokio::spawn(async move {
            detach_handler
                .detach_other_attach_clients_for_session_identity(
                    &detach_alpha,
                    alpha_id,
                    attach_pid + 1,
                    false,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
            .await
            .expect("detach reaches the final identity check");
        assert_eq!(
            handler
                .send_attach_control_for_client_and_session_identity(
                    attach_pid,
                    attach_id,
                    AttachControl::switch(beta_target),
                    beta.clone(),
                    beta_id,
                    None,
                )
                .await
                .expect("concurrent switch succeeds"),
            alpha
        );
        pause.release.notify_one();
        detach
            .await
            .expect("detach task joins")
            .expect("original session remains available");

        let mut saw_switch = false;
        while let Ok(control) = control_rx.try_recv() {
            match control {
                AttachControl::Switch(_) => saw_switch = true,
                AttachControl::Detach | AttachControl::DetachKill => {
                    panic!("stale detach must not reach the switched client")
                }
                _ => {}
            }
        }
        assert!(
            saw_switch,
            "the production switch control must be delivered"
        );
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("switched attach survives");
        assert_eq!(active.id, attach_id);
        assert_eq!((&active.session_name, active.session_id), (&beta, beta_id));
        assert!(!active.closing.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn session_switch_rejects_target_built_for_replaced_session_identity() {
        let handler = RequestHandler::new();
        let alpha = SessionName::new("switch-built-alpha").expect("valid session name");
        let beta = SessionName::new("switch-built-beta").expect("valid session name");
        create_test_session(&handler, &alpha).await;
        create_test_session(&handler, &beta).await;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(91_339, alpha.clone(), control_tx)
            .await;
        let terminal_context = handler
            .terminal_context_for_attached_client(91_339)
            .await
            .expect("attach terminal context exists");
        let (target, target_session_id) = {
            let state = handler.state.lock().await;
            let target_session_id = state.sessions.session(&beta).expect("old beta exists").id();
            let target = attach_target_for_session(
                &state,
                &beta,
                AttachSessionRenderOptions {
                    attached_count: 1,
                    terminal_context: &terminal_context,
                    previous_title: None,
                    client: None,
                    socket_path: &handler.socket_path(),
                    client_loop_snapshots: None,
                },
            )
            .expect("old beta target builds");
            (target, target_session_id)
        };

        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: beta.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_test_session(&handler, &beta).await;

        assert_eq!(
            handler
                .send_attach_control_for_session_identity(
                    91_339,
                    AttachControl::switch(target),
                    "switch-client",
                    beta.clone(),
                    target_session_id,
                )
                .await,
            Err(rmux_proto::RmuxError::SessionNotFound(beta.to_string()))
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .by_pid
                .get(&91_339)
                .map(|active| active.session_name.clone()),
            Some(alpha)
        );
    }

    #[test]
    fn session_switch_resets_stale_interactive_overlay_state() {
        let session_name = SessionName::new("alpha").expect("valid session name");
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let control_backlog = Arc::new(AtomicUsize::new(0));
        let closing = Arc::new(AtomicBool::new(false));
        let persistent_overlay_epoch = Arc::new(AtomicU64::new(7));
        let uid = current_owner_uid();
        let mut active = ActiveAttach {
            theme: crate::theme::ClientTheme::default(),
            pan: Default::default(),
            viewport: Default::default(),
            id: 1,
            client_name: "77".to_owned(),
            session_name,
            session_id: rmux_proto::SessionId::new(1),
            last_session: None,
            last_session_id: None,
            flags: ClientFlags::default(),
            control_tx: crate::pane_io::AttachControlSender::new(
                control_tx,
                Arc::clone(&control_backlog),
                ATTACH_CONTROL_BACKLOG_LIMIT,
                Arc::clone(&closing),
            ),
            control_backlog,
            _read_demand: crate::pane_io::PaneOutputReadDemandRegistry::default()
                .acquire_unrestricted(),
            render_stream: true,
            render_refresh_pending: false,
            uid,
            user: UserIdentity::Uid(uid),
            can_write: true,
            reported_focused: true,
            focus_origin: crate::handler::focus_support::AttachFocusOrigin::Native,
            session_scope: super::state::AttachSessionScope::Movable,
            clipboard_queries_desynchronized: false,
            suspended: false,
            closing,
            emit_detached_on_finish: false,
            detach_event_owned: false,
            terminal_context: OuterTerminalContext::default(),
            client_title: ClientTitleState::default(),
            client_size: TerminalSize { cols: 80, rows: 24 },
            client_size_provenance: AttachClientSizeProvenance::Declared,
            client_pixels: None,
            size_sequence: 0,
            last_activity_sequence: 0,
            activity_at: crate::client_activity::ClientActivityTimestamp::from_parts(0, 0),
            persistent_overlay_epoch: persistent_overlay_epoch.clone(),
            render_generation: 5,
            overlay_generation: 11,
            overlay_state_id: 13,
            display_panes_state_id: 17,
            key_table_name: None,
            key_table_set_at: None,
            key_table_generation: 0,
            repeat_deadline: None,
            repeat_active: false,
            last_key: None,
            held_keyboard: Default::default(),
            assumed_paste: Default::default(),
            idle_lock_timer: None,
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
            transient_message: None,
            transient_terminal_prefix: Vec::new(),
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

    #[tokio::test]
    async fn stale_session_cleanup_preserves_attach_for_recreated_identity() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("reused").expect("valid session name");
        create_test_session(&handler, &session_name).await;
        let old_session_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&session_name)
            .expect("session exists")
            .id();
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        let (new_tx, mut new_rx) = mpsc::unbounded_channel();
        let _old_attach = handler
            .register_attach(77, session_name.clone(), old_tx)
            .await;
        let _new_attach = handler
            .register_attach(88, session_name.clone(), new_tx)
            .await;
        let new_session_id = rmux_proto::SessionId::new(old_session_id.as_u32() + 1);
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get_mut(&88)
            .expect("new attach exists")
            .session_id = new_session_id;

        handler
            .exit_attached_session_identity(
                &session_name,
                old_session_id,
                SessionDetachOnDestroy::Detach,
            )
            .await;

        assert!(matches!(old_rx.try_recv(), Ok(AttachControl::Exited)));
        assert!(matches!(
            new_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        assert!(!active_attach.by_pid.contains_key(&77));
        assert_eq!(
            active_attach
                .by_pid
                .get(&88)
                .map(|active| active.session_id),
            Some(new_session_id)
        );
    }

    #[tokio::test]
    async fn finishing_stale_attach_does_not_destroy_recreated_unattached_session() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("attach-finish-reused").expect("valid session name");
        create_test_session(&handler, &session_name).await;
        let old_session_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&session_name)
            .expect("old session exists")
            .id();
        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: session_name.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_test_session(&handler, &session_name).await;
        let new_session_id = {
            let mut state = handler.state.lock().await;
            let session_id = state
                .sessions
                .session(&session_name)
                .expect("new session exists")
                .id();
            state
                .options
                .set(
                    ScopeSelector::Session(session_name.clone()),
                    OptionName::DestroyUnattached,
                    "on".to_owned(),
                    SetOptionMode::Replace,
                )
                .expect("destroy-unattached option is valid");
            session_id
        };
        assert_ne!(new_session_id, old_session_id);
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let attach_id = handler
            .register_attach(91_338, session_name.clone(), control_tx)
            .await;
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get_mut(&91_338)
            .expect("attach exists")
            .session_id = old_session_id;

        handler.finish_attach(91_338, attach_id).await;

        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&session_name)
                .map(|session| session.id()),
            Some(new_session_id)
        );
    }

    async fn create_test_session(handler: &RequestHandler, session_name: &SessionName) {
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);
    }

    async fn test_session_id(
        handler: &RequestHandler,
        session_name: &SessionName,
    ) -> rmux_proto::SessionId {
        handler
            .state
            .lock()
            .await
            .sessions
            .session(session_name)
            .expect("test session exists")
            .id()
    }
}
