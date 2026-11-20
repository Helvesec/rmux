use rmux_core::LifecycleEvent;
use rmux_proto::SessionId;

use super::{
    collect_control_notifications, control_clients_snapshot_locked, deliver_control_notification,
    ActiveControlState, ControlClientIdentity, QueuedLifecycleEvent, RequestHandler,
};
use crate::pane_terminals::HandlerState;

/// An immutable C echo, bound to the recipient registration and attachment that
/// existed at commit. It carries no mutable or authoritative geometry state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct ControlLayoutNotification {
    identity: ControlClientIdentity,
    session_id: Option<SessionId>,
    line: String,
}

pub(in crate::handler) fn freeze_control_layout_notifications(
    state: &HandlerState,
    controls: &ActiveControlState,
    events: &mut [QueuedLifecycleEvent],
) {
    let clients = control_clients_snapshot_locked(state, controls);
    for event in events {
        if !matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }) {
            continue;
        }
        event.control_layout_notifications = Some(
            collect_control_notifications(
                state,
                &event.event,
                event.control_session_identity,
                event.control_window_identity,
                &clients,
            )
            .into_iter()
            .filter_map(|notification| {
                let active = controls.by_pid.get(&notification.pid)?;
                Some(ControlLayoutNotification {
                    identity: ControlClientIdentity::new(notification.pid, active.id),
                    session_id: active.session_id,
                    line: notification.line,
                })
            })
            .collect(),
        );
    }
}

pub(super) fn deliver_control_layout_notifications(
    controls: &mut ActiveControlState,
    notifications: &[ControlLayoutNotification],
) {
    for notification in notifications {
        let pid = notification.identity.requester_pid();
        if controls.by_pid.get(&pid).is_some_and(|active| {
            active.id == notification.identity.control_id()
                && active.session_id == notification.session_id
                && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
        }) {
            deliver_control_notification(controls, pid, notification.line.clone());
        }
    }
}

impl RequestHandler {
    // A queued event retains its window ID and name, but membership belongs to
    // publication time: a later command may already have unlinked that window.
    pub(crate) async fn refresh_control_window_notification(
        &self,
        identity: ControlClientIdentity,
        line: String,
    ) -> String {
        let Some((prefix, arguments)) = line.split_once(' ') else {
            return line;
        };
        let kind = match prefix {
            "%window-add" | "%unlinked-window-add" => "add",
            "%window-close" | "%unlinked-window-close" => "close",
            "%window-renamed" | "%unlinked-window-renamed" => "renamed",
            _ => return line,
        };
        let Some(window_id) = arguments
            .split_whitespace()
            .next()
            .and_then(|value| value.strip_prefix('@'))
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return line;
        };
        let state = self.state.lock().await;
        let controls = self.active_control.lock().await;
        let Some(active) = controls
            .by_pid
            .get(&identity.requester_pid())
            .filter(|active| active.id == identity.control_id())
        else {
            return line;
        };
        let linked = active.session_name.as_ref().is_some_and(|name| {
            state.sessions.session(name).is_some_and(|session| {
                Some(session.id()) == active.session_id
                    && session
                        .windows()
                        .any(|(_, window)| window.id().as_u32() == window_id)
            })
        });
        let unlinked = if linked { "" } else { "unlinked-" };
        format!("%{unlinked}window-{kind} {arguments}")
    }
}
