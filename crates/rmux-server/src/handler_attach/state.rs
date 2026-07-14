use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::time::Instant;

use rmux_core::KeyCode;
use rmux_os::identity::UserIdentity;
use rmux_proto::{PaneTarget, SessionId, TerminalPixels, TerminalSize, WindowTarget};
use tokio::sync::mpsc;

use super::super::mode_tree_support::ModeTreeClientState;
use super::super::overlay_support::ClientOverlayState;
use super::super::prompt_support::ClientPromptState;
use super::super::scripting_support::{rename_pane_target_session, rename_window_target_session};
use crate::client_flags::ClientFlags;
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
use crate::mouse::ClientMouseState;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;

#[derive(Debug, Default)]
pub(in crate::handler) struct ActiveAttachState {
    pub(in crate::handler) next_id: u64,
    pub(in crate::handler) next_size_sequence: u64,
    pub(in crate::handler) by_pid: HashMap<u32, ActiveAttach>,
    pub(in crate::handler) active_client_by_window:
        HashMap<rmux_proto::SessionName, HashMap<u32, u32>>,
}

#[derive(Debug)]
pub(in crate::handler) struct ActiveAttach {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) session_name: rmux_proto::SessionName,
    pub(in crate::handler) session_id: SessionId,
    pub(in crate::handler) last_session: Option<rmux_proto::SessionName>,
    pub(in crate::handler) last_session_id: Option<SessionId>,
    pub(in crate::handler) flags: ClientFlags,
    pub(in crate::handler) pan_window: Option<u32>,
    pub(in crate::handler) pan_ox: u32,
    pub(in crate::handler) pan_oy: u32,
    pub(in crate::handler) control_tx: mpsc::UnboundedSender<AttachControl>,
    pub(in crate::handler) control_backlog: Arc<AtomicUsize>,
    pub(in crate::handler) render_stream: bool,
    pub(in crate::handler) render_refresh_pending: bool,
    pub(in crate::handler) uid: u32,
    pub(in crate::handler) user: UserIdentity,
    pub(in crate::handler) can_write: bool,
    pub(in crate::handler) suspended: bool,
    pub(in crate::handler) closing: Arc<AtomicBool>,
    pub(in crate::handler) terminal_context: OuterTerminalContext,
    pub(in crate::handler) client_size: TerminalSize,
    pub(in crate::handler) client_pixels: Option<TerminalPixels>,
    pub(in crate::handler) size_sequence: u64,
    pub(in crate::handler) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(in crate::handler) render_generation: u64,
    pub(in crate::handler) overlay_generation: u64,
    pub(in crate::handler) overlay_state_id: u64,
    pub(in crate::handler) display_panes_state_id: u64,
    pub(in crate::handler) key_table_name: Option<String>,
    pub(in crate::handler) key_table_set_at: Option<Instant>,
    pub(in crate::handler) repeat_deadline: Option<Instant>,
    pub(in crate::handler) repeat_active: bool,
    pub(in crate::handler) last_key: Option<KeyCode>,
    pub(in crate::handler) mouse: ClientMouseState,
    pub(in crate::handler) prompt: Option<ClientPromptState>,
    pub(in crate::handler) mode_tree_state_id: u64,
    pub(in crate::handler) mode_tree: Option<ModeTreeClientState>,
    pub(in crate::handler) mode_tree_frame: Option<Vec<u8>>,
    pub(in crate::handler) overlay: Option<ClientOverlayState>,
    pub(in crate::handler) display_panes: Option<DisplayPanesClientState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveAttachIdentity {
    attach_pid: u32,
    attach_id: u64,
    session_id: SessionId,
}

impl ActiveAttachIdentity {
    pub(crate) const fn new(attach_pid: u32, attach_id: u64, session_id: SessionId) -> Self {
        Self {
            attach_pid,
            attach_id,
            session_id,
        }
    }

    pub(crate) const fn attach_pid(self) -> u32 {
        self.attach_pid
    }

    pub(crate) const fn attach_id(self) -> u64 {
        self.attach_id
    }

    pub(crate) const fn session_id(self) -> SessionId {
        self.session_id
    }

    pub(in crate::handler) fn matches_active(self, active: &ActiveAttach) -> bool {
        // A live attach may legitimately switch sessions without changing its
        // forwarder.  The registration id, not the current session id, owns
        // the socket lifetime.
        self.attach_id == active.id
    }

    pub(in crate::handler) fn matches_active_session(
        self,
        active: &ActiveAttach,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
    ) -> bool {
        self.attach_id == active.id
            && active.session_id == session_id
            && &active.session_name == session_name
    }

    pub(in crate::handler) fn matches(
        self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
        active: &ActiveAttach,
    ) -> bool {
        self.attach_pid == attach_pid
            && self.attach_id == active.id
            && self.session_id == active.session_id
            && &active.session_name == session_name
    }
}

impl ActiveAttach {
    pub(in crate::handler) const fn identity(&self, attach_pid: u32) -> ActiveAttachIdentity {
        ActiveAttachIdentity::new(attach_pid, self.id, self.session_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct DisplayPanesClientState {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) window: WindowTarget,
    pub(in crate::handler) labels: Vec<DisplayPanesLabel>,
    pub(in crate::handler) input: String,
    pub(in crate::handler) template: Option<String>,
    pub(in crate::handler) clear_frame: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct DisplayPanesLabel {
    pub(in crate::handler) label: String,
    pub(in crate::handler) target: PaneTarget,
    pub(in crate::handler) target_string: String,
}

#[derive(Debug)]
pub(crate) struct AttachRegistration {
    pub(crate) control_tx: mpsc::UnboundedSender<AttachControl>,
    pub(crate) control_backlog: Arc<AtomicUsize>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(crate) terminal_context: OuterTerminalContext,
    pub(crate) flags: ClientFlags,
    pub(crate) render_stream: bool,
    pub(crate) uid: u32,
    pub(crate) user: UserIdentity,
    pub(crate) can_write: bool,
    pub(crate) client_size: Option<TerminalSize>,
}

fn rename_display_panes_state(
    state: &mut DisplayPanesClientState,
    old_name: &rmux_proto::SessionName,
    new_name: &rmux_proto::SessionName,
) -> bool {
    let mut renamed = state.clone();
    rename_window_target_session(&mut renamed.window, old_name, new_name);
    let old_prefix = format!("={old_name}:");
    for label in &mut renamed.labels {
        if label.target.session_name() != old_name {
            continue;
        }
        let Some(suffix) = label.target_string.strip_prefix(&old_prefix) else {
            return false;
        };
        rename_pane_target_session(&mut label.target, old_name, new_name);
        label.target_string = format!("={new_name}:{suffix}");
    }
    *state = renamed;
    true
}

impl ActiveAttachState {
    pub(in crate::handler) fn attached_count(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> usize {
        self.by_pid
            .values()
            .filter(|active| &active.session_name == session_name && !active.suspended)
            .count()
    }

    pub(in crate::handler) fn rename_session(
        &mut self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        new_name: &rmux_proto::SessionName,
    ) {
        for active in self.by_pid.values_mut() {
            if &active.session_name == session_name && active.session_id == session_id {
                active.session_name = new_name.clone();
                if let Some(mode_tree) = active.mode_tree.as_mut() {
                    mode_tree.rename_session(session_name, new_name);
                }
                if active.display_panes.as_mut().is_some_and(|display_panes| {
                    !rename_display_panes_state(display_panes, session_name, new_name)
                }) {
                    active.display_panes = None;
                    active.display_panes_state_id = active.display_panes_state_id.saturating_add(1);
                }
            }
            if active.last_session.as_ref() == Some(session_name)
                && active.last_session_id == Some(session_id)
            {
                active.last_session = Some(new_name.clone());
            }
            active
                .mouse
                .rename_session_targets(session_name, session_id, new_name);
            if let Some(prompt) = active.prompt.as_mut() {
                prompt.rename_session_targets(session_name, new_name);
            }
            if let Some(overlay) = active.overlay.as_mut() {
                overlay.rename_session_targets(session_name, new_name);
            }
        }
        if let Some(renamed_windows) = self.active_client_by_window.remove(session_name) {
            self.active_client_by_window
                .entry(new_name.clone())
                .or_default()
                .extend(renamed_windows);
        }
    }

    pub(in crate::handler) fn remove_attached_client(
        &mut self,
        attach_pid: u32,
    ) -> Option<ActiveAttach> {
        let active = self.by_pid.remove(&attach_pid)?;
        self.forget_attached_client_windows(attach_pid);
        Some(active)
    }

    pub(in crate::handler) fn forget_attached_client_windows(&mut self, attach_pid: u32) {
        self.active_client_by_window.retain(|_, windows| {
            windows.retain(|_, pid| *pid != attach_pid);
            !windows.is_empty()
        });
    }

    pub(in crate::handler) fn forget_window(&mut self, target: &WindowTarget) {
        let remove_session = self
            .active_client_by_window
            .get_mut(target.session_name())
            .is_some_and(|windows| {
                let _ = windows.remove(&target.window_index());
                windows.is_empty()
            });
        if remove_session {
            let _ = self.active_client_by_window.remove(target.session_name());
        }
    }

    pub(in crate::handler) fn seed_active_client_for_window(
        &mut self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
        window_index: u32,
    ) {
        if self.by_pid.contains_key(&attach_pid) {
            self.active_client_by_window
                .entry(session_name.clone())
                .or_default()
                .insert(window_index, attach_pid);
        }
    }

    pub(in crate::handler) fn record_active_client_for_window(
        &mut self,
        attach_pid: u32,
        target: &PaneTarget,
    ) -> bool {
        if self
            .active_client_by_window
            .get(target.session_name())
            .and_then(|windows| windows.get(&target.window_index()))
            == Some(&attach_pid)
        {
            return false;
        }

        match self
            .active_client_by_window
            .entry(target.session_name().clone())
            .or_default()
            .insert(target.window_index(), attach_pid)
        {
            Some(previous) => previous != attach_pid,
            None => false,
        }
    }

    pub(in crate::handler) fn toggle_read_only_for_identity(
        &mut self,
        attach_pid: u32,
        expected_attach_id: u64,
    ) -> Result<ClientFlags, rmux_proto::RmuxError> {
        let active = self
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| active.id == expected_attach_id)
            .ok_or_else(|| {
                rmux_proto::RmuxError::Server("attached client disappeared".to_owned())
            })?;
        active.flags.toggle_read_only();
        Ok(active.flags)
    }

    #[cfg(test)]
    pub(in crate::handler) fn last_session_for_client(
        &self,
        attach_pid: u32,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.last_session_identity_for_client(attach_pid)
            .map(|identity| identity.map(|(session_name, _)| session_name))
    }

    #[cfg(test)]
    pub(in crate::handler) fn last_session_identity_for_client(
        &self,
        attach_pid: u32,
    ) -> Result<Option<(rmux_proto::SessionName, SessionId)>, rmux_proto::RmuxError> {
        self.by_pid
            .get(&attach_pid)
            .map(|active| active.last_session.clone().zip(active.last_session_id))
            .ok_or_else(|| rmux_proto::RmuxError::Server("attached client disappeared".to_owned()))
    }

    pub(in crate::handler) fn session_for_attached_client(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        if self.by_pid.is_empty() {
            return Ok(None);
        }

        let attach_pid = self.resolve_attached_client_pid(requester_pid, command_name)?;
        Ok(self
            .by_pid
            .get(&attach_pid)
            .map(|active| active.session_name.clone()))
    }

    pub(in crate::handler) fn current_session_candidate(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        if let Some(active) = self.by_pid.get(&requester_pid) {
            return Some(active.session_name.clone());
        }

        if self.by_pid.len() == 1 {
            return self
                .by_pid
                .values()
                .next()
                .map(|active| active.session_name.clone());
        }

        None
    }

    pub(in crate::handler) fn resolve_attached_client_pid(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<u32, rmux_proto::RmuxError> {
        if self.by_pid.contains_key(&requester_pid) {
            return Ok(requester_pid);
        }

        match self.by_pid.len() {
            0 => Err(attached_client_required(command_name)),
            1 => Ok(*self
                .by_pid
                .keys()
                .next()
                .expect("single-entry attach map must have one key")),
            _ => Err(ambiguous_attached_client(command_name)),
        }
    }
}
