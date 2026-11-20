use crate::client_activity::ClientActivityTimestamp;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::time::Instant;

use rmux_core::KeyCode;
use rmux_os::identity::UserIdentity;
use rmux_proto::{PaneTarget, SessionId, TerminalPixels, TerminalSize, WindowTarget};
use tokio::sync::mpsc;

use super::super::focus_support::{
    AttachFocusOrigin, FocusTransportOrder, FocusedPaneState, PaneFocusRuntimeIdentity,
};
use super::super::mode_tree_support::ModeTreeClientState;
use super::super::overlay_support::ClientOverlayState;
use super::super::prompt_support::ClientPromptState;
use super::super::scripting_support::{
    rename_pane_target_session, rename_window_target_session, QueueExecutionContext,
};
use super::super::{current_client_activity_timestamp, RequesterOrigin, StableTargetIdentity};
use super::input_path::AttachedInputWaits;
use super::transient_message::TransientMessageInputState;
use crate::client_flags::ClientFlags;
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
use crate::mouse::ClientMouseState;
use crate::outer_terminal::{ClientTitleState, OuterTerminalContext, RenderedClientTitle};
use crate::pane_io::{AttachControl, AttachControlSender, PaneOutputReadDemandGuard};

#[derive(Debug, Default)]
pub(in crate::handler) struct ActiveAttachState {
    pub(in crate::handler) theme_publication: Arc<tokio::sync::Mutex<()>>,
    pub(in crate::handler) by_pid: HashMap<u32, ActiveAttach>,
    pub(in crate::handler) active_client_by_window:
        HashMap<rmux_proto::SessionName, HashMap<u32, u32>>,
    /// Runtime incarnations currently receiving at least one native attach's
    /// focus contribution. The value is a stable visible target for hooks;
    /// aliases of the same runtime share one key and therefore one edge.
    pub(in crate::handler) focused_panes: HashMap<PaneFocusRuntimeIdentity, FocusedPaneState>,
    /// Orders focus bytes by the state commit that derived them. Hook
    /// execution is deliberately outside this transport turn.
    pub(in crate::handler) focus_transport_order: FocusTransportOrder,
    /// Bindings that attached clients' input paths are awaiting, until a popup releases them.
    pub(in crate::handler) attached_input_waits: AttachedInputWaits,
}

/// How an attached client's outer terminal geometry became known.
///
/// This is server-internal typing only; it never crosses the wire and must
/// never be reconstructed from the numeric dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum AttachClientSizeProvenance {
    /// The client declared this outer terminal geometry itself.
    Declared,
    /// The client declared no size, so registration anchored it to the outer
    /// terminal geometry of the session it joined. Anchoring to the session's
    /// *content* geometry instead would hand an already status-subtracted row
    /// count to every consumer that subtracts the status rows again.
    InferredFromSession,
}

/// Which sessions an attach may occupy after registration.
///
/// This is server-internal typing only; it is fixed by the registration path
/// and never inferred from a synthetic pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum AttachSessionScope {
    /// A terminal client: `switch-client`, choose-tree and `detach-on-destroy`
    /// may move it onto another session.
    Movable,
    /// A Web session-share view exposes only the session its share names. It
    /// never moves onto another session and is closed with its own.
    #[cfg(all(any(unix, windows), feature = "web"))]
    SharedSessionOnly,
}

#[derive(Debug)]
pub(in crate::handler) struct ActiveAttach {
    pub(in crate::handler) theme: crate::theme::ClientTheme,
    pub(in crate::handler) pan: crate::client_viewport::ClientPan,
    pub(in crate::handler) viewport: crate::client_viewport::ClientViewport,
    pub(in crate::handler) id: u64,
    /// Canonical display identity captured while the attaching process still
    /// exposes its tty. Never reconstruct this from the pid after registration.
    pub(in crate::handler) client_name: String,
    pub(in crate::handler) session_name: rmux_proto::SessionName,
    pub(in crate::handler) session_id: SessionId,
    pub(in crate::handler) last_session: Option<rmux_proto::SessionName>,
    pub(in crate::handler) last_session_id: Option<SessionId>,
    pub(in crate::handler) flags: ClientFlags,
    pub(in crate::handler) control_tx: AttachControlSender,
    pub(in crate::handler) control_backlog: Arc<AtomicUsize>,
    pub(in crate::handler) _read_demand: PaneOutputReadDemandGuard,
    pub(in crate::handler) render_stream: bool,
    pub(in crate::handler) render_refresh_pending: bool,
    pub(in crate::handler) uid: u32,
    pub(in crate::handler) user: UserIdentity,
    pub(in crate::handler) can_write: bool,
    /// Focus reported by this exact attach generation. This is independent of
    /// suspension, activity and input permission.
    pub(in crate::handler) reported_focused: bool,
    /// Only native terminal attaches contribute physical focus. Web session
    /// views remain explicit virtual registrations until a browser protocol is
    /// qualified separately.
    pub(in crate::handler) focus_origin: AttachFocusOrigin,
    pub(in crate::handler) session_scope: AttachSessionScope,
    /// A timed-out OSC 52 request makes subsequent untagged responses
    /// impossible to correlate safely for this attach generation.
    pub(in crate::handler) clipboard_queries_desynchronized: bool,
    pub(in crate::handler) suspended: bool,
    pub(in crate::handler) closing: Arc<AtomicBool>,
    /// An accepted closing control owns the departure; sender failure does not.
    pub(in crate::handler) detach_event_owned: bool,
    /// The server initiated this close without first emitting `client-detached`.
    pub(in crate::handler) emit_detached_on_finish: bool,
    pub(in crate::handler) terminal_context: OuterTerminalContext,
    /// Expanded `set-titles-string` and OSC 7 path this client's outer terminal
    /// was last told about, tmux's per-client `c->title` / `c->path`.
    pub(in crate::handler) client_title: ClientTitleState,
    /// Outer terminal geometry, status rows included. Every consumer derives
    /// content geometry from it by subtracting the status rows exactly once,
    /// so an already-subtracted row count must never be stored here.
    pub(in crate::handler) client_size: TerminalSize,
    pub(in crate::handler) client_size_provenance: AttachClientSizeProvenance,
    pub(in crate::handler) client_pixels: Option<TerminalPixels>,
    pub(in crate::handler) size_sequence: u64,
    /// Server-monotonic ordering of the client's latest accepted live input.
    pub(in crate::handler) last_activity_sequence: u64,
    /// Unix timestamp of the client's latest accepted live input.
    pub(in crate::handler) activity_at: ClientActivityTimestamp,
    pub(in crate::handler) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(in crate::handler) render_generation: u64,
    pub(in crate::handler) overlay_generation: u64,
    pub(in crate::handler) overlay_state_id: u64,
    pub(in crate::handler) display_panes_state_id: u64,
    pub(in crate::handler) key_table_name: Option<String>,
    pub(in crate::handler) key_table_set_at: Option<Instant>,
    pub(in crate::handler) key_table_generation: u64,
    pub(in crate::handler) repeat_deadline: Option<Instant>,
    pub(in crate::handler) repeat_active: bool,
    pub(in crate::handler) last_key: Option<KeyCode>,
    pub(in crate::handler) held_keyboard: super::super::pane_support::HeldKeys,
    pub(in crate::handler) assumed_paste: super::super::pane_support::AssumedPasteState,
    pub(in crate::handler) idle_lock_timer: Option<super::super::lock_support::IdleLockTimer>,
    pub(in crate::handler) mouse: ClientMouseState,
    pub(in crate::handler) prompt: Option<ClientPromptState>,
    pub(in crate::handler) mode_tree_state_id: u64,
    pub(in crate::handler) mode_tree: Option<ModeTreeClientState>,
    pub(in crate::handler) mode_tree_frame: Option<Vec<u8>>,
    pub(in crate::handler) overlay: Option<ClientOverlayState>,
    pub(in crate::handler) display_panes: Option<DisplayPanesClientState>,
    pub(in crate::handler) transient_message: Option<TransientMessageInputState>,
    pub(in crate::handler) transient_terminal_prefix: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct AttachedClientControlOutcome {
    pub(in crate::handler) detached_event:
        Option<super::super::lifecycle_support::UnsequencedLifecycleEvent>,
    pub(in crate::handler) session_name: rmux_proto::SessionName,
    pub(in crate::handler) client_name: String,
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

    /// Whether this attach is still the published one for its client and still
    /// owns `session_id`.
    ///
    /// The name a session is stored under is mutable and its id is not, so a
    /// caller that captured a name before releasing the state lock cannot use
    /// that name as identity: a rename keeps the same lifetime attached to the
    /// same client under a new key.  Callers that need a name read the current
    /// one off the attach this accepts.  A session switch does change
    /// `active.session_id`, so it is still rejected here.
    pub(in crate::handler) fn matches_active_lifetime(
        self,
        active: &ActiveAttach,
        session_id: SessionId,
    ) -> bool {
        self.attach_id == active.id && active.session_id == session_id
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

/// One exact attach registration: a pid *and* the generation holding it.
///
/// A pid on its own names whichever client owns it *now*.
/// `register_attach_identity` replaces `by_pid[attach_pid]` in place and hands
/// the replacement a fresh `id`, so a command that captured a registration must
/// carry this pair to keep speaking for the one it captured — and to stop
/// speaking at all once a replacement has taken the pid over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct AttachGeneration {
    attach_pid: u32,
    attach_id: u64,
}

impl AttachGeneration {
    pub(in crate::handler) const fn new(attach_pid: u32, attach_id: u64) -> Self {
        Self {
            attach_pid,
            attach_id,
        }
    }

    /// `true` when `active`, registered under `attach_pid`, is this exact
    /// registration rather than a same-pid replacement of it.
    pub(in crate::handler) fn is(self, attach_pid: u32, active: &ActiveAttach) -> bool {
        self.attach_pid == attach_pid && self.attach_id == active.id
    }
}

impl ActiveAttach {
    pub(in crate::handler) fn may_leave_session(&self) -> bool {
        self.session_scope == AttachSessionScope::Movable
    }

    pub(in crate::handler) fn needs_detached_event(&self) -> bool {
        self.emit_detached_on_finish || !self.detach_event_owned
    }

    pub(in crate::handler) const fn identity(&self, attach_pid: u32) -> ActiveAttachIdentity {
        ActiveAttachIdentity::new(attach_pid, self.id, self.session_id)
    }

    /// `true` while `client_size` is still the anchor registration inferred
    /// from the session rather than geometry the client declared.
    pub(in crate::handler) const fn client_size_is_inferred(&self) -> bool {
        matches!(
            self.client_size_provenance,
            AttachClientSizeProvenance::InferredFromSession
        )
    }

    /// Records outer terminal geometry the client itself reported, promoting an
    /// inferred anchor even when the dimensions are numerically unchanged.
    pub(in crate::handler) fn set_declared_client_size(&mut self, client_size: TerminalSize) {
        self.client_size = client_size;
        self.client_size_provenance = AttachClientSizeProvenance::Declared;
    }

    /// Remembers what this client's outer terminal was just given.
    ///
    /// A render made while `set-titles` is off commits nothing and must leave
    /// the remembered values alone: tmux keeps `c->title` across the option
    /// going off and back on, so a title that is still current is not
    /// re-emitted when it is switched back on. A render that resolved a title
    /// but wrote no bytes commits nothing either — see
    /// [`RenderedClientTitle::committed`].
    pub(in crate::handler) fn remember_client_title(
        &mut self,
        rendered: Option<&RenderedClientTitle>,
    ) {
        if let Some(state) = rendered.and_then(RenderedClientTitle::committed) {
            self.client_title = state.clone();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct DisplayPanesClientState {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) origin: RequesterOrigin,
    pub(in crate::handler) command_context: QueueExecutionContext,
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
    pub(in crate::handler) target_identity: StableTargetIdentity,
}

#[derive(Debug)]
pub(crate) struct AttachRegistration {
    pub(crate) control_tx: mpsc::UnboundedSender<AttachControl>,
    pub(crate) control_backlog: Arc<AtomicUsize>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(crate) terminal_context: OuterTerminalContext,
    /// What the attach frame this registration publishes already told the
    /// outer terminal, so the client's first refresh does not repeat it.
    pub(crate) client_title: Option<ClientTitleState>,
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
    renamed
        .command_context
        .rename_session_targets(old_name, new_name);
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
        label.target_identity.rename_session(old_name, new_name);
    }
    *state = renamed;
    true
}

impl ActiveAttachState {
    pub(in crate::handler) fn record_client_activity(
        &mut self,
        attach_pid: u32,
        sequence: u64,
    ) -> bool {
        let Some(active) = self.by_pid.get_mut(&attach_pid) else {
            return false;
        };
        active.last_activity_sequence = sequence;
        active.activity_at = current_client_activity_timestamp();
        true
    }

    pub(in crate::handler) fn attached_count(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> usize {
        self.by_pid
            .values()
            .filter(|active| &active.session_name == session_name && !active.suspended)
            .count()
    }

    /// Renames `session_name` in every client registration and returns the
    /// sessions of the clients that remember it as their last session: their
    /// `#{client_last_session}` changed, so they need the redraw tmux gives.
    pub(in crate::handler) fn rename_session(
        &mut self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        new_name: &rmux_proto::SessionName,
    ) -> Vec<rmux_proto::SessionName> {
        let mut remembering_sessions = Vec::new();
        for active in self.by_pid.values_mut() {
            if &active.session_name == session_name && active.session_id == session_id {
                active.session_name = new_name.clone();
                if let Some(mode_tree) = active.mode_tree.as_mut() {
                    mode_tree.rename_session(session_name, new_name);
                }
            }
            if active.display_panes.as_mut().is_some_and(|display_panes| {
                !rename_display_panes_state(display_panes, session_name, new_name)
            }) {
                active.display_panes = None;
                active.display_panes_state_id = active.display_panes_state_id.saturating_add(1);
            }
            if active.last_session.as_ref() == Some(session_name)
                && active.last_session_id == Some(session_id)
            {
                active.last_session = Some(new_name.clone());
                if !remembering_sessions.contains(&active.session_name) {
                    remembering_sessions.push(active.session_name.clone());
                }
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
        for focused in self.focused_panes.values_mut() {
            focused.rename_session_target(session_name, new_name);
        }
        remembering_sessions
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
