use rmux_core::formats::{FormatVariable, FormatVariables};
use rmux_core::{AlertFlags, Window, WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE};
use rmux_proto::SessionName;

use super::{bool_string, RuntimeFormatContext};

impl RuntimeFormatContext<'_> {
    pub(super) fn session_group_name(&self) -> Option<SessionName> {
        let session = self.session?;
        self.session_store
            .and_then(|store| store.session_group_name(session.name()).cloned())
            .or_else(|| session.group_name().cloned())
    }

    pub(super) fn session_group_members(&self) -> Vec<SessionName> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        if self.session_group_name().is_none() {
            return Vec::new();
        }
        self.session_store
            .map(|store| store.session_group_members(session.name()))
            .unwrap_or_default()
    }

    pub(super) fn session_attached_count(&self) -> usize {
        self.base
            .format_value(FormatVariable::SessionAttached)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0)
    }

    pub(super) fn window_flags(&self) -> Option<String> {
        Some(self.printable_window_flags(true))
    }

    pub(super) fn window_linked(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        Some(bool_string(
            self.state?
                .window_link_count(session_name, window_index)
                .saturating_sub(1)
                > 0,
        ))
    }

    pub(super) fn window_linked_sessions(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        Some(
            self.state?
                .window_linked_session_count(session_name, window_index)
                .to_string(),
        )
    }

    pub(super) fn window_linked_sessions_list(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        Some(
            self.state?
                .window_linked_sessions_list(session_name, window_index)
                .into_iter()
                .map(|session_name| session_name.to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    fn window_alert_flags(&self) -> AlertFlags {
        self.session
            .zip(self.window_index)
            .map(|(session, window_index)| session.winlink_alert_flags(window_index))
            .unwrap_or_else(AlertFlags::empty)
    }

    pub(super) fn printable_window_flags(&self, escape_activity: bool) -> String {
        let active = self
            .base
            .format_value(FormatVariable::WindowActive)
            .is_some_and(|value| value == "1");
        let last = self
            .base
            .format_value(FormatVariable::WindowLastFlag)
            .is_some_and(|value| value == "1");
        let zoomed = self.window.is_some_and(Window::is_zoomed);
        let alerts = self.window_alert_flags();

        let mut flags = String::new();
        if alerts.contains(WINLINK_ACTIVITY) {
            if escape_activity {
                flags.push_str("##");
            } else {
                flags.push('#');
            }
        }
        if alerts.contains(WINLINK_BELL) {
            flags.push('!');
        }
        if alerts.contains(WINLINK_SILENCE) {
            flags.push('~');
        }
        if active {
            flags.push('*');
        } else if last {
            flags.push('-');
        }
        if zoomed {
            flags.push('Z');
        }
        flags
    }

    pub(super) fn session_alert(&self) -> Option<String> {
        let session = self.session?;
        let flags = session.session_alert_flags();
        let mut value = String::new();
        if flags.contains(WINLINK_ACTIVITY) {
            value.push('#');
        }
        if flags.contains(WINLINK_BELL) {
            value.push('!');
        }
        if flags.contains(WINLINK_SILENCE) {
            value.push('~');
        }
        Some(value)
    }

    pub(super) fn session_alerts(&self) -> Option<String> {
        let session = self.session?;
        let alerts = session
            .alerted_window_indexes()
            .into_iter()
            .filter_map(|window_index| {
                let flags = session.winlink_alert_flags(window_index);
                if flags.is_empty() {
                    return None;
                }

                let mut value = window_index.to_string();
                if flags.contains(WINLINK_ACTIVITY) {
                    value.push('#');
                }
                if flags.contains(WINLINK_BELL) {
                    value.push('!');
                }
                if flags.contains(WINLINK_SILENCE) {
                    value.push('~');
                }
                Some(value)
            })
            .collect::<Vec<_>>();
        Some(alerts.join(","))
    }

    pub(super) fn session_flag(&self, flag: AlertFlags) -> Option<String> {
        self.session
            .map(|session| bool_string(session.session_alert_flags().contains(flag)))
    }

    pub(super) fn window_flag(&self, flag: AlertFlags) -> Option<String> {
        self.window_index
            .map(|_| bool_string(self.window_alert_flags().contains(flag)))
    }
}
