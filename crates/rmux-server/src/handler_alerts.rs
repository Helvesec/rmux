use rmux_core::{
    AlertFlags, LifecycleEvent, WINDOW_ACTIVITY, WINDOW_BELL, WINDOW_SILENCE, WINLINK_ACTIVITY,
    WINLINK_BELL, WINLINK_SILENCE,
};
use rmux_proto::{OptionName, SessionName, WindowTarget};
use tokio::task::JoinHandle;

use super::RequestHandler;
use crate::pane_io::{AttachControl, PaneAlertCallback, PaneAlertEvent};
use crate::renderer;

#[path = "handler_alerts/automatic_names.rs"]
mod automatic_names;
#[path = "handler_alerts/show_messages.rs"]
mod show_messages;
#[path = "handler_alerts/silence_timers.rs"]
mod silence_timers;

const SHOW_MESSAGES_TEMPLATE: &str = "#{t/p:message_time}: #{message_text}";

#[derive(Debug)]
pub(super) struct SilenceTimerState {
    generation: u64,
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
struct AlertPlan {
    session_name: SessionName,
    refresh_session: bool,
    send_bell: bool,
    show_message: bool,
    message_text: String,
    lifecycle_event: Option<LifecycleEvent>,
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
    pub(super) fn pane_alert_callback(&self) -> PaneAlertCallback {
        let handler = self.downgrade();
        let runtime = tokio::runtime::Handle::current();
        std::sync::Arc::new(move |event: PaneAlertEvent| {
            let Some(handler) = handler.upgrade() else {
                return;
            };
            runtime.spawn(async move {
                handler.handle_pane_alert_event(event).await;
            });
        })
    }

    pub(super) async fn alerts_queue_window(&self, target: WindowTarget, flags: AlertFlags) {
        let mut state = self.state.lock().await;
        let alerts_enabled = alert_flags_enabled(
            &state.options,
            target.session_name(),
            target.window_index(),
            flags,
        );
        // Only reset the silence timer on activity/bell, not when silence itself fires.
        let reset_silence = !flags.contains(WINDOW_SILENCE);
        let monitor_silence = if reset_silence {
            monitor_silence_seconds(&state.options, target.session_name(), target.window_index())
        } else {
            0
        };
        {
            let Some(session) = state.sessions.session_mut(target.session_name()) else {
                return;
            };
            let Some(window) = session.window_at_mut(target.window_index()) else {
                return;
            };

            // Clear pending silence on any non-silence queue (activity resets the idle clock).
            if reset_silence {
                window.clear_alert_flags(WINDOW_SILENCE);
            }
            window.queue_alerts(flags);
            if !window.alerts_queued() && alerts_enabled {
                window.set_alerts_queued(true);
                let handler = self.clone();
                let queued_target = target.clone();
                tokio::spawn(async move {
                    handler.process_queued_alerts(queued_target).await;
                });
            }
        }
        drop(state);

        if reset_silence {
            self.configure_silence_timer(target, monitor_silence);
        }
    }

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

    pub(super) async fn handle_pane_alert_event(&self, event: PaneAlertEvent) {
        let Some((target, refresh_for_inactive_pane_output)) = ({
            let state = self.state.lock().await;
            let Some(runtime_session_name) = state.resolve_pane_event_runtime_session(
                &event.session_name,
                event.pane_id,
                event.generation,
            ) else {
                return;
            };
            let Some(pane_target) =
                state.pane_target_for_runtime_pane(&runtime_session_name, event.pane_id)
            else {
                return;
            };
            let window_index = pane_target.window_index();
            let refresh_for_inactive_pane_output = state
                .sessions
                .session(pane_target.session_name())
                .is_some_and(|session| {
                    session.active_window_index() != window_index
                        || session.active_pane_id() != Some(event.pane_id)
                });
            Some((
                WindowTarget::with_window(pane_target.session_name().clone(), window_index),
                refresh_for_inactive_pane_output,
            ))
        }) else {
            return;
        };
        self.refresh_automatic_window_name_for_window_target(&target)
            .await;
        if refresh_for_inactive_pane_output {
            self.refresh_attached_session(target.session_name()).await;
        }
        // Combine activity + bell into a single queue call when both are present.
        // Bells are flag-based (not counted), so one queue call is sufficient.
        let flags = if event.bell_count > 0 {
            WINDOW_ACTIVITY.union(WINDOW_BELL)
        } else {
            WINDOW_ACTIVITY
        };
        self.alerts_queue_window(target, flags).await;
    }

    async fn process_queued_alerts(&self, target: WindowTarget) {
        let queued = {
            let mut state = self.state.lock().await;
            let Some(session) = state.sessions.session_mut(target.session_name()) else {
                return;
            };
            let Some(window) = session.window_at_mut(target.window_index()) else {
                return;
            };
            let queued = window.take_alert_flags();
            window.set_alerts_queued(false);
            queued
        };

        if queued.contains(WINDOW_BELL) {
            self.process_alert_kind(target.clone(), AlertKind::Bell)
                .await;
        }
        if queued.contains(WINDOW_ACTIVITY) {
            self.process_alert_kind(target.clone(), AlertKind::Activity)
                .await;
        }
        if queued.contains(WINDOW_SILENCE) {
            self.process_alert_kind(target, AlertKind::Silence).await;
        }
    }

    async fn process_alert_kind(&self, target: WindowTarget, kind: AlertKind) {
        let session_name = target.session_name().clone();
        let attached_count = self.attached_count(&session_name).await;
        let plan = {
            let mut state = self.state.lock().await;
            let is_enabled =
                alert_kind_enabled(&state.options, &session_name, target.window_index(), kind);
            if !is_enabled {
                return;
            }
            let action = alert_action(&state.options, &session_name, kind.action_option());
            let visual = visual_mode(&state.options, &session_name, kind.visual_option());

            let Some(session) = state.sessions.session_mut(&session_name) else {
                return;
            };
            let is_current = session.active_window_index() == target.window_index();
            let winlink_flag = kind.winlink_flag();
            let existing_flags = session.winlink_alert_flags(target.window_index());
            if matches!(kind, AlertKind::Activity | AlertKind::Silence)
                && existing_flags.contains(winlink_flag)
            {
                return;
            }

            let refresh_session = if !is_current || attached_count == 0 {
                session.add_winlink_alert_flags(target.window_index(), winlink_flag)
            } else {
                false
            };
            let action_applies = action.applies(is_current);
            let message_text = if is_current {
                format!("{} in current window", kind.label())
            } else {
                format!("{} in window {}", kind.label(), target.window_index())
            };

            AlertPlan {
                session_name: session_name.clone(),
                refresh_session,
                send_bell: action_applies && matches!(visual, VisualMode::Off | VisualMode::Both),
                show_message: action_applies && !matches!(visual, VisualMode::Off),
                message_text,
                lifecycle_event: action_applies.then(|| kind.lifecycle_event(target.clone())),
            }
        };

        if let Some(event) = &plan.lifecycle_event {
            self.emit(event.clone()).await;
        }
        if plan.send_bell {
            self.send_attached_bell(&plan.session_name).await;
        }
        if plan.show_message {
            self.show_alert_message(&plan).await;
        }
        if plan.refresh_session {
            self.refresh_attached_session(&plan.session_name).await;
        }
    }

    async fn show_alert_message(&self, plan: &AlertPlan) {
        let (overlay_frame, clear_frame, duration) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(&plan.session_name) else {
                return;
            };
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
                display_time(&state.options, &plan.session_name),
            )
        };

        // Log the message unconditionally (tmux always calls server_add_message),
        // then attempt overlay delivery to attached clients.
        {
            let mut state = self.state.lock().await;
            state.add_message(plan.message_text.clone());
        }
        self.send_attached_overlay(&plan.session_name, overlay_frame, clear_frame, duration)
            .await;
    }

    async fn send_attached_bell(&self, session_name: &SessionName) {
        let mut active_attach = self.active_attach.lock().await;
        active_attach.by_pid.retain(|_, active| {
            if &active.session_name != session_name {
                return true;
            }
            active
                .control_tx
                .send(AttachControl::Write(vec![0x07]))
                .is_ok()
        });
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
