use rmux_core::formats::{is_known_format_variable_name, FormatVariable, FormatVariables};
use rmux_core::input::mode;
use rmux_core::{Session, Window, WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE};

use crate::hook_runtime::current_hook_format_value;

use super::{bool_string, hostname, server_start_time, RuntimeFormatContext};

impl FormatVariables for RuntimeFormatContext<'_> {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        if variable == FormatVariable::WindowName {
            return self.window_name();
        }
        self.base.format_value(variable)
    }

    fn format_loop(&self, scope: char, body: &str, count_only: bool) -> Option<String> {
        match scope {
            'S' => self.render_session_loop(body, count_only),
            'W' => self.render_window_loop(body, count_only),
            'P' => self.render_pane_loop(body, count_only),
            _ => None,
        }
    }

    fn format_name_exists(&self, scope: Option<char>, name: &str) -> Option<bool> {
        match scope {
            Some('s') => Some(
                rmux_proto::SessionName::new(name)
                    .ok()
                    .and_then(|session_name| {
                        self.session_store
                            .map(|sessions| sessions.contains_session(&session_name))
                    })
                    .unwrap_or(false),
            ),
            None | Some('w') => Some(
                self.session
                    .map(|session| {
                        session.windows().iter().any(|(window_index, window)| {
                            self.rendered_window_name(*window_index, window).as_deref()
                                == Some(name)
                        })
                    })
                    .unwrap_or(false),
            ),
            Some(_) => None,
        }
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        let runtime_value = match name {
            "pane_at_bottom" => self.visible_pane_snapshot().map(|pane| {
                bool_string(self.visible_window_snapshot().is_some_and(|window| {
                    pane.geometry().y() + pane.geometry().rows() >= window.size().rows
                }))
            }),
            "pane_bottom" => self.visible_pane_snapshot().map(|pane| {
                (pane.geometry().y() + pane.geometry().rows())
                    .saturating_sub(1)
                    .to_string()
            }),
            "pane_height" => self
                .visible_pane_snapshot()
                .map(|pane| pane.geometry().rows().to_string()),
            "pane_width" => self
                .visible_pane_snapshot()
                .map(|pane| pane.geometry().cols().to_string()),
            "session_activity_flag" => self.session_flag(WINLINK_ACTIVITY),
            "session_alert" => self.session_alert(),
            "session_alerts" => self.session_alerts(),
            "session_bell_flag" => self.session_flag(WINLINK_BELL),
            "session_height" => (self.hide_session_size || self.session_attached_count() == 0)
                .then(String::new)
                .or_else(|| {
                    self.visible_session_snapshot()
                        .map(|session| session.window().size().rows.to_string())
                }),
            "session_silence_flag" => self.session_flag(WINLINK_SILENCE),
            "session_width" => (self.hide_session_size || self.session_attached_count() == 0)
                .then(String::new)
                .or_else(|| {
                    self.visible_session_snapshot()
                        .map(|session| session.window().size().cols.to_string())
                }),
            "window_activity_flag" => self.window_flag(WINLINK_ACTIVITY),
            "window_bell_flag" => self.window_flag(WINLINK_BELL),
            "window_flags" => self.window_flags(),
            "window_height" => self
                .visible_window_snapshot()
                .map(|window| window.size().rows.to_string()),
            "window_layout" | "window_visible_layout" => self
                .layout_window_snapshot()
                .map(|window| window.layout_dump()),
            "window_linked" => self.window_linked(),
            "window_linked_sessions" => self.window_linked_sessions(),
            "window_linked_sessions_list" => self.window_linked_sessions_list(),
            "window_name" => self.window_name(),
            "window_raw_flags" => Some(self.printable_window_flags(false)),
            "window_silence_flag" => self.window_flag(WINLINK_SILENCE),
            "window_width" => self
                .visible_window_snapshot()
                .map(|window| window.size().cols.to_string()),
            _ => None,
        };
        if let Some(value) = runtime_value {
            return Some(value);
        }
        if let Some(value) = self.base.format_value_by_name(name) {
            return Some(value);
        }
        if let Some(value) = current_hook_format_value(name) {
            return Some(value);
        }

        let value = match name {
            "active_window_index" => self
                .session
                .map(|session| session.active_window_index().to_string()),
            "buffer_full" => self
                .buffer_head()
                .map(|(_, content)| String::from_utf8_lossy(&content).into_owned()),
            "buffer_name" => self.buffer_head().map(|(name, _)| name),
            "buffer_sample" => self.buffer_head().map(|(_, content)| {
                let text = String::from_utf8_lossy(&content);
                text.chars().take(50).collect()
            }),
            "buffer_size" => self
                .buffer_head()
                .map(|(_, content)| content.len().to_string()),
            "client_height" => self.client_size.map(|size| size.rows.to_string()),
            "client_width" => self.client_size.map(|size| size.cols.to_string()),
            "config_files" => Some(String::new()),
            "cursor_x" => self
                .pane_cursor_position()
                .map(|(cursor_x, _)| cursor_x.to_string()),
            "cursor_y" => self
                .pane_cursor_position()
                .map(|(_, cursor_y)| cursor_y.to_string()),
            "history_bytes" => self.pane_history_bytes(),
            "history_limit" => self.pane_history_limit(),
            "history_size" => self.pane_history_size(),
            "host" => hostname(),
            "host_short" => {
                hostname().map(|host| host.split('.').next().unwrap_or_default().to_owned())
            }
            "last_window_index" => self
                .session
                .and_then(|session| session.windows().keys().next_back().copied())
                .map(|value| value.to_string()),
            "next_session_id" => self
                .session_store
                .map(|sessions| sessions.next_session_id().to_string()),
            "pane_at_left" => self.pane.map(|pane| bool_string(pane.geometry().x() == 0)),
            "pane_at_right" => self.pane.map(|pane| {
                bool_string(self.window.is_some_and(|window| {
                    pane.geometry().x() + pane.geometry().cols() >= window.size().cols
                }))
            }),
            "pane_at_top" => self.pane.map(|pane| bool_string(pane.geometry().y() == 0)),
            "alternate_on" => self.pane_alternate_on(),
            "bracket_paste_flag" => self.pane_mode_flag(mode::MODE_BRACKETPASTE),
            "mouse_all_flag" => self.pane_mode_flag(mode::MODE_MOUSE_ALL),
            "mouse_any_flag" => self
                .pane_screen_mode()
                .map(|mode_value| bool_string(mode_value & mode::ALL_MOUSE_MODES != 0)),
            "mouse_button_flag" => self.pane_mode_flag(mode::MODE_MOUSE_BUTTON),
            "mouse_sgr_flag" => self.pane_mode_flag(mode::MODE_MOUSE_SGR),
            "mouse_standard_flag" => self.pane_mode_flag(mode::MODE_MOUSE_STANDARD),
            "mouse_utf8_flag" => self.pane_mode_flag(mode::MODE_MOUSE_UTF8),
            "pane_current_path" | "pane_path" | "pane_start_path" | "session_path" => self
                .pane_current_path()
                .or_else(|| self.environment_value_by_name("PWD"))
                .or_else(|| self.environment_value_by_name("HOME")),
            "pane_current_command" => self.pane_current_command(),
            "pane_dead" => Some(bool_string(self.pane_dead())),
            "pane_dead_signal" => self.pane_dead_signal(),
            "pane_dead_status" => self.pane_dead_status(),
            "pane_dead_time" => self.pane_dead_time(),
            "pane_flags" => self.pane.map(|pane| {
                let mut flags = String::new();
                if self
                    .base
                    .format_value(FormatVariable::PaneActive)
                    .is_some_and(|value| value == "1")
                {
                    flags.push('*');
                }
                if self.window.is_some_and(Window::is_zoomed)
                    && self
                        .window
                        .is_some_and(|window| window.active_pane_index() == pane.index())
                {
                    flags.push('Z');
                }
                flags
            }),
            "pane_format" => Some(bool_string(self.pane.is_some())),
            "pane_in_mode" => Some(bool_string(self.pane_in_mode())),
            "pane_marked" => self.pane_marked(),
            "pane_marked_set" => Some(bool_string(self.marked_pane_set())),
            "pane_last" => self.pane.map(|pane| {
                bool_string(
                    self.window
                        .and_then(Window::last_pane_index)
                        .is_some_and(|last| last == pane.index()),
                )
            }),
            "pane_left" => self.pane.map(|pane| pane.geometry().x().to_string()),
            "pane_mode" => self.pane_mode_name(),
            "pane_pid" => self.pane_pid(),
            "pane_right" => self.pane.map(|pane| {
                (pane.geometry().x() + pane.geometry().cols())
                    .saturating_sub(1)
                    .to_string()
            }),
            "pane_search_string" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.pane_search_string),
            "pane_tty" => self.pane_tty(),
            "pane_title" => self.pane_title(),
            "pane_top" => self.pane.map(|pane| pane.geometry().y().to_string()),
            "pane_zoomed_flag" => Some(bool_string(self.window.is_some_and(Window::is_zoomed))),
            "pid" => Some(std::process::id().to_string()),
            "scroll_position" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.scroll_position.to_string()),
            "rectangle_toggle" => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.rectangle_toggle)),
            "copy_cursor_x" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.cursor_x.to_string()),
            "copy_cursor_y" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.cursor_y.to_string()),
            "selection_start_x" => self.pane_copy_mode_summary().and_then(|summary| {
                summary
                    .selection_start
                    .map(|position| position.x.to_string())
            }),
            "selection_start_y" => self.pane_copy_mode_summary().and_then(|summary| {
                summary
                    .selection_start
                    .map(|position| position.y.to_string())
            }),
            "selection_end_x" => self
                .pane_copy_mode_summary()
                .and_then(|summary| summary.selection_end.map(|position| position.x.to_string())),
            "selection_end_y" => self
                .pane_copy_mode_summary()
                .and_then(|summary| summary.selection_end.map(|position| position.y.to_string())),
            "selection_active" => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.selection_active)),
            "selection_present" => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.selection_present)),
            "selection_mode" => self.pane_copy_mode_summary().and_then(|summary| {
                summary
                    .selection_mode
                    .map(|selection_mode| selection_mode.as_str().to_owned())
            }),
            "search_present" => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.search_present)),
            "search_timed_out" => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.search_timed_out)),
            "search_count" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.search_count.to_string()),
            "search_count_partial" => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.search_count_partial)),
            "search_match" => self
                .pane_copy_mode_summary()
                .and_then(|summary| summary.search_match),
            "copy_cursor_word" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.copy_cursor_word),
            "copy_cursor_line" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.copy_cursor_line),
            "copy_cursor_hyperlink" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.copy_cursor_hyperlink),
            "top_line_time" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.top_line_time.to_string()),
            "server_sessions" => self
                .session_store
                .map(|sessions| sessions.len().to_string()),
            "session_active" => Some(bool_string(self.session.is_some())),
            "session_created" => self.session.map(|session| session.created_at().to_string()),
            "session_format" => Some(bool_string(
                self.session.is_some() && self.window.is_none() && self.pane.is_none(),
            )),
            "session_group" => Some(
                self.session_group_name()
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
            ),
            "session_group_attached" => Some(self.session_attached_count().to_string()),
            "session_group_attached_list" => Some(
                (self.session_attached_count() > 0)
                    .then(|| self.session_name().map(ToString::to_string))
                    .flatten()
                    .unwrap_or_default(),
            ),
            "session_group_list" => Some(
                self.session_group_members()
                    .into_iter()
                    .map(|session_name| session_name.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            "session_group_many_attached" => Some(bool_string(self.session_attached_count() > 1)),
            "session_group_size" => Some(self.session_group_members().len().to_string()),
            "session_grouped" => Some(bool_string(self.session_group_name().is_some())),
            "session_id" => self.session.map(|session| format!("${}", session.id())),
            "session_last_attached" => self
                .session
                .and_then(Session::last_attached_at)
                .map(|timestamp| timestamp.to_string()),
            "session_many_attached" => Some(bool_string(self.session_attached_count() > 1)),
            "session_marked" => Some(bool_string(self.session_marked())),
            "socket_path" => Some(String::new()),
            "start_time" => Some(server_start_time().to_string()),
            "uid" => std::env::var("UID").ok(),
            "user" => std::env::var("USER").ok(),
            "version" => Some(env!("CARGO_PKG_VERSION").to_owned()),
            "window_active_clients" => Some(
                if self
                    .base
                    .format_value(FormatVariable::WindowActive)
                    .is_some_and(|value| value == "1")
                {
                    self.session_attached_count().to_string()
                } else {
                    "0".to_owned()
                },
            ),
            "window_active_sessions" => Some(bool_string(
                self.base
                    .format_value(FormatVariable::WindowActive)
                    .is_some_and(|value| value == "1"),
            )),
            "window_bigger" => Some("0".to_owned()),
            "window_end_flag" => self.window_index.map(|window_index| {
                bool_string(
                    self.session
                        .and_then(|session| session.windows().keys().next_back().copied())
                        .is_some_and(|last| last == window_index),
                )
            }),
            "window_format" => Some(bool_string(self.window.is_some() && self.pane.is_none())),
            "window_marked_flag" => Some(bool_string(self.window_marked())),
            "window_offset_x" | "window_offset_y" => Some("0".to_owned()),
            "window_start_flag" => self.window_index.map(|window_index| {
                bool_string(
                    self.session
                        .and_then(|session| session.windows().keys().next().copied())
                        .is_some_and(|first| first == window_index),
                )
            }),
            "window_zoomed_flag" => Some(bool_string(self.window.is_some_and(Window::is_zoomed))),
            _ => None,
        };

        if let Some(value) = value {
            return Some(value);
        }

        if let Some(value) = self.option_value_by_name(name) {
            return Some(value);
        }

        if let Some(value) = self.environment_value_by_name(name) {
            return Some(value);
        }

        if is_known_format_variable_name(name) {
            return Some(String::new());
        }

        None
    }
}
