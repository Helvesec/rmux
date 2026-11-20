use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use rmux_core::{AttachedWindowGeometry, WindowSwitch};
use rmux_proto::{
    OptionName, RmuxError, SessionId, SessionName, TerminalSize, WindowId, WindowTarget,
};

use crate::status_lines::content_rows_for_status;

use super::super::{client_support::SwitchTargetSelection, RequestHandler};

mod client_removal;
mod control;
mod identity;
mod ranking;
mod reconcile;
mod selection;
use control::control_size_candidates;
pub(in crate::handler) use control::{
    resize_control_session_for_client, switch_control_session_for_client, ControlResizeClient,
};
use ranking::{policy_from_option_value, selected_client_size};
pub(in crate::handler) use reconcile::prepare_applied_window_resize_events;
#[cfg(test)]
#[path = "../../handler_attach/resize_policy/tests.rs"]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum AttachedWindowSizePolicy {
    Latest,
    Largest,
    Smallest,
    Manual,
}

/// One client's claim on the window geometry, already reduced to window
/// *content* rows.
#[derive(Debug, Clone, Copy)]
struct AttachedSizeCandidate {
    size: TerminalSize,
    /// The rows to store back as terminal geometry: the outer rows this client
    /// really owns, before `status` came off them.
    stored_rows: u16,
    sequence: u64,
    basis: ClientSizeBasis,
}

impl AttachedSizeCandidate {
    /// A client whose stored geometry is its *outer* terminal size.
    ///
    /// `status` must be the resolved `status` option of the session **this
    /// client is attached to**, never the resize target's: tmux 3.7b's
    /// `default_window_size()` subtracts `status_line_size(loop)` per client,
    /// and that helper reads `loop->session`'s options. A window linked into two
    /// sessions with different `status` values therefore converts each client
    /// with its own session's status.
    fn from_terminal(size: TerminalSize, sequence: u64, status: Option<&str>) -> Self {
        Self {
            size: TerminalSize {
                cols: size.cols,
                rows: content_rows_for_status(status, size.rows),
            },
            stored_rows: size.rows,
            sequence,
            basis: ClientSizeBasis::Terminal,
        }
    }

    /// A client that already reported window *content* geometry, which never
    /// loses status rows.
    const fn from_content(size: TerminalSize, sequence: u64) -> Self {
        Self {
            size,
            stored_rows: size.rows,
            sequence,
            basis: ClientSizeBasis::Content,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientSizeBasis {
    Content,
    Terminal,
}

/// The sessions a window is linked into, each paired with the resolved `status`
/// value that converts *its own* clients' outer terminal rows.
///
/// Membership is keyed on the exact `(SessionName, SessionId)` pair so a
/// destroyed session's name cannot lend its identity to a stale client.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkedSessions {
    status_by_identity: HashMap<(SessionName, SessionId), Option<String>>,
}

impl LinkedSessions {
    /// The `status` that converts a client attached to this identity, or `None`
    /// when the window is not linked into that session at all.
    fn client_status(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
    ) -> Option<Option<&str>> {
        self.status_by_identity
            .get(&(session_name.clone(), session_id))
            .map(Option::as_deref)
    }

    fn contains(&self, session_name: &SessionName, session_id: SessionId) -> bool {
        self.client_status(session_name, session_id).is_some()
    }
}

/// The client a size selection is being computed *for*.
///
/// tmux 3.7b's `cmd_switch_client_exec` assigns `c->session = s` before it calls
/// `recalculate_sizes()`, so a client moving between two aliases of one linked
/// window is counted exactly once, under the session it is joining. rmux selects
/// the size *before* its atomic commit, while the client's `active_attach`
/// registration still names the session it is leaving. That registration is
/// therefore replaced by this candidate, never counted beside it: a linked
/// window whose two aliases resolve different `status` values would otherwise
/// let one client vote twice, once per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct IncomingSizeClient {
    displaced: Option<super::AttachGeneration>,
    /// `None` while the client's flags leave it owning no size at all, exactly
    /// as tmux's `ignore_client_size()` skips it.
    size: Option<TerminalSize>,
    /// The arrival order this candidate votes with under `window-size latest`.
    size_sequence: u64,
}

impl IncomingSizeClient {
    /// `displaced` is the exact registration this candidate stands in for: the
    /// two describe one physical client, so only one of them may vote.
    ///
    /// `None` means the client holds no registration for the candidate to
    /// displace — a first attach, or a control client, which is registered in
    /// `active_control` instead. Such a candidate stands *beside* every attached
    /// vote; it must never displace one, because a matching pid number alone
    /// does not make some other live client this client.
    ///
    /// `size_sequence` is the arrival order the joining client votes with, and a
    /// caller that displaces a registration owns a second obligation: it must
    /// commit this same value to that registration. tmux 3.7b assigns
    /// `s->curw->window->latest = c` before `recalculate_sizes()`, so the
    /// joining client is the window's newest sizing authority from that instant
    /// on — not only for the one selection this candidate is built for. Leaving
    /// the registration on its original order lets the next reconcile of the
    /// same linked family pick a different winner than the frame just sent.
    pub(in crate::handler) fn joining(
        displaced: Option<super::AttachGeneration>,
        size: TerminalSize,
        flags: super::ClientFlags,
        size_sequence: u64,
    ) -> Self {
        Self {
            displaced,
            size: (!flags.contains(super::ClientFlags::IGNORESIZE)).then_some(size),
            size_sequence,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlSizeCandidate {
    pid: u32,
    control_id: u64,
    session_name: SessionName,
    session_id: SessionId,
    size: TerminalSize,
    window_limit: Option<TerminalSize>,
    sequence: u64,
}

impl ControlSizeCandidate {
    const fn size_candidate(&self) -> AttachedSizeCandidate {
        AttachedSizeCandidate::from_content(self.size, self.sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectedWindowSize {
    constrained_manual: bool,
    terminal_size: TerminalSize,
    content_size: TerminalSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectedContentSize {
    size: TerminalSize,
    row_basis: ClientSizeBasis,
    stored_rows: u16,
}

impl SelectedWindowSize {
    const fn content_size(self) -> TerminalSize {
        self.content_size
    }

    fn matches_window(self, session: rmux_core::ManagedSessionView<'_>, window_index: u32) -> bool {
        session
            .window_at(window_index)
            .is_some_and(|window| window.size() == self.content_size)
            && (session.active_window_index() != window_index
                || session.terminal_size() == self.terminal_size)
    }

    fn apply_to_attached_geometry(self, mut geometry: AttachedWindowGeometry<'_>) {
        if self.constrained_manual {
            geometry.mark_manual_size_constrained();
        }
        geometry.resize(self.terminal_size, self.content_size);
    }

    fn apply_to_window_switch(self, switch: &mut WindowSwitch<'_>) {
        if self.constrained_manual {
            switch.mark_manual_size_constrained();
        }
        switch.resize(self.terminal_size, self.content_size);
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct AttachedSizeSelection {
    selected_size: Option<SelectedWindowSize>,
    pub(in crate::handler) session_id: SessionId,
    active_window_index: u32,
    active_window_id: WindowId,
    policy: AttachedWindowSizePolicy,
    manual_size: rmux_core::ManualWindowSize,
    status: Option<String>,
    aggressive_resize: bool,
    linked_sessions: LinkedSessions,
    active_attach_epoch: u64,
    incoming_client: Option<IncomingSizeClient>,
    control_candidates: Vec<ControlSizeCandidate>,
}

impl AttachedSizeSelection {
    fn still_exists(&self, session: rmux_core::ManagedSessionView<'_>) -> bool {
        session.id() == self.session_id
            && session
                .window_at(self.active_window_index)
                .is_some_and(|window| window.id() == self.active_window_id)
    }

    pub(in crate::handler) fn selected_size(&self) -> Option<TerminalSize> {
        self.selected_size.map(SelectedWindowSize::content_size)
    }

    pub(in crate::handler) fn render_override(
        &self,
        window_index: u32,
    ) -> Option<super::AttachWindowSizeOverride> {
        self.selected_size.map(|selected| {
            super::AttachWindowSizeOverride::new(
                window_index,
                selected.terminal_size,
                selected.content_size,
            )
        })
    }

    pub(in crate::handler) fn matches_window(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        window_index: u32,
    ) -> bool {
        self.selected_size
            .is_none_or(|selected| selected.matches_window(session, window_index))
    }

    pub(in crate::handler) fn apply_to_attached_geometry(
        &self,
        geometry: AttachedWindowGeometry<'_>,
    ) {
        if let Some(selected) = self.selected_size {
            selected.apply_to_attached_geometry(geometry);
        }
    }

    pub(in crate::handler) fn apply_to_window_switch(&self, switch: &mut WindowSwitch<'_>) {
        if let Some(selected) = self.selected_size {
            selected.apply_to_window_switch(switch);
        }
    }
}

pub(in crate::handler) const ATTACHED_SIZE_RECONCILE_ATTEMPTS: usize = 4;

/// Effective cap from the control clients eligible for this canonical window.
pub(in crate::handler) fn control_window_size_limit(
    state: &crate::pane_terminals::HandlerState,
    active_control: &super::super::ActiveControlState,
    target: &WindowTarget,
) -> Option<TerminalSize> {
    let aggressive = state.options.resolve_for_window(
        target.session_name(),
        target.window_index(),
        OptionName::AggressiveResize,
    ) == Some("on");
    let linked_sessions = linked_session_statuses(
        state,
        target.session_name(),
        target.window_index(),
        aggressive,
    );
    let window = state
        .sessions
        .session(target.session_name())?
        .window_at(target.window_index())?;
    control_size_candidates(active_control, &linked_sessions, None, window.id())
        .into_iter()
        .filter_map(|candidate| candidate.window_limit)
        .reduce(|left, right| TerminalSize {
            cols: left.cols.min(right.cols),
            rows: left.rows.min(right.rows),
        })
}

/// The window content geometry `resize-window -A` (largest) or `-a` (smallest)
/// must apply to `target`.
///
/// tmux 3.7b's `cmd_resize_window_exec` hands `-A`/`-a` straight to
/// `default_window_size(.., WINDOW_SIZE_LARGEST | WINDOW_SIZE_SMALLEST)`, which
/// is the walk the automatic `window-size largest`/`smallest` policies already
/// take here: drop every client `ignore_client_size()` rejects, take the status
/// rows of *that client's own session* off its outer terminal rows, then keep
/// the extreme of each dimension on its own. Every session the window is linked
/// into votes, because `default_window_size` asks for `current = 0`: neither
/// `aggressive-resize` nor the window a session currently shows narrows the
/// field.
///
/// `None` means no client owns a size at all, and the caller applies its own
/// fallback the way tmux falls through to `default_window_size`'s `manual:`
/// branch — a window-basis value that never loses status rows.
pub(in crate::handler) fn linked_window_client_content_size(
    state: &crate::pane_terminals::HandlerState,
    active_attach: &super::ActiveAttachState,
    active_control: &super::super::ActiveControlState,
    target: &WindowTarget,
    policy: AttachedWindowSizePolicy,
) -> Option<TerminalSize> {
    let linked_sessions =
        linked_session_statuses(state, target.session_name(), target.window_index(), false);
    let candidates = attached_size_candidates(active_attach, &linked_sessions, None, None);
    let window = state
        .sessions
        .session(target.session_name())?
        .window_at(target.window_index())?;
    let control_candidates =
        control_size_candidates(active_control, &linked_sessions, None, window.id());
    selected_client_size(
        policy,
        candidates,
        &control_candidates,
        window.manual_size(),
    )
    .map(SelectedWindowSize::content_size)
}

impl RequestHandler {
    pub(in crate::handler) async fn attached_window_size_policy_for_session(
        &self,
        session_name: &SessionName,
    ) -> Result<AttachedWindowSizePolicy, RmuxError> {
        let state = self.state.lock().await;
        let Some(session) = state.sessions.session(session_name) else {
            return Err(crate::pane_terminals::session_not_found(session_name));
        };
        let window_index = session.active_window_index();
        Ok(policy_from_option_value(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::WindowSize,
        )))
    }

    pub(in crate::handler) async fn attached_window_size_policy_for_session_identity(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
    ) -> Result<AttachedWindowSizePolicy, RmuxError> {
        let state = self.state.lock().await;
        let Some(session) = state
            .sessions
            .session(session_name)
            .filter(|session| session.id() == session_id)
        else {
            return Err(crate::pane_terminals::session_not_found(session_name));
        };
        let window_index = session.active_window_index();
        Ok(policy_from_option_value(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::WindowSize,
        )))
    }
}

/// The attached clients that own a size for the window-size policy.
///
/// `displaced` is the exact registration `incoming_client` stands in for. The
/// two describe one physical client — the one this selection is being computed
/// for — so counting both would give it two votes under two different sessions'
/// `status` values. The match is on the whole generation, never on the pid
/// alone: a same-pid replacement is a different, legitimate sizing authority
/// that this selection does not speak for.
fn attached_size_candidates(
    active_attach: &super::ActiveAttachState,
    linked_sessions: &LinkedSessions,
    incoming_client: Option<AttachedSizeCandidate>,
    displaced: Option<super::AttachGeneration>,
) -> Vec<AttachedSizeCandidate> {
    let mut candidates = active_attach
        .by_pid
        .iter()
        .filter(|(pid, active)| {
            !displaced.is_some_and(|displaced| displaced.is(**pid, active))
                && !active.suspended
                && !active.closing.load(Ordering::Acquire)
                && !active.flags.contains(super::ClientFlags::IGNORESIZE)
        })
        .filter_map(|(_, active)| {
            // The lookup that admits the client also names the status that
            // converts it: `ActiveAttach.client_size` is outer terminal
            // geometry, and the rows it loses belong to the session this very
            // client is attached to, which a linked window need not share with
            // the resize target.
            let status = linked_sessions.client_status(&active.session_name, active.session_id)?;
            Some(AttachedSizeCandidate::from_terminal(
                active.client_size,
                active.size_sequence,
                status,
            ))
        })
        .collect::<Vec<_>>();
    if let Some(incoming_client) = incoming_client {
        candidates.push(incoming_client);
    }
    candidates
}

fn linked_session_statuses(
    state: &crate::pane_terminals::HandlerState,
    session_name: &SessionName,
    window_index: u32,
    aggressive_resize: bool,
) -> LinkedSessions {
    let linked_sessions = if aggressive_resize {
        state.window_linked_current_sessions_list(session_name, window_index)
    } else {
        state.window_linked_sessions_list(session_name, window_index)
    };
    LinkedSessions {
        status_by_identity: linked_sessions
            .into_iter()
            .filter_map(|linked_session_name| {
                let session_id = state.sessions.session(&linked_session_name)?.id();
                let status = state
                    .options
                    .resolve(Some(&linked_session_name), OptionName::Status)
                    .map(str::to_owned);
                Some(((linked_session_name, session_id), status))
            })
            .collect(),
    }
}

pub(in crate::handler) fn surviving_attached_resize_targets(
    state: &crate::pane_terminals::HandlerState,
    window_ids: impl IntoIterator<Item = WindowId>,
) -> Vec<WindowTarget> {
    let wanted = window_ids
        .into_iter()
        .map(WindowId::as_u32)
        .collect::<HashSet<_>>();
    let mut candidates = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .flat_map(|(session_name, session)| {
            session
                .windows()
                .filter(|(_, window)| wanted.contains(&window.id().as_u32()))
                .map(move |(window_index, window)| {
                    (
                        state.runtime_session_name_for_window(session_name, window_index),
                        window.id().as_u32(),
                        WindowTarget::with_window(session_name.clone(), window_index),
                    )
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.0
            .as_str()
            .cmp(right.0.as_str())
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| {
                left.2
                    .session_name()
                    .as_str()
                    .cmp(right.2.session_name().as_str())
            })
            .then_with(|| left.2.window_index().cmp(&right.2.window_index()))
    });

    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|(runtime_session_name, window_id, target)| {
            seen.insert((runtime_session_name, window_id))
                .then_some(target)
        })
        .collect()
}
