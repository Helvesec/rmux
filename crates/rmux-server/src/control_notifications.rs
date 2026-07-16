use rmux_core::formats::FormatContext;
use rmux_core::{encode_paste_bytes, LifecycleEvent, Session, Window};
use rmux_proto::{octal_escape, SessionName, WindowTarget};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

const LAYOUT_CHANGE_TEMPLATE: &str =
    "%layout-change #{window_id} #{window_layout} #{window_visible_layout} #{window_raw_flags}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlClientSnapshot {
    pub(crate) pid: u32,
    pub(crate) session_name: Option<SessionName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedControlNotification {
    pub(crate) pid: u32,
    pub(crate) line: String,
}

#[must_use]
pub(crate) fn collect_control_notifications(
    state: &HandlerState,
    event: &LifecycleEvent,
    clients: &[ControlClientSnapshot],
) -> Vec<PreparedControlNotification> {
    match event {
        LifecycleEvent::PaneModeChanged { target } => {
            pane_mode_changed_notifications(state, target, clients)
        }
        LifecycleEvent::WindowLayoutChanged { target } => {
            window_layout_changed_notifications(state, target, clients)
        }
        LifecycleEvent::WindowPaneChanged { target } => {
            window_pane_changed_notifications(state, target, clients)
        }
        LifecycleEvent::WindowUnlinked { .. } => window_membership_notifications(
            state,
            event,
            clients,
            "%window-close",
            "%unlinked-window-close",
        ),
        LifecycleEvent::WindowLinked { .. } => window_membership_notifications(
            state,
            event,
            clients,
            "%window-add",
            "%unlinked-window-add",
        ),
        LifecycleEvent::WindowRenamed { .. } => window_renamed_notifications(state, event, clients),
        LifecycleEvent::ClientSessionChanged {
            session_name,
            client_name,
        } => client_session_changed_notifications(
            state,
            session_name,
            client_name.as_deref(),
            clients,
        ),
        LifecycleEvent::ClientDetached { client_name, .. } => broadcast_line(
            clients,
            client_name
                .as_deref()
                .map(|name| format!("%client-detached {name}")),
        ),
        LifecycleEvent::SessionRenamed { session_name } => {
            let line = state
                .sessions
                .session(session_name)
                .map(|session| format!("%session-renamed {}", session_identity(session)));
            broadcast_line(clients, line)
        }
        LifecycleEvent::SessionCreated { .. } | LifecycleEvent::SessionClosed { .. } => {
            broadcast_line(clients, Some("%sessions-changed".to_owned()))
        }
        LifecycleEvent::SessionWindowChanged { session_name } => {
            let line = state.sessions.session(session_name).and_then(|session| {
                session
                    .window_at(session.active_window_index())
                    .map(|window| {
                        format!("%session-window-changed {} {}", session.id(), window.id())
                    })
            });
            broadcast_line(clients, line)
        }
        LifecycleEvent::PasteBufferChanged { buffer_name } => broadcast_line(
            clients,
            Some(format!(
                "%paste-buffer-changed {}",
                control_arg(buffer_name)
            )),
        ),
        LifecycleEvent::PasteBufferDeleted { buffer_name } => broadcast_line(
            clients,
            Some(format!(
                "%paste-buffer-deleted {}",
                control_arg(buffer_name)
            )),
        ),
        LifecycleEvent::ClientActive { .. }
        | LifecycleEvent::ClientAttached { .. }
        | LifecycleEvent::ClientFocusIn { .. }
        | LifecycleEvent::ClientFocusOut { .. }
        | LifecycleEvent::ClientLightTheme { .. }
        | LifecycleEvent::ClientDarkTheme { .. }
        | LifecycleEvent::ClientResized { .. }
        | LifecycleEvent::AlertBell { .. }
        | LifecycleEvent::AlertActivity { .. }
        | LifecycleEvent::AlertSilence { .. }
        | LifecycleEvent::PaneExited { .. }
        | LifecycleEvent::PaneDied { .. }
        | LifecycleEvent::PaneFocusIn { .. }
        | LifecycleEvent::PaneFocusOut { .. }
        | LifecycleEvent::PaneSetClipboard { .. }
        | LifecycleEvent::PaneTitleChanged { .. }
        | LifecycleEvent::WindowResized { .. }
        | LifecycleEvent::AfterSelectWindow { .. }
        | LifecycleEvent::AfterSelectPane { .. }
        | LifecycleEvent::AfterSendKeys { .. }
        | LifecycleEvent::AfterSetOption { .. } => Vec::new(),
    }
}

#[must_use]
pub(crate) fn format_control_message_line(message: &str) -> String {
    let escaped = String::from_utf8_lossy(&encode_paste_bytes(message.as_bytes())).into_owned();
    format!("%message {escaped}")
}

fn broadcast_line(
    clients: &[ControlClientSnapshot],
    line: Option<String>,
) -> Vec<PreparedControlNotification> {
    let Some(line) = line else {
        return Vec::new();
    };
    clients
        .iter()
        .map(|client| PreparedControlNotification {
            pid: client.pid,
            line: line.clone(),
        })
        .collect()
}

fn pane_mode_changed_notifications(
    state: &HandlerState,
    target: &rmux_proto::PaneTarget,
    clients: &[ControlClientSnapshot],
) -> Vec<PreparedControlNotification> {
    let line = resolve_session_window(state, target.session_name(), target.window_index())
        .and_then(|(_session, window)| {
            window
                .pane(target.pane_index())
                .map(|pane| format!("%pane-mode-changed %{}", pane.id().as_u32()))
        });
    broadcast_line(clients, line)
}

fn window_layout_changed_notifications(
    state: &HandlerState,
    target: &WindowTarget,
    clients: &[ControlClientSnapshot],
) -> Vec<PreparedControlNotification> {
    let Some((window_id, line)) = render_layout_change_line(state, target) else {
        return Vec::new();
    };
    session_scoped_clients(clients)
        .filter(|(_, session_name)| session_contains_window_id(state, session_name, window_id))
        .map(|(pid, _session_name)| PreparedControlNotification {
            pid,
            line: line.clone(),
        })
        .collect()
}

fn window_pane_changed_notifications(
    state: &HandlerState,
    target: &WindowTarget,
    clients: &[ControlClientSnapshot],
) -> Vec<PreparedControlNotification> {
    let line = resolve_session_window(state, target.session_name(), target.window_index())
        .and_then(|(_session, window)| {
            window.active_pane().map(|pane| {
                format!(
                    "%window-pane-changed {} %{}",
                    window.id(),
                    pane.id().as_u32()
                )
            })
        });
    broadcast_line(clients, line)
}

fn window_membership_notifications(
    state: &HandlerState,
    event: &LifecycleEvent,
    clients: &[ControlClientSnapshot],
    linked_prefix: &str,
    unlinked_prefix: &str,
) -> Vec<PreparedControlNotification> {
    let Some(window_id) = event_window_id(state, event) else {
        return Vec::new();
    };

    session_scoped_clients(clients)
        .map(|(pid, session_name)| {
            let prefix = if session_contains_window_id(state, session_name, window_id) {
                linked_prefix
            } else {
                unlinked_prefix
            };
            PreparedControlNotification {
                pid,
                line: format!("{prefix} @{window_id}"),
            }
        })
        .collect()
}

fn window_renamed_notifications(
    state: &HandlerState,
    event: &LifecycleEvent,
    clients: &[ControlClientSnapshot],
) -> Vec<PreparedControlNotification> {
    let Some((window_id, window_name)) = event_window_id_and_name(state, event) else {
        return Vec::new();
    };

    session_scoped_clients(clients)
        .map(|(pid, session_name)| {
            let prefix = if session_contains_window_id(state, session_name, window_id) {
                "%window-renamed"
            } else {
                "%unlinked-window-renamed"
            };
            PreparedControlNotification {
                pid,
                line: format!("{prefix} @{window_id} {}", control_arg(&window_name)),
            }
        })
        .collect()
}

fn control_arg(value: &str) -> String {
    octal_escape(value.as_bytes())
}

fn client_session_changed_notifications(
    state: &HandlerState,
    session_name: &SessionName,
    client_name: Option<&str>,
    clients: &[ControlClientSnapshot],
) -> Vec<PreparedControlNotification> {
    let Some(client_name) = client_name else {
        return Vec::new();
    };
    let Some(session) = state.sessions.session(session_name) else {
        return Vec::new();
    };
    let switched_pid = client_name.parse::<u32>().ok();
    let session_identity = session_identity(session);

    session_scoped_clients(clients)
        .map(|(pid, _session_name)| {
            let self_change = switched_pid.is_some_and(|switched_pid| switched_pid == pid);
            let line = if self_change {
                format!("%session-changed {session_identity}")
            } else {
                format!("%client-session-changed {client_name} {session_identity}")
            };
            PreparedControlNotification { pid, line }
        })
        .collect()
}

fn render_layout_change_line(state: &HandlerState, target: &WindowTarget) -> Option<(u32, String)> {
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    if window.panes().is_empty() {
        return None;
    }

    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let mut context = FormatContext::from_session(session).with_window(
        target.window_index(),
        window,
        target.window_index() == active_window,
        Some(target.window_index()) == last_window,
    );
    if let Some(pane) = window.active_pane() {
        context = context.with_window_pane(window, pane);
    }

    let mut runtime = RuntimeFormatContext::new(context)
        .with_state(state)
        .with_session(session)
        .with_window(target.window_index(), window);
    if let Some(pane) = window.active_pane() {
        runtime = runtime.with_pane(pane);
    }

    Some((
        window.id().as_u32(),
        render_runtime_template(LAYOUT_CHANGE_TEMPLATE, &runtime, false),
    ))
}

fn event_window_id(state: &HandlerState, event: &LifecycleEvent) -> Option<u32> {
    event.window_id().or_else(|| {
        event
            .window_target()
            .and_then(|target| resolve_window(state, &target).map(|window| window.id().as_u32()))
    })
}

fn event_window_id_and_name(state: &HandlerState, event: &LifecycleEvent) -> Option<(u32, String)> {
    if let Some(target) = event.window_target() {
        if let Some(window) = resolve_window(state, &target) {
            return Some((
                window.id().as_u32(),
                window.name().unwrap_or_default().to_owned(),
            ));
        }
    }

    Some((event.window_id()?, event.window_name_snapshot()?.to_owned()))
}

fn resolve_window<'a>(state: &'a HandlerState, target: &WindowTarget) -> Option<&'a Window> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
}

fn session_scoped_clients(
    clients: &[ControlClientSnapshot],
) -> impl Iterator<Item = (u32, &SessionName)> + '_ {
    clients.iter().filter_map(|client| {
        client
            .session_name
            .as_ref()
            .map(|session_name| (client.pid, session_name))
    })
}

fn resolve_session_window<'a>(
    state: &'a HandlerState,
    session_name: &SessionName,
    window_index: u32,
) -> Option<(&'a Session, &'a Window)> {
    let session = state.sessions.session(session_name)?;
    let window = session.window_at(window_index)?;
    Some((session, window))
}

fn session_contains_window_id(
    state: &HandlerState,
    session_name: &SessionName,
    window_id: u32,
) -> bool {
    state.sessions.session(session_name).is_some_and(|session| {
        session
            .windows()
            .values()
            .any(|window| window.id().as_u32() == window_id)
    })
}

fn session_identity(session: &Session) -> String {
    format!("{} {}", session.id(), session.name())
}
