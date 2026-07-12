use rmux_core::{
    AlertFlags, LifecycleEvent, WINDOW_ACTIVITY, WINDOW_BELL, WINDOW_SILENCE, WINLINK_ACTIVITY,
    WINLINK_BELL, WINLINK_SILENCE,
};
use rmux_proto::{OptionName, SessionId, SessionName, WindowId, WindowTarget};
use tokio::task::JoinHandle;

use super::{QueuedLifecycleEvent, RequestHandler};
use crate::pane_io::AttachControl;
use crate::renderer;

#[path = "handler_alerts/automatic_names.rs"]
mod automatic_names;
#[path = "handler_alerts/pane_alert_coalescer.rs"]
mod pane_alert_coalescer;
#[path = "handler_alerts/pane_events.rs"]
mod pane_events;
#[path = "handler_alerts/queue.rs"]
mod queue;
#[path = "handler_alerts/show_messages.rs"]
mod show_messages;
#[path = "handler_alerts/silence_timers.rs"]
mod silence_timers;

pub(in crate::handler) use pane_alert_coalescer::PaneAlertCoalescer;

const SHOW_MESSAGES_TEMPLATE: &str = "#{t/p:message_time}: #{message_text}";

#[derive(Debug)]
pub(super) struct SilenceTimerState {
    session_id: SessionId,
    window_id: WindowId,
    generation: u64,
    deadline: tokio::time::Instant,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertKind {
    Bell,
    Activity,
    Silence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertAction {
    None,
    Any,
    Current,
    Other,
}

impl AlertAction {
    fn applies(self, is_current: bool) -> bool {
        match self {
            Self::None => false,
            Self::Any => true,
            Self::Current => is_current,
            Self::Other => !is_current,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisualMode {
    Off,
    On,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct AlertPlan {
    session_id: SessionId,
    refresh_session: bool,
    send_bell: bool,
    show_message: bool,
    message_text: String,
    lifecycle_event: Option<QueuedLifecycleEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalSummary {
    attach_pid: u32,
    session_name: SessionName,
    cols: u16,
    rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JobSummary {
    attach_pid: u32,
    session_name: SessionName,
}

impl AlertKind {
    const fn window_flag(self) -> AlertFlags {
        match self {
            Self::Bell => WINDOW_BELL,
            Self::Activity => WINDOW_ACTIVITY,
            Self::Silence => WINDOW_SILENCE,
        }
    }

    const fn winlink_flag(self) -> AlertFlags {
        match self {
            Self::Bell => WINLINK_BELL,
            Self::Activity => WINLINK_ACTIVITY,
            Self::Silence => WINLINK_SILENCE,
        }
    }

    const fn monitor_option(self) -> OptionName {
        match self {
            Self::Bell => OptionName::MonitorBell,
            Self::Activity => OptionName::MonitorActivity,
            Self::Silence => OptionName::MonitorSilence,
        }
    }

    const fn visual_option(self) -> OptionName {
        match self {
            Self::Bell => OptionName::VisualBell,
            Self::Activity => OptionName::VisualActivity,
            Self::Silence => OptionName::VisualSilence,
        }
    }

    const fn action_option(self) -> OptionName {
        match self {
            Self::Bell => OptionName::BellAction,
            Self::Activity => OptionName::ActivityAction,
            Self::Silence => OptionName::SilenceAction,
        }
    }

    const fn lifecycle_event(self, target: WindowTarget) -> LifecycleEvent {
        match self {
            Self::Bell => LifecycleEvent::AlertBell { target },
            Self::Activity => LifecycleEvent::AlertActivity { target },
            Self::Silence => LifecycleEvent::AlertSilence { target },
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Bell => "Bell",
            Self::Activity => "Activity",
            Self::Silence => "Silence",
        }
    }
}

impl RequestHandler {
    pub(super) async fn clear_session_alerts_on_focus(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> bool {
        let changed = {
            let mut state = self.state.lock().await;
            let Some(session) = state.sessions.session_mut(session_name) else {
                return false;
            };
            session.clear_all_winlink_alert_flags(window_index)
        };

        if changed {
            self.refresh_attached_session(session_name).await;
        }
        changed
    }

    async fn show_alert_message(&self, plan: &AlertPlan) {
        let (overlay_frame, clear_frame, duration) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session_by_id(plan.session_id) else {
                return;
            };
            let session_name = session.name().clone();
            let overlay_frame = {
                let mut frame = renderer::render_display_panes_clear(session, &state.options);
                frame.extend_from_slice(
                    renderer::render_status_message(session, &state.options, &plan.message_text)
                        .as_slice(),
                );
                frame
            };
            let clear_frame = renderer::render_display_panes_clear(session, &state.options);
            (
                overlay_frame,
                clear_frame,
                display_time(&state.options, &session_name),
            )
        };

        // Log for a still-live session even when no client can display the overlay,
        // matching tmux's server_add_message behavior without crossing an identity reuse.
        {
            let mut state = self.state.lock().await;
            if state.sessions.session_by_id(plan.session_id).is_none() {
                return;
            }
            state.add_message(plan.message_text.clone());
        }
        self.send_alert_overlay(plan.session_id, overlay_frame, clear_frame, duration)
            .await;
    }

    async fn send_alert_overlay(
        &self,
        session_id: SessionId,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
        duration: std::time::Duration,
    ) {
        let state = self.state.lock().await;
        let Some(session_name) = state
            .sessions
            .session_by_id(session_id)
            .map(|session| session.name().clone())
        else {
            return;
        };
        let _ = self
            .send_attached_overlay(&session_name, overlay_frame, clear_frame, duration)
            .await;
    }

    async fn send_attached_bell(&self, session_id: SessionId) {
        let state = self.state.lock().await;
        let Some(session_name) = state
            .sessions
            .session_by_id(session_id)
            .map(|session| session.name().clone())
        else {
            return;
        };
        let mut active_attach = self.active_attach.lock().await;
        active_attach.by_pid.retain(|_, active| {
            if active.session_name != session_name {
                return true;
            }
            active
                .control_tx
                .send(AttachControl::Write(vec![0x07]))
                .is_ok()
        });
    }

    async fn refresh_alert_plan_session(&self, session_id: SessionId) {
        let session_name = {
            let state = self.state.lock().await;
            state
                .sessions
                .session_by_id(session_id)
                .map(|session| session.name().clone())
        };
        if let Some(session_name) = session_name {
            self.refresh_attached_session(&session_name).await;
        }
    }
}

fn alert_flags_enabled(
    options: &rmux_core::OptionStore,
    session_name: &SessionName,
    window_index: u32,
    flags: AlertFlags,
) -> bool {
    (flags.contains(WINDOW_BELL)
        && flag_option_is_on(options.resolve_for_window(
            session_name,
            window_index,
            OptionName::MonitorBell,
        )))
        || (flags.contains(WINDOW_ACTIVITY)
            && flag_option_is_on(options.resolve_for_window(
                session_name,
                window_index,
                OptionName::MonitorActivity,
            )))
        || (flags.contains(WINDOW_SILENCE)
            && monitor_silence_seconds(options, session_name, window_index) != 0)
}

fn alert_kind_enabled(
    options: &rmux_core::OptionStore,
    session_name: &SessionName,
    window_index: u32,
    kind: AlertKind,
) -> bool {
    match kind {
        AlertKind::Bell | AlertKind::Activity => flag_option_is_on(options.resolve_for_window(
            session_name,
            window_index,
            kind.monitor_option(),
        )),
        AlertKind::Silence => monitor_silence_seconds(options, session_name, window_index) != 0,
    }
}

fn monitor_silence_seconds(
    options: &rmux_core::OptionStore,
    session_name: &SessionName,
    window_index: u32,
) -> u64 {
    options
        .resolve_for_window(session_name, window_index, OptionName::MonitorSilence)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

fn flag_option_is_on(value: Option<&str>) -> bool {
    matches!(value, Some("on"))
}

fn alert_action(
    options: &rmux_core::OptionStore,
    session_name: &SessionName,
    option: OptionName,
) -> AlertAction {
    match options.resolve(Some(session_name), option).unwrap_or("any") {
        "none" => AlertAction::None,
        "current" => AlertAction::Current,
        "other" => AlertAction::Other,
        _ => AlertAction::Any,
    }
}

fn visual_mode(
    options: &rmux_core::OptionStore,
    session_name: &SessionName,
    option: OptionName,
) -> VisualMode {
    match options.resolve(Some(session_name), option).unwrap_or("off") {
        "on" => VisualMode::On,
        "both" => VisualMode::Both,
        _ => VisualMode::Off,
    }
}

fn display_time(
    options: &rmux_core::OptionStore,
    session_name: &SessionName,
) -> std::time::Duration {
    std::time::Duration::from_millis(
        options
            .resolve(Some(session_name), OptionName::DisplayTime)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(750),
    )
}
