use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rmux_proto::{
    AttachShellCommand, AttachedKeystroke, KeyDispatched, OptionName, PaneTarget, SessionId,
    SessionName, TerminalSize,
};
use tokio::time::sleep;

use super::{client_support::SwitchTargetSelection, RequestHandler};
use crate::handler_support::attached_client_required;
use crate::key_table::effective_client_key_table_name;
use crate::outer_terminal::{CursorScope, OuterTerminal, OuterTerminalContext};
use crate::pane_io::{AttachControl, AttachTarget, LivePaneRender, OverlayFrame};
use crate::pane_terminals::{session_not_found, HandlerState};
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

#[path = "handler_attach/key_table.rs"]
mod key_table;
#[path = "handler_attach/refresh.rs"]
mod refresh;
#[path = "handler_attach/refresh_identity.rs"]
mod refresh_identity;
#[path = "handler_attach/registration.rs"]
mod registration;
#[path = "handler_attach/resize_policy.rs"]
mod resize_policy;
#[path = "handler_attach/session_destroy.rs"]
mod session_destroy;
#[path = "handler_attach/state.rs"]
mod state;
#[path = "handler_attach/switch_commit.rs"]
mod switch_commit;

pub(crate) use crate::client_flags::ClientFlags;
pub(in crate::handler) use key_table::AttachedKeyTableCommit;
pub(in crate::handler) use resize_policy::{
    surviving_attached_resize_targets, AttachedWindowSizePolicy,
};
pub(in crate::handler) use session_destroy::{
    PreparedAttachedDestroySwitches, SessionDetachOnDestroy,
};
pub(super) use state::{
    ActiveAttach, ActiveAttachState, DisplayPanesClientState, DisplayPanesLabel,
};
pub(crate) use state::{ActiveAttachIdentity, AttachRegistration};
pub(in crate::handler) use switch_commit::{
    AttachedSwitchCommitOutcome, AttachedSwitchCommitRequest, AttachedSwitchCommittedTarget,
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
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn send_attach_control_for_session_identity(
        &self,
        attach_pid: u32,
        command: AttachControl,
        command_name: &str,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: rmux_proto::SessionId,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                next_session: Some((next_session_name, expected_session_id)),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn send_attach_control_for_client_and_session_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        command: AttachControl,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: rmux_proto::SessionId,
        target_selection: Option<SwitchTargetSelection>,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            "switch-client",
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                next_session: Some((next_session_name, expected_session_id)),
                target_selection,
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    pub(super) async fn send_attach_control_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        command: AttachControl,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    pub(in crate::handler) async fn send_attach_control_for_client_identity_from_mode_tree(
        &self,
        requester: super::mode_tree_support::ModeTreeActionIdentity,
        attach_pid: u32,
        expected_attach_id: u64,
        command: AttachControl,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                mode_tree_requester: Some(requester),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    pub(super) async fn send_attach_control_for_client_current_session_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        expected_current_session_id: rmux_proto::SessionId,
        command: AttachControl,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable target session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                current_session_id: Some(expected_current_session_id),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    async fn send_attach_control_with_expected_identity(
        &self,
        attach_pid: u32,
        command: AttachControl,
        command_name: &str,
        identity: AttachControlIdentityExpectation,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        let expected_attach_id = identity.attach_id;
        let expected_current_session_id = identity.current_session_id;
        let target_selection = identity.target_selection;
        let mode_tree_requester = identity.mode_tree_requester;
        let (next_session_name, expected_next_session_id) = identity
            .next_session
            .map_or((None, None), |(name, session_id)| {
                (Some(name), Some(session_id))
            });
        let is_switch = matches!(command, AttachControl::Switch(_));
        let next_session_id = if let Some(session_name) = next_session_name.as_ref() {
            let state = self.state.lock().await;
            let session_id = state
                .sessions
                .session(session_name)
                .ok_or_else(|| rmux_proto::RmuxError::SessionNotFound(session_name.to_string()))?
                .id();
            if expected_next_session_id.is_some_and(|expected| expected != session_id) {
                return Err(rmux_proto::RmuxError::SessionNotFound(
                    session_name.to_string(),
                ));
            }
            Some(session_id)
        } else {
            None
        };
        #[cfg(test)]
        pause_after_attach_control_identity_capture(attach_pid).await;
        let switch_changes_session = if is_switch {
            self.attach_switch_changes_session(
                attach_pid,
                expected_attach_id,
                next_session_name.as_ref(),
                next_session_id,
                command_name,
            )
            .await?
        } else {
            false
        };
        let mode_tree_refresh_sessions = if switch_changes_session {
            match expected_attach_id {
                Some(attach_id) => {
                    self.dismiss_mode_tree_for_client_identity(attach_pid, attach_id)
                        .await?
                }
                None => self.dismiss_mode_tree(attach_pid).await?,
            }
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
        // Keep the model generation stable until the attached client has been
        // updated. Otherwise a kill/recreate of the same session name between
        // target construction and this assignment can bind the client to a
        // stale target while recording the replacement session's name.
        let mut state = self.state.lock().await;
        if let (Some(session_name), Some(expected_session_id)) =
            (next_session_name.as_ref(), next_session_id)
        {
            let current_session_id = state
                .sessions
                .session(session_name)
                .map(rmux_core::Session::id);
            if current_session_id != Some(expected_session_id) {
                return Err(rmux_proto::RmuxError::SessionNotFound(
                    session_name.to_string(),
                ));
            }
        }
        let mut active_attach = self.active_attach.lock().await;
        if mode_tree_requester
            .is_some_and(|requester| !requester.matches_active(&state, &active_attach))
        {
            return Err(attached_client_required(command_name));
        }
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(attached_client_required(command_name));
        };
        if expected_attach_id.is_some_and(|expected| active.id != expected) {
            return Err(attached_client_required(command_name));
        }
        if active.closing.load(Ordering::SeqCst) {
            return Err(attached_client_required(command_name));
        }
        if expected_current_session_id.is_some_and(|expected| active.session_id != expected) {
            return Err(attached_client_required(command_name));
        }
        if let Some(selection) = target_selection.as_ref() {
            if !is_switch {
                return Err(rmux_proto::RmuxError::Server(
                    "target selection requires a session switch".to_owned(),
                ));
            }
            let session_name = next_session_name
                .as_ref()
                .expect("a switch target selection carries a session");
            let session_id = next_session_id
                .expect("a switch target selection carries a stable session identity");
            selection.validate_for_session_identity(&state, session_name, session_id)?;
        }
        let previous_session_name = active.session_name.clone();
        let mut overlay_to_terminate = None;

        let switches_session_identity = next_session_name.as_ref().is_some_and(|session_name| {
            session_name != &active.session_name
                || next_session_id.is_some_and(|session_id| session_id != active.session_id)
        });
        if is_switch && switches_session_identity {
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
                if switches_session_identity {
                    active.last_session = Some(active.session_name.clone());
                    active.last_session_id = Some(active.session_id);
                }
                active.session_name = session_name;
                active.session_id = next_session_id
                    .expect("a session switch target must carry its stable identity");
            }
            if !active.render_refresh_pending {
                active.render_refresh_pending = true;
                if let Err(error) = active.control_tx.send(AttachControl::Refresh) {
                    if error.is_full() {
                        active.closing.store(true, Ordering::SeqCst);
                    }
                    active_attach.remove_attached_client(attach_pid);
                    self.bump_active_attach_epoch();
                    return if error.is_full() {
                        Err(rmux_proto::RmuxError::Server(
                            "attached client is not draining updates".to_owned(),
                        ))
                    } else {
                        Err(attached_client_required(command_name))
                    };
                }
            }
            if let Some(selection) = target_selection.as_ref() {
                selection
                    .apply_to_state(&mut state)
                    .expect("prevalidated switch selection remains applicable while locked");
            }
            if expected_attach_id.is_some() {
                state
                    .sessions
                    .session_mut(&active.session_name)
                    .expect("switch target stayed locked across the attached client update")
                    .touch_attached();
            }
            drop(active_attach);
            drop(state);
            if clear_prompt {
                match expected_attach_id {
                    Some(attach_id) => {
                        self.clear_prompt_for_attach_identity(attach_pid, attach_id)
                            .await;
                    }
                    None => self.clear_prompt_for_attach(attach_pid).await,
                }
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
        if let Err(error) = active.control_tx.send(command) {
            if error.is_full() {
                active.closing.store(true, Ordering::SeqCst);
            }
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return if error.is_full() {
                Err(rmux_proto::RmuxError::Server(
                    "attached client is not draining updates".to_owned(),
                ))
            } else {
                Err(attached_client_required(command_name))
            };
        }
        if let Some(selection) = target_selection.as_ref() {
            selection
                .apply_to_state(&mut state)
                .expect("prevalidated switch selection remains applicable while locked");
        }
        if closing_control {
            active.closing.store(true, Ordering::SeqCst);
        }
        if let Some(session_name) = next_session_name {
            if switches_session_identity {
                active.last_session = Some(active.session_name.clone());
                active.last_session_id = Some(active.session_id);
            }
            active.session_name = session_name;
            active.session_id =
                next_session_id.expect("a session switch target must carry its stable identity");
        }
        if is_switch && expected_attach_id.is_some() {
            state
                .sessions
                .session_mut(&active.session_name)
                .expect("switch target stayed locked across the attached client update")
                .touch_attached();
        }
        drop(active_attach);
        drop(state);

        if clear_prompt {
            match expected_attach_id {
                Some(attach_id) => {
                    self.clear_prompt_for_attach_identity(attach_pid, attach_id)
                        .await;
                }
                None => self.clear_prompt_for_attach(attach_pid).await,
            }
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
        expected_attach_id: Option<u64>,
        next_session_name: Option<&rmux_proto::SessionName>,
        next_session_id: Option<rmux_proto::SessionId>,
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
        if expected_attach_id.is_some_and(|expected| active.id != expected) {
            return Err(attached_client_required(command_name));
        }
        Ok(next_session_name != &active.session_name
            || next_session_id.is_some_and(|session_id| session_id != active.session_id))
    }

    async fn close_attached_session<F>(&self, session_id: rmux_proto::SessionId, mut control: F)
    where
        F: FnMut() -> AttachControl,
    {
        let mut overlay_jobs = Vec::new();
        let mut removed_pids = Vec::new();
        let mut active_attach = self.active_attach.lock().await;
        for active in active_attach.by_pid.values_mut() {
            if active.last_session_id == Some(session_id) {
                active.last_session = None;
                active.last_session_id = None;
            }
        }
        active_attach.by_pid.retain(|pid, active| {
            if active.session_id != session_id {
                return true;
            }

            overlay_jobs.push(active.overlay.take());
            removed_pids.push(*pid);
            let _ = active.control_tx.send(control());
            active.closing.store(true, Ordering::SeqCst);
            false
        });
        for pid in removed_pids {
            active_attach.forget_attached_client_windows(pid);
        }
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
        self.send_attached_overlay_to_client_guarded(
            attach_pid,
            None,
            None,
            overlay_frame,
            clear_frame,
            duration,
        )
        .await
    }

    pub(super) async fn send_attached_overlay_to_client_identity(
        &self,
        identity: ActiveAttachIdentity,
        expected_session_id: Option<SessionId>,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
        duration: Duration,
    ) -> bool {
        self.send_attached_overlay_to_client_guarded(
            identity.attach_pid(),
            Some(identity),
            expected_session_id,
            overlay_frame,
            clear_frame,
            duration,
        )
        .await
    }

    async fn send_attached_overlay_to_client_guarded(
        &self,
        attach_pid: u32,
        expected_identity: Option<ActiveAttachIdentity>,
        expected_session_id: Option<SessionId>,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
        duration: Duration,
    ) -> bool {
        let handler = self.clone();
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return false;
        };
        if active.suspended
            || expected_identity.is_some_and(|identity| !identity.matches_active(active))
            || expected_session_id.is_some_and(|session_id| active.session_id != session_id)
        {
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
    active.mouse.reset_for_session_switch();
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
            selection: None,
            window_size_override: None,
            master: AttachTargetMaster::Clone,
            socket_path,
        },
    )
}

pub(super) struct AttachSessionSwitchRenderOptions<'a> {
    pub(super) attached_count: usize,
    pub(super) terminal_context: &'a OuterTerminalContext,
    pub(super) socket_path: &'a Path,
    pub(super) render_stream: bool,
    pub(super) selection: Option<&'a SwitchTargetSelection>,
    pub(super) window_size_override: Option<(u32, TerminalSize)>,
}

pub(super) fn attach_target_for_session_switch(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    options: AttachSessionSwitchRenderOptions<'_>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    let AttachSessionSwitchRenderOptions {
        attached_count,
        terminal_context,
        socket_path,
        render_stream,
        selection,
        window_size_override,
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
            key_table: None,
            terminal_context,
            render_size: None,
            window_index: None,
            selection,
            window_size_override,
            master,
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
            selection: None,
            window_size_override: None,
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
            selection: None,
            window_size_override: None,
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
    selection: Option<&'a SwitchTargetSelection>,
    window_size_override: Option<(u32, TerminalSize)>,
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
    let session = attach_render_session(
        canonical_session,
        options.render_size,
        options.window_index,
        options.selection,
        options.window_size_override,
    )?;
    let session = session.as_ref();
    let key_table = effective_client_key_table_name(state, session, options.key_table);
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
    // A kept-dead pane retains its transcript for capture and rendering, but
    // no live application remains to consume outer mouse reports. Do not let
    // stale DECSET 1000/1002/1003 bits from that transcript keep the client's
    // terminal in mouse-tracking mode after remain-on-exit refreshes it.
    let active_pane_mouse_mode = active_pane.as_ref().map_or(0, |pane| {
        if state.pane_is_dead(session_name, pane.id()) {
            0
        } else {
            pane_state.as_ref().map_or(0, |pane_state| pane_state.mode)
        }
    });
    let outer_terminal = OuterTerminal::resolve_for_session(
        &state.options,
        Some(session_name),
        options.terminal_context.clone(),
    )
    .with_active_pane_mouse_mode(active_pane_mouse_mode);
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
                key_table: Some(&key_table),
                socket_path: Some(options.socket_path),
            },
        )
        .as_slice(),
    );
    for pane in session.window().panes() {
        let copy_snapshot = state.pane_copy_mode_render_snapshot(session_name, pane.id());
        if let Some(snapshot) = copy_snapshot.as_ref() {
            let pane_frame = if options.prompt.is_some() {
                renderer::render_copy_mode_pane_screen_preserving_prompt_cursor(
                    session,
                    &state.options,
                    pane,
                    snapshot,
                )
            } else {
                renderer::render_copy_mode_pane_screen(session, &state.options, pane, snapshot)
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
        if pane.index() == session.active_pane_index() && copy_snapshot.is_some() {
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
                        copy_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.alternate_on),
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
            if let Some(snapshot) =
                state.pane_copy_mode_render_snapshot(session_name, active_pane.id())
            {
                render_frame.extend_from_slice(
                    renderer::render_copy_mode_pane_cursor(
                        session,
                        &state.options,
                        &active_pane,
                        &snapshot,
                    )
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
            renderer::visible_pane_terminal_geometry(
                session,
                &state.options,
                pane,
                pane_state.as_ref().is_some_and(|state| state.alternate_on),
                state
                    .pane_copy_mode_summary(session_name, pane.id())
                    .is_some(),
            )
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

    Ok(AttachTarget {
        session_name: session_name.clone(),
        pane_master: match options.master {
            AttachTargetMaster::Clone if !active_pane_is_starting => {
                if options.selection.is_some() {
                    Some(state.clone_pane_master(
                        session_name,
                        session.active_window_index(),
                        session.active_pane_index(),
                    )?)
                } else {
                    Some(state.active_pane_master(session_name)?)
                }
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

fn attach_render_session<'a>(
    session: &'a rmux_core::Session,
    size: Option<TerminalSize>,
    window_index: Option<u32>,
    selection: Option<&SwitchTargetSelection>,
    window_size_override: Option<(u32, TerminalSize)>,
) -> Result<Cow<'a, rmux_core::Session>, rmux_proto::RmuxError> {
    let mut rendered = sized_session(session, size);
    if let Some((window_index, selected_size)) = window_size_override {
        rendered
            .to_mut()
            .resize_window(window_index, selected_size)?;
    }
    if let Some(selection) = selection {
        if selection.session_name() != session.name() {
            return Err(rmux_proto::RmuxError::Server(
                "switch render selection changed sessions before commit".to_owned(),
            ));
        }
        selection.apply_to_session(rendered.to_mut())?;
    } else if let Some(window_index) = window_index {
        if rendered.active_window_index() != window_index
            && rendered.windows().contains_key(&window_index)
        {
            rendered
                .to_mut()
                .select_window(window_index)
                .expect("selected web render window was validated above");
        }
    }

    if let Some((window_index, _)) = window_size_override {
        if rendered.active_window_index() == window_index {
            let active_size = rendered.window().size();
            rendered.to_mut().resize_active_window_terminal(active_size);
        }
    }

    Ok(rendered)
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
        reset_interactive_attach_state_for_session_switch, ActiveAttach, AttachRegistration,
        RequestHandler, SessionDetachOnDestroy, ATTACH_CONTROL_BACKLOG_LIMIT,
    };
    use crate::client_flags::ClientFlags;
    use crate::handler::scripting_support::QueueExecutionContext;
    use crate::mouse::ClientMouseState;
    use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
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
            cursor_style: 0,
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
            cursor_style: 0,
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
                1,
                &terminal_context,
                &handler.socket_path(),
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
                1,
                &terminal_context,
                &handler.socket_path(),
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
            id: 1,
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
            render_stream: true,
            render_refresh_pending: false,
            uid,
            user: UserIdentity::Uid(uid),
            can_write: true,
            clipboard_queries_desynchronized: false,
            suspended: false,
            closing,
            emit_detached_on_finish: false,
            terminal_context: OuterTerminalContext::default(),
            client_size: TerminalSize { cols: 80, rows: 24 },
            client_pixels: None,
            size_sequence: 0,
            last_activity_sequence: 0,
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
                .map(rmux_core::Session::id),
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
        assert!(matches!(response, Response::NewSession(_)));
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
