use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Instant;

use rmux_core::KeyCode;
use rmux_os::identity::UserIdentity;
use rmux_proto::{PaneTarget, TerminalPixels, TerminalSize, WindowTarget};
use tokio::sync::mpsc;

use super::super::mode_tree_support::ModeTreeClientState;
use super::super::overlay_support::ClientOverlayState;
use super::super::prompt_support::ClientPromptState;
use crate::client_flags::ClientFlags;
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
use crate::mouse::ClientMouseState;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;

#[derive(Debug, Default)]
pub(in crate::handler) struct ActiveAttachState {
    pub(in crate::handler) next_id: u64,
    pub(in crate::handler) by_pid: HashMap<u32, ActiveAttach>,
}

#[derive(Debug)]
pub(in crate::handler) struct ActiveAttach {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) session_name: rmux_proto::SessionName,
    pub(in crate::handler) last_session: Option<rmux_proto::SessionName>,
    pub(in crate::handler) flags: ClientFlags,
    pub(in crate::handler) pan_window: Option<u32>,
    pub(in crate::handler) pan_ox: u32,
    pub(in crate::handler) pan_oy: u32,
    pub(in crate::handler) control_tx: mpsc::UnboundedSender<AttachControl>,
    pub(in crate::handler) uid: u32,
    pub(in crate::handler) user: UserIdentity,
    pub(in crate::handler) can_write: bool,
    pub(in crate::handler) suspended: bool,
    pub(in crate::handler) closing: Arc<AtomicBool>,
    pub(in crate::handler) terminal_context: OuterTerminalContext,
    pub(in crate::handler) client_size: TerminalSize,
    pub(in crate::handler) client_pixels: Option<TerminalPixels>,
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
    /// Count of terminal queries (DA1/DA2/DSR/etc.) currently in flight
    /// from the active pane out to the client.  Incremented when the
    /// forwarder spots a query in pane→client bytes; decremented (and
    /// the reply is forwarded) when a matching response arrives on
    /// client→pane.  When a response arrives with a 0 count it's an
    /// orphan — typically vim asked DA1, exited before the reply
    /// round-tripped back across SSH, and the reply now lands at the
    /// shell as visible garbage like `^[[?65;4;6;18;22c`.  We drop
    /// orphans rather than forward them.
    pub(in crate::handler) outstanding_terminal_queries: u32,
    pub(in crate::handler) mode_tree_state_id: u64,
    pub(in crate::handler) mode_tree: Option<ModeTreeClientState>,
    pub(in crate::handler) mode_tree_frame: Option<Vec<u8>>,
    pub(in crate::handler) overlay: Option<ClientOverlayState>,
    pub(in crate::handler) display_panes: Option<DisplayPanesClientState>,
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
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(crate) terminal_context: OuterTerminalContext,
    pub(crate) flags: ClientFlags,
    pub(crate) uid: u32,
    pub(crate) user: UserIdentity,
    pub(crate) can_write: bool,
    pub(crate) client_size: Option<TerminalSize>,
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
        new_name: &rmux_proto::SessionName,
    ) {
        for active in self.by_pid.values_mut() {
            if &active.session_name == session_name {
                active.session_name = new_name.clone();
            }
            if active.last_session.as_ref() == Some(session_name) {
                active.last_session = Some(new_name.clone());
            }
        }
    }

    pub(in crate::handler) fn toggle_read_only(
        &mut self,
        attach_pid: u32,
    ) -> Result<ClientFlags, rmux_proto::RmuxError> {
        let active = self.by_pid.get_mut(&attach_pid).ok_or_else(|| {
            rmux_proto::RmuxError::Server("attached client disappeared".to_owned())
        })?;
        active.flags.toggle_read_only();
        Ok(active.flags)
    }

    pub(in crate::handler) fn last_session_for_client(
        &self,
        attach_pid: u32,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.by_pid
            .get(&attach_pid)
            .map(|active| active.last_session.clone())
            .ok_or_else(|| rmux_proto::RmuxError::Server("attached client disappeared".to_owned()))
    }

    pub(in crate::handler) fn attached_client_pids_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
        except_pid: Option<u32>,
    ) -> Vec<u32> {
        let mut pids = self
            .by_pid
            .iter()
            .filter_map(|(pid, active)| {
                (&active.session_name == session_name && except_pid != Some(*pid)).then_some(*pid)
            })
            .collect::<Vec<_>>();
        pids.sort_unstable();
        pids
    }

    pub(in crate::handler) fn attached_client_pids_except(&self, except_pid: u32) -> Vec<u32> {
        let mut pids = self
            .by_pid
            .keys()
            .copied()
            .filter(|pid| *pid != except_pid)
            .collect::<Vec<_>>();
        pids.sort_unstable();
        pids
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
