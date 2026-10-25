use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use rmux_core::{LifecycleEvent, Session};
use rmux_proto::{
    OptionName, RmuxError, SessionId, SessionName, TerminalSize, WindowId, WindowTarget,
};

use crate::pane_io::AttachControl;
use crate::status_lines::content_rows_for_status;

use super::super::{client_support::SwitchTargetSelection, RequestHandler};

#[path = "resize_policy/identity.rs"]
mod identity;
#[cfg(test)]
#[path = "resize_policy/tests.rs"]
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
    sequence: u64,
}

impl ControlSizeCandidate {
    const fn size_candidate(&self) -> AttachedSizeCandidate {
        AttachedSizeCandidate::from_content(self.size, self.sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectedWindowSize {
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

    fn matches_window(self, session: &Session, window_index: u32) -> bool {
        session
            .window_at(window_index)
            .is_some_and(|window| window.size() == self.content_size)
            && (session.active_window_index() != window_index
                || session.terminal_size() == self.terminal_size)
    }

    fn apply_to_window(self, session: &mut Session, window_index: u32) -> Result<(), RmuxError> {
        if session.active_window_index() == window_index {
            session.resize_active_window_geometry(self.terminal_size, self.content_size);
        } else {
            session.resize_window(window_index, self.content_size)?;
        }
        Ok(())
    }

    fn apply_to_active_window(self, session: &mut Session) {
        session.resize_active_window_geometry(self.terminal_size, self.content_size);
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct AttachedSizeSelection {
    selected_size: Option<SelectedWindowSize>,
    pub(in crate::handler) session_id: SessionId,
    active_window_index: u32,
    active_window_id: WindowId,
    policy: AttachedWindowSizePolicy,
    status: Option<String>,
    aggressive_resize: bool,
    linked_sessions: LinkedSessions,
    active_attach_epoch: u64,
    incoming_client: Option<IncomingSizeClient>,
    control_candidates: Vec<ControlSizeCandidate>,
}

impl AttachedSizeSelection {
    fn still_exists(&self, session: &rmux_core::Session) -> bool {
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

    pub(in crate::handler) fn matches_window(&self, session: &Session, window_index: u32) -> bool {
        self.selected_size
            .is_none_or(|selected| selected.matches_window(session, window_index))
    }

    pub(in crate::handler) fn apply_to_window(
        &self,
        session: &mut Session,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        match self.selected_size {
            Some(selected) => selected.apply_to_window(session, window_index),
            None => Ok(()),
        }
    }

    pub(in crate::handler) fn apply_to_active_window(&self, session: &mut Session) {
        if let Some(selected) = self.selected_size {
            selected.apply_to_active_window(session);
        }
    }
}

pub(in crate::handler) const ATTACHED_SIZE_RECONCILE_ATTEMPTS: usize = 4;

#[derive(Clone, Copy)]
pub(in crate::handler) struct ControlResizeClient<'a> {
    active_attach: &'a super::ActiveAttachState,
    active_control: &'a super::super::ActiveControlState,
    control_pid: u32,
    /// `None` while the client has never announced a size with
    /// `refresh-client -C`; such a client owns no window geometry, exactly as
    /// tmux's `ignore_client_size()` skips it.
    declared_size: Option<TerminalSize>,
    size_sequence: u64,
}

impl<'a> ControlResizeClient<'a> {
    pub(in crate::handler) const fn new(
        active_attach: &'a super::ActiveAttachState,
        active_control: &'a super::super::ActiveControlState,
        control_pid: u32,
        declared_size: Option<TerminalSize>,
        size_sequence: u64,
    ) -> Self {
        Self {
            active_attach,
            active_control,
            control_pid,
            declared_size,
            size_sequence,
        }
    }
}

/// Applies a control client's reported size to its session.
///
/// Any window this really resizes is recorded by the geometry chokepoint and
/// published by `RequestHandler::publish_applied_window_resizes`; callers must
/// never publish a layout notification of their own.
pub(in crate::handler) fn resize_control_session_for_client(
    state: &mut crate::pane_terminals::HandlerState,
    client: ControlResizeClient<'_>,
    session_name: &SessionName,
    expected_session_id: SessionId,
) -> Result<(), RmuxError> {
    let window_index = state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == expected_session_id)
        .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?
        .active_window_index();
    let (current_size, selected_size) = control_resize_selection(
        state,
        client,
        session_name,
        expected_session_id,
        window_index,
    )?;
    let Some(selected_size) = selected_size else {
        return Ok(());
    };
    if current_size == selected_size {
        return Ok(());
    }

    state.mutate_session_and_resize_active_window_geometry(session_name, |session| {
        if session.id() != expected_session_id {
            return Err(crate::pane_terminals::session_not_found(session_name));
        }
        session.touch_attached();
        selected_size.apply_to_active_window(session);
        Ok(())
    })
}

/// Moves a control client to `session_name` and applies the geometry that move
/// implies.
///
/// Any window this really resizes is recorded by the geometry chokepoint and
/// published by `RequestHandler::publish_applied_window_resizes`; callers must
/// never publish a layout notification of their own.
pub(in crate::handler) fn switch_control_session_for_client(
    state: &mut crate::pane_terminals::HandlerState,
    client: ControlResizeClient<'_>,
    session_name: &SessionName,
    expected_session_id: SessionId,
    selection: Option<&SwitchTargetSelection>,
) -> Result<Vec<SessionName>, RmuxError> {
    let window_index = match selection {
        Some(selection) => selection.window_target().window_index(),
        None => state
            .sessions
            .session(session_name)
            .filter(|session| session.id() == expected_session_id)
            .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?
            .active_window_index(),
    };
    let (current_size, selected_size) = control_resize_selection(
        state,
        client,
        session_name,
        expected_session_id,
        window_index,
    )?;
    let resizes = selected_size.is_some_and(|selected_size| selected_size != current_size);
    if selection.is_none() && !resizes {
        return Ok(Vec::new());
    }

    let (_, refresh_sessions) = state.mutate_session_and_resize_window_terminal_with_family(
        session_name,
        window_index,
        |session| {
            if session.id() != expected_session_id {
                return Err(crate::pane_terminals::session_not_found(session_name));
            }
            if let Some(selection) = selection {
                selection.apply_to_session(session)?;
            }
            if let Some(selected_size) = selected_size {
                selected_size.apply_to_window(session, window_index)?;
            }
            Ok(())
        },
    )?;
    Ok(refresh_sessions)
}

fn control_resize_selection(
    state: &crate::pane_terminals::HandlerState,
    client: ControlResizeClient<'_>,
    session_name: &SessionName,
    expected_session_id: SessionId,
    window_index: u32,
) -> Result<(SelectedWindowSize, Option<SelectedWindowSize>), RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == expected_session_id)
        .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?;
    let current_size = session
        .window_at(window_index)
        .ok_or_else(|| RmuxError::invalid_target(window_index.to_string(), "window not found"))?
        .size();
    let policy = policy_from_option_value(state.options.resolve_for_window(
        session_name,
        window_index,
        OptionName::WindowSize,
    ));
    let aggressive_resize =
        state
            .options
            .resolve_for_window(session_name, window_index, OptionName::AggressiveResize)
            == Some("on");
    let linked_sessions =
        linked_session_statuses(state, session_name, window_index, aggressive_resize);
    let candidates = attached_size_candidates(
        client.active_attach,
        &linked_sessions,
        client
            .declared_size
            .map(|size| AttachedSizeCandidate::from_content(size, client.size_sequence)),
        // A control client is registered in `active_control`; it owns no
        // `active_attach` entry for its own candidate to displace.
        None,
    );
    let control_candidates = control_size_candidates(
        client.active_control,
        &linked_sessions,
        Some(client.control_pid),
    );
    Ok((
        SelectedWindowSize {
            terminal_size: session.terminal_size(),
            content_size: current_size,
        },
        selected_client_size(policy, candidates, &control_candidates),
    ))
}

/// The control clients that own a size for the window-size policy.
///
/// A control client is only a candidate once it has announced a size with
/// `refresh-client -C`: tmux 3.7b's `ignore_client_size()` skips a
/// `CLIENT_CONTROL` client without `CLIENT_SIZECHANGED`, so a freshly attached
/// control client — the state every control client starts in, including
/// iTerm2's `-CC` before its first `refresh-client -C` — must not pull the
/// session down to its 80x24 placeholder.
fn control_size_candidates(
    active_control: &super::super::ActiveControlState,
    linked_sessions: &LinkedSessions,
    excluded_pid: Option<u32>,
) -> Vec<ControlSizeCandidate> {
    let mut candidates = active_control
        .by_pid
        .iter()
        .filter(|(pid, active)| {
            Some(**pid) != excluded_pid
                && active.size_declared
                && !active.flags.ignore_size
                && !active.closing.load(Ordering::Acquire)
                && active
                    .session_name
                    .as_ref()
                    .zip(active.session_id)
                    .is_some_and(|(session_name, session_id)| {
                        linked_sessions.contains(session_name, session_id)
                    })
        })
        .filter_map(|(&pid, active)| {
            let (session_name, session_id) = active.session_name.clone().zip(active.session_id)?;
            Some(ControlSizeCandidate {
                pid,
                control_id: active.id,
                session_name,
                session_id,
                size: TerminalSize {
                    cols: active.client_width,
                    rows: active.client_height,
                },
                sequence: active.size_sequence,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| (candidate.pid, candidate.control_id));
    candidates
}

/// Ranks the candidates, each of which already lost the status rows of its own
/// session when it was built.
fn selected_client_size(
    policy: AttachedWindowSizePolicy,
    mut attached_candidates: Vec<AttachedSizeCandidate>,
    control_candidates: &[ControlSizeCandidate],
) -> Option<SelectedWindowSize> {
    attached_candidates.extend(
        control_candidates
            .iter()
            .map(ControlSizeCandidate::size_candidate),
    );
    let selected = selected_attached_size(policy, &attached_candidates)?;
    let terminal_size = TerminalSize {
        cols: selected.size.cols,
        rows: selected.stored_rows,
    };
    Some(SelectedWindowSize {
        terminal_size,
        content_size: selected.size,
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
    target: &WindowTarget,
    policy: AttachedWindowSizePolicy,
) -> Option<TerminalSize> {
    let linked_sessions =
        linked_session_statuses(state, target.session_name(), target.window_index(), false);
    let candidates = attached_size_candidates(active_attach, &linked_sessions, None, None);
    selected_client_size(policy, candidates, &[]).map(SelectedWindowSize::content_size)
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

    pub(in crate::handler) async fn reconcile_attached_session_size(
        &self,
        session_name: &SessionName,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let selection = self
                .selected_attached_session_size(session_name, None)
                .await?;
            self.pause_after_attached_size_selection().await;

            let mut state = self.state.lock().await;
            if state.sessions.session(session_name).is_none() {
                return Ok(None);
            }
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
                session_name,
                &selection,
                true,
            ) {
                continue;
            }
            if selection.selected_size.is_none() {
                return Ok(None);
            }
            let session = state
                .sessions
                .session(session_name)
                .expect("stable attached-size selection was revalidated");
            if selection.matches_window(session, selection.active_window_index) {
                return Ok(None);
            }
            self.pause_before_attached_size_apply().await;
            state.mutate_session_and_resize_active_window_geometry(session_name, |session| {
                selection.apply_to_active_window(session);
                Ok(())
            })?;
            drop(active_control);
            drop(active_attach);
            return Ok(Some(WindowTarget::with_window(
                session_name.clone(),
                selection.active_window_index,
            )));
        }
        Ok(None)
    }

    /// Publishes what tmux publishes for every window that really did change
    /// size since the last publication.
    ///
    /// tmux 3.7b's `resize_window()` runs `window-layout-changed` and then
    /// `window-resized` for every applied resize, and only the former reaches
    /// control clients (as `%layout-change`). Measured 2026-07-25 on a source
    /// session holding a 101x41 PTY client and a 60x20 control client with
    /// `window-size largest`: after `detach-client` on the PTY client, the
    /// control client receives
    ///     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    /// then `%client-detached /dev/ttys007`; when the PTY client's process dies
    /// instead, the same `%layout-change` arrives after `%client-detached`.
    /// All tickets are reserved under one state lock so they publish in tmux's
    /// order.
    pub(in crate::handler) async fn publish_applied_window_resizes(&self) {
        let prepared = {
            let mut state = self.state.lock().await;
            prepare_applied_window_resize_events(&mut state)
        };
        for event in prepared {
            self.emit_prepared(event).await;
        }
    }

    /// Publishes `target` as an applied resize even when its stored size did not
    /// move, for the commands whose tmux counterpart calls `resize_window()`
    /// unconditionally (`resize-window`).
    pub(in crate::handler) async fn emit_applied_window_resize(&self, target: WindowTarget) {
        let prepared = {
            let mut state = self.state.lock().await;
            state.record_applied_window_resize(target);
            prepare_applied_window_resize_events(&mut state)
        };
        for event in prepared {
            self.emit_prepared(event).await;
        }
    }

    pub(in crate::handler) async fn reconcile_attached_session_size_and_emit(
        &self,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.reconcile_attached_session_size(session_name).await?;
        self.publish_applied_window_resizes().await;
        Ok(())
    }

    pub(in crate::handler) async fn reconcile_attached_window_size(
        &self,
        target: &WindowTarget,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let selection = self.selected_attached_window_size(target, None).await?;
            let mut state = self.state.lock().await;
            if state.sessions.session(target.session_name()).is_none() {
                return Ok(None);
            }
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
                target.session_name(),
                &selection,
                false,
            ) {
                continue;
            }
            if selection.selected_size.is_none() {
                return Ok(None);
            }
            let session = state
                .sessions
                .session(target.session_name())
                .expect("stable attached-size session was revalidated");
            if selection.matches_window(session, target.window_index()) {
                return Ok(None);
            }
            self.pause_before_attached_size_apply().await;
            let window_index = target.window_index();
            state.mutate_session_and_resize_window_terminal(
                target.session_name(),
                window_index,
                |session| selection.apply_to_window(session, window_index),
            )?;
            drop(active_control);
            drop(active_attach);
            return Ok(Some(target.clone()));
        }
        Ok(None)
    }

    pub(in crate::handler) async fn reconcile_attached_window_size_and_emit(
        &self,
        target: &WindowTarget,
    ) -> Result<(), RmuxError> {
        self.reconcile_attached_window_size(target).await?;
        self.publish_applied_window_resizes().await;
        Ok(())
    }

    pub(in crate::handler) async fn selected_attached_session_size(
        &self,
        session_name: &SessionName,
        incoming_client: Option<IncomingSizeClient>,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        let (
            policy,
            status,
            aggressive_resize,
            linked_sessions,
            session_id,
            active_window_index,
            active_window_id,
        ) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Err(crate::pane_terminals::session_not_found(session_name));
            };
            let active_window_index = session.active_window_index();
            let active_window_id = session.window().id();
            let policy = policy_from_option_value(state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::AggressiveResize,
            ) == Some("on");
            (
                policy,
                state
                    .options
                    .resolve(Some(session_name), OptionName::Status)
                    .map(str::to_owned),
                aggressive_resize,
                linked_session_statuses(
                    &state,
                    session_name,
                    active_window_index,
                    aggressive_resize,
                ),
                session.id(),
                active_window_index,
                active_window_id,
            )
        };

        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates = self.incoming_client_candidates(
                &active_attach,
                &linked_sessions,
                incoming_client,
                status.as_deref(),
            );
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_client_size(policy, candidates, &control_candidates),
            session_id,
            active_window_index,
            active_window_id,
            policy,
            status,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client,
            control_candidates,
        })
    }

    /// The attached candidates, with the incoming client counted exactly once.
    ///
    /// `status` is the target session's own resolved value: the incoming client
    /// is joining that session, so it is the status that converts its outer
    /// terminal rows. Under `aggressive-resize` the target need not appear in
    /// its own linked set when it is not currently showing the window, so this
    /// value is resolved directly rather than looked up there.
    fn incoming_client_candidates(
        &self,
        active_attach: &super::ActiveAttachState,
        linked_sessions: &LinkedSessions,
        incoming_client: Option<IncomingSizeClient>,
        status: Option<&str>,
    ) -> Vec<AttachedSizeCandidate> {
        let incoming_candidate = incoming_client.and_then(|client| {
            client.size.map(|size| {
                AttachedSizeCandidate::from_terminal(size, client.size_sequence, status)
            })
        });
        attached_size_candidates(
            active_attach,
            linked_sessions,
            incoming_candidate,
            incoming_client.and_then(|client| client.displaced),
        )
    }

    pub(in crate::handler) async fn selected_attached_window_size(
        &self,
        target: &WindowTarget,
        incoming_client: Option<IncomingSizeClient>,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        let (policy, status, aggressive_resize, linked_sessions, session_id, window_id) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| crate::pane_terminals::session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
            let policy = policy_from_option_value(state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::AggressiveResize,
            ) == Some("on");
            (
                policy,
                state
                    .options
                    .resolve(Some(target.session_name()), OptionName::Status)
                    .map(str::to_owned),
                aggressive_resize,
                linked_session_statuses(
                    &state,
                    target.session_name(),
                    target.window_index(),
                    aggressive_resize,
                ),
                session.id(),
                window.id(),
            )
        };
        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates = self.incoming_client_candidates(
                &active_attach,
                &linked_sessions,
                incoming_client,
                status.as_deref(),
            );
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_client_size(policy, candidates, &control_candidates),
            session_id,
            active_window_index: target.window_index(),
            active_window_id: window_id,
            policy,
            status,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client,
            control_candidates,
        })
    }

    pub(in crate::handler) fn attached_size_selection_is_current(
        &self,
        state: &crate::pane_terminals::HandlerState,
        active_attach: &super::ActiveAttachState,
        active_control: &super::super::ActiveControlState,
        session_name: &SessionName,
        selection: &AttachedSizeSelection,
        require_active_window: bool,
    ) -> bool {
        if self.active_attach_epoch.load(Ordering::Acquire) != selection.active_attach_epoch {
            return false;
        }
        let Some(session) = state.sessions.session(session_name) else {
            return false;
        };
        if !selection.still_exists(session)
            || (require_active_window
                && session.active_window_index() != selection.active_window_index)
        {
            return false;
        }
        let policy = policy_from_option_value(state.options.resolve_for_window(
            session_name,
            selection.active_window_index,
            OptionName::WindowSize,
        ));
        let status = state
            .options
            .resolve(Some(session_name), OptionName::Status);
        let aggressive_resize = state.options.resolve_for_window(
            session_name,
            selection.active_window_index,
            OptionName::AggressiveResize,
        ) == Some("on");
        let current_control_candidates =
            control_size_candidates(active_control, &selection.linked_sessions, None);
        policy == selection.policy
            && status == selection.status.as_deref()
            && aggressive_resize == selection.aggressive_resize
            && linked_session_statuses(
                state,
                session_name,
                selection.active_window_index,
                aggressive_resize,
            ) == selection.linked_sessions
            && current_control_candidates == selection.control_candidates
            && selected_client_size(
                policy,
                self.incoming_client_candidates(
                    active_attach,
                    &selection.linked_sessions,
                    selection.incoming_client,
                    status,
                ),
                &current_control_candidates,
            ) == selection.selected_size
    }

    pub(in crate::handler) async fn prune_stale_attached_clients_for_session(
        &self,
        session_name: &SessionName,
    ) -> Vec<u32> {
        let stale_clients = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    &active.session_name == session_name
                        && (active.control_tx.is_closed()
                            || active.control_backlog.load(Ordering::Acquire)
                                >= super::ATTACH_CONTROL_BACKLOG_LIMIT)
                })
                .map(|(pid, active)| active.identity(*pid))
                .collect::<Vec<_>>()
        };
        self.remove_attached_clients_for_session(session_name, stale_clients)
            .await
    }

    pub(in crate::handler) async fn remove_attached_clients_for_session(
        &self,
        session_name: &SessionName,
        attach_identities: Vec<super::ActiveAttachIdentity>,
    ) -> Vec<u32> {
        if attach_identities.is_empty() {
            return Vec::new();
        }
        let (removed, key_tables, overlays) = {
            let mut active_attach = self.active_attach.lock().await;
            let mut removed = Vec::new();
            let mut key_tables = Vec::new();
            let mut overlays = Vec::new();
            for identity in attach_identities {
                let pid = identity.attach_pid();
                let remove = active_attach
                    .by_pid
                    .get(&pid)
                    .is_some_and(|active| identity.matches(pid, session_name, active));
                if remove {
                    let mut active = active_attach
                        .remove_attached_client(pid)
                        .expect("attached client checked above");
                    let _ = active.control_tx.send(AttachControl::Detach);
                    active.closing.store(true, Ordering::SeqCst);
                    removed.push((pid, active.client_name));
                    if let Some(table_name) = active.key_table_name.take() {
                        key_tables.push(table_name);
                    }
                    overlays.push(active.overlay.take());
                }
            }
            (removed, key_tables, overlays)
        };
        if !removed.is_empty() {
            self.bump_active_attach_epoch();
        }

        for overlay in overlays {
            super::terminate_overlay_job(overlay);
        }
        if !key_tables.is_empty() {
            let mut state = self.state.lock().await;
            for table_name in key_tables {
                state.key_bindings.unref_table(&table_name);
            }
        }
        for (_, client_name) in &removed {
            self.emit_without_attached_refresh(LifecycleEvent::ClientDetached {
                session_name: session_name.clone(),
                client_name: Some(client_name.clone()),
            })
            .await;
        }
        removed.into_iter().map(|(pid, _)| pid).collect()
    }
}

/// Reserves the `window-layout-changed` / `window-resized` pair for every
/// window the geometry mutation paths recorded, in tmux's order, under one
/// state lock.
pub(in crate::handler) fn prepare_applied_window_resize_events(
    state: &mut crate::pane_terminals::HandlerState,
) -> Vec<crate::handler::QueuedLifecycleEvent> {
    let targets = state.take_applied_window_resizes();
    let mut prepared = Vec::with_capacity(targets.len().saturating_mul(2));
    for resize in targets {
        let (target, layout_change_prepared) = resize.into_parts();
        if !layout_change_prepared {
            prepared.push(crate::handler::prepare_lifecycle_event(
                state,
                &LifecycleEvent::WindowLayoutChanged {
                    target: target.clone(),
                },
            ));
        }
        prepared.push(crate::handler::prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowResized { target },
        ));
    }
    prepared
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
        .flat_map(|(session_name, session)| {
            session
                .windows()
                .iter()
                .filter(|(_, window)| wanted.contains(&window.id().as_u32()))
                .map(move |(window_index, window)| {
                    (
                        state.runtime_session_name_for_window(session_name, *window_index),
                        window.id().as_u32(),
                        WindowTarget::with_window(session_name.clone(), *window_index),
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

fn policy_from_option_value(value: Option<&str>) -> AttachedWindowSizePolicy {
    match value {
        Some("largest") => AttachedWindowSizePolicy::Largest,
        Some("smallest") => AttachedWindowSizePolicy::Smallest,
        Some("manual") => AttachedWindowSizePolicy::Manual,
        Some("latest") | None => AttachedWindowSizePolicy::Latest,
        Some(_) => AttachedWindowSizePolicy::Latest,
    }
}

fn selected_attached_size(
    policy: AttachedWindowSizePolicy,
    candidates: &[AttachedSizeCandidate],
) -> Option<SelectedContentSize> {
    match policy {
        AttachedWindowSizePolicy::Manual => None,
        AttachedWindowSizePolicy::Latest => candidates
            .iter()
            .max_by_key(|candidate| candidate.sequence)
            .map(|candidate| SelectedContentSize {
                size: candidate.size,
                row_basis: candidate.basis,
                stored_rows: candidate.stored_rows,
            }),
        AttachedWindowSizePolicy::Largest => selected_extreme_attached_size(candidates, u16::max),
        AttachedWindowSizePolicy::Smallest => selected_extreme_attached_size(candidates, u16::min),
    }
}

fn selected_extreme_attached_size(
    candidates: &[AttachedSizeCandidate],
    select: impl Fn(u16, u16) -> u16,
) -> Option<SelectedContentSize> {
    let first = candidates.first()?;
    let size = candidates
        .iter()
        .skip(1)
        .fold(first.size, |selected, candidate| TerminalSize {
            cols: select(selected.cols, candidate.size.cols),
            rows: select(selected.rows, candidate.size.rows),
        });
    let row_basis = if candidates.iter().any(|candidate| {
        candidate.size.rows == size.rows && candidate.basis == ClientSizeBasis::Content
    }) {
        ClientSizeBasis::Content
    } else {
        ClientSizeBasis::Terminal
    };
    let stored_rows = candidates
        .iter()
        .filter(|candidate| candidate.size.rows == size.rows && candidate.basis == row_basis)
        .map(|candidate| candidate.stored_rows)
        .reduce(select)
        .expect("the selected row has at least one owning candidate");
    Some(SelectedContentSize {
        size,
        row_basis,
        stored_rows,
    })
}
