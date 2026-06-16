use regex::RegexBuilder;
use rmux_core::formats::{is_known_format_variable_name, FormatVariable, FormatVariables};
use rmux_core::input::mode;
use rmux_core::{colour_to_string, GridRenderOptions, ScreenCaptureRange, Style, StyleCell};
use rmux_core::{Session, Window, WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE};
use rmux_proto::OptionName;

use crate::hook_runtime::current_hook_format_value;

use crate::host_name::local_hostname;

use super::{bool_string, server_start_time, RuntimeFormatContext};

impl RuntimeFormatContext<'_> {
    fn pane_history_all_bytes(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let stats = self.state?.pane_history_stats(session.name(), pane.id())?;
        Some(stats.all_bytes)
    }

    fn pane_pipe(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let window_index = self.window_index?;
        Some(bool_string(self.state.is_some_and(|state| {
            state.pane_has_pipe(session.name(), window_index, pane.id())
        })))
    }

    fn pane_synchronized(&self) -> Option<String> {
        let session = self.session?;
        let window_index = self.window_index?;
        Some(bool_string(
            self.options?.resolve_for_window(
                session.name(),
                window_index,
                OptionName::SynchronizePanes,
            ) == Some("on"),
        ))
    }

    fn pane_style_colour(&self, foreground: bool) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let window_index = self.window_index?;
        let options = self.options?;
        let mut style = Style::default();
        let base = StyleCell::default();
        for option in [OptionName::WindowStyle, OptionName::WindowActiveStyle] {
            if option == OptionName::WindowActiveStyle
                && self
                    .window
                    .is_some_and(|window| window.active_pane_index() != pane.index())
            {
                continue;
            }
            let Some(value) =
                options.resolve_for_pane(session.name(), window_index, pane.index(), option)
            else {
                continue;
            };
            if value.is_empty() || value == "default" {
                continue;
            }
            let _ = style.parse_in_place(&base, value);
        }
        Some(colour_to_string(if foreground {
            style.cell.fg
        } else {
            style.cell.bg
        }))
    }

    fn window_stack_index(&self) -> Option<String> {
        let session = self.session?;
        let window_index = self.window_index?;
        window_stack(session)
            .iter()
            .position(|candidate| *candidate == window_index)
            .map(|index| index.to_string())
    }

    fn pane_content_search(&self, options: &str, pattern: &str) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let screen = self
            .state?
            .pane_copy_mode_render_screen(session.name(), pane.id())
            .or_else(|| self.state?.pane_render_screen(session.name(), pane.id()))?;
        let lines = screen.capture_transcript_lines_independent(
            ScreenCaptureRange::default(),
            GridRenderOptions::default(),
        );
        Some(search_visible_lines(&lines, options, pattern).to_string())
    }

    fn pane_tabs(&self) -> Option<String> {
        let width = self.visible_pane_geometry()?.cols();
        Some(
            (8..width)
                .step_by(8)
                .map(|stop| stop.to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

impl FormatVariables for RuntimeFormatContext<'_> {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        if let Some(value) = self.runtime_format_value(variable) {
            return Some(value);
        }
        self.base.format_value(variable)
    }

    fn format_loop(
        &self,
        scope: char,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        match scope {
            'S' => self.render_session_loop(body, current_body, count_only),
            'W' => self.render_window_loop(body, current_body, count_only),
            'P' => self.render_pane_loop(body, current_body, count_only),
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

    fn format_search(&self, options: &str, pattern: &str) -> Option<String> {
        self.pane_content_search(options, pattern)
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        if let Some(variable) = FormatVariable::from_name(name) {
            if let Some(value) = self.runtime_format_value(variable) {
                return Some(value);
            }
        }

        let runtime_value = match name {
            "pane_at_bottom" => self.visible_pane_geometry().map(|geometry| {
                bool_string(
                    self.visible_window_snapshot()
                        .is_some_and(|window| geometry.y() + geometry.rows() >= window.size().rows),
                )
            }),
            "pane_bottom" => self.visible_pane_geometry().map(|geometry| {
                (geometry.y() + geometry.rows())
                    .saturating_sub(1)
                    .to_string()
            }),
            "pane_height" => self
                .visible_pane_geometry()
                .map(|geometry| geometry.rows().to_string()),
            "pane_width" => self
                .visible_pane_geometry()
                .map(|geometry| geometry.cols().to_string()),
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
            "alternate_saved_x" | "alternate_saved_y" => Some(u32::MAX.to_string()),
            "buffer_full" => self
                .buffer_head()
                .map(|(_, content)| String::from_utf8_lossy(&content).into_owned()),
            "buffer_mode_format" => Some("#{t/p:buffer_created}: #{buffer_sample}".to_owned()),
            "buffer_name" => self.buffer_head().map(|(name, _)| name),
            "buffer_sample" => self.buffer_head().map(|(_, content)| {
                let text = String::from_utf8_lossy(&content);
                text.chars().take(50).collect()
            }),
            "buffer_size" => self
                .buffer_head()
                .map(|(_, content)| content.len().to_string()),
            "client_mode_format" => Some("#{t/p:client_activity}: session #{session_name}".to_owned()),
            "client_height" => self.client_size.map(|size| size.rows.to_string()),
            "client_width" => self.client_size.map(|size| size.cols.to_string()),
            "command" => Some("display-message".to_owned()),
            "config_files" => Some("/dev/null".to_owned()),
            "cursor_character" => Some(" ".to_owned()),
            "cursor_flag" => Some("1".to_owned()),
            "cursor_x" => self
                .pane_cursor_position()
                .map(|(cursor_x, _)| cursor_x.to_string()),
            "cursor_y" => self
                .pane_cursor_position()
                .map(|(_, cursor_y)| cursor_y.to_string()),
            "default-shell" => self.resolved_default_shell(),
            "history_bytes" => self.pane_history_bytes(),
            "history_all_bytes" => self.pane_history_all_bytes(),
            "history_limit" => self.pane_history_limit(),
            "history_size" => self.pane_history_size(),
            "host" => local_hostname(),
            "host_short" => {
                local_hostname().map(|host| host.split('.').next().unwrap_or_default().to_owned())
            }
            "insert_flag" | "keypad_cursor_flag" | "keypad_flag" | "origin_flag" => {
                Some("0".to_owned())
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
            "pane_current_path" | "session_path" => self
                .pane_current_path()
                .or_else(|| self.environment_value_by_name("PWD"))
                .or_else(|| self.environment_value_by_name("HOME")),
            "pane_path" => Some(String::new()),
            "pane_current_command" => self.pane_current_command(),
            "pane_dead" => Some(bool_string(self.pane_dead())),
            "pane_dead_signal" => self.pane_dead_signal(),
            "pane_dead_status" => self.pane_dead_status(),
            "pane_dead_time" => self.pane_dead_time(),
            "pane_bg" => self.pane_style_colour(false),
            "pane_fg" => self.pane_style_colour(true),
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
            "pane_input_off" => Some("0".to_owned()),
            "pane_marked" => self.pane_marked(),
            "pane_marked_set" => Some(bool_string(self.marked_pane_set())),
            "pane_last" => self.pane.map(|pane| {
                bool_string(
                    self.window
                        .and_then(Window::last_pane_index)
                        .is_some_and(|last| last == pane.index()),
                )
            }),
            "pane_left" => self
                .visible_pane_geometry()
                .map(|geometry| geometry.x().to_string()),
            "pane_mode" => self.pane_mode_name(),
            "pane_lifecycle_generation" | "pane_generation" => self.pane_lifecycle_generation(),
            "pane_lifecycle_revision" | "pane_revision" => self.pane_lifecycle_revision(),
            "pane_output_sequence" => self.pane_output_sequence(),
            "pane_pid" => self.pane_pid(),
            "pane_pipe" => self.pane_pipe(),
            "pane_right" => self.visible_pane_geometry().map(|geometry| {
                (geometry.x() + geometry.cols())
                    .saturating_sub(1)
                    .to_string()
            }),
            "pane_search_string" => self
                .pane_copy_mode_summary()
                .map(|summary| summary.pane_search_string),
            "pane_start_command" => self.pane_start_command(),
            "pane_start_path" => self
                .pane_start_path()
                .or_else(|| self.environment_value_by_name("PWD"))
                .or_else(|| self.environment_value_by_name("HOME")),
            "pane_synchronized" => self.pane_synchronized(),
            "pane_tabs" => self.pane_tabs(),
            "pane_tty" => self.pane_tty(),
            "pane_title" => self.pane_title(),
            "pane_top" => self
                .visible_pane_geometry()
                .map(|geometry| geometry.y().to_string()),
            "pane_unseen_changes" => Some("0".to_owned()),
            "pane_zoomed_flag" => Some(String::new()),
            "pid" => Some(std::process::id().to_string()),
            "scroll_region_lower" => self
                .visible_window_snapshot()
                .map(|window| window.size().rows.saturating_sub(1).to_string()),
            "scroll_region_upper" => Some("0".to_owned()),
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
            "session_activity" => self.session.map(|session| session.activity_at().to_string()),
            "session_created" => self.session.map(|session| session.created_at().to_string()),
            "session_format" => Some(bool_string(
                self.session.is_some() && self.window.is_none() && self.pane.is_none(),
            )),
            "session_group" => Some(
                self.session_group_name()
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
            ),
            "session_group_attached" => Some(
                self.session_group_name()
                    .map(|_| self.session_attached_count().to_string())
                    .unwrap_or_default(),
            ),
            "session_group_attached_list" => Some(
                (self.session_group_name().is_some() && self.session_attached_count() > 0)
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
            "session_group_many_attached" => Some(
                self.session_group_name()
                    .map(|_| bool_string(self.session_attached_count() > 1))
                    .unwrap_or_default(),
            ),
            "session_group_size" => Some(
                self.session_group_name()
                    .map(|_| self.session_group_members().len().to_string())
                    .unwrap_or_default(),
            ),
            "session_grouped" => Some(bool_string(self.session_group_name().is_some())),
            "session_attached" => Some(self.session_attached_count().to_string()),
            "session_id" => self.session.map(|session| session.id().to_string()),
            "session_last_attached" => self
                .session
                .and_then(Session::last_attached_at)
                .map(|timestamp| timestamp.to_string()),
            "session_many_attached" => Some(bool_string(self.session_attached_count() > 1)),
            "session_marked" => Some(bool_string(self.session_marked())),
            "session_stack" => self.session.map(session_stack),
            "socket_path" => Some(String::new()),
            "start_time" => Some(server_start_time().to_string()),
            "tree_mode_format" => Some("#{?pane_format,#{?pane_marked,#[reverse],}#{pane_current_command}#{?pane_active,*,}#{?pane_marked,M,}#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",},#{?window_format,#{?window_marked_flag,#[reverse],}#{window_name}#{window_flags}#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",},#{session_windows} windows#{?session_grouped, (group #{session_group}: #{session_group_list}),}#{?session_attached, (attached),}}}".to_owned()),
            "uid" => Some(crate::server_access::current_owner_uid().to_string()),
            "user" => current_user_name(),
            "version" => Some("3.4".to_owned()),
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
            "window_active_sessions_list" => Some(
                self.base
                    .format_value(FormatVariable::WindowActive)
                    .filter(|value| value == "1")
                    .and_then(|_| self.session_name().map(ToString::to_string))
                    .unwrap_or_default(),
            ),
            "window_activity" => self.session.map(|session| session.activity_at().to_string()),
            "window_bigger" => Some("0".to_owned()),
            "window_cell_height" => Some("32".to_owned()),
            "window_cell_width" => Some("16".to_owned()),
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
            "window_stack_index" => self.window_stack_index(),
            "window_start_flag" => self.window_index.map(|window_index| {
                bool_string(
                    self.session
                        .and_then(|session| session.windows().keys().next().copied())
                        .is_some_and(|first| first == window_index),
                )
            }),
            "window_zoomed_flag" => Some(bool_string(self.window.is_some_and(Window::is_zoomed))),
            "wrap_flag" => Some("1".to_owned()),
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

fn search_visible_lines(lines: &[Vec<u8>], options: &str, pattern: &str) -> usize {
    if pattern.is_empty() {
        return 0;
    }

    let ignore_case = options.contains('i');
    if options.contains('r') {
        let Ok(regex) = RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
        else {
            return 0;
        };
        return search_lines(lines, |line| regex.is_match(line));
    }

    if ignore_case {
        let pattern = pattern.to_lowercase();
        return search_lines(lines, |line| line.to_lowercase().contains(&pattern));
    }

    search_lines(lines, |line| line.contains(pattern))
}

fn search_lines(lines: &[Vec<u8>], mut matches: impl FnMut(&str) -> bool) -> usize {
    lines
        .iter()
        .map(|line| String::from_utf8_lossy(line))
        .position(|line| matches(line.as_ref()))
        .map_or(0, |index| index + 1)
}

fn session_stack(session: &Session) -> String {
    window_stack(session)
        .into_iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn window_stack(session: &Session) -> Vec<u32> {
    let mut stack = Vec::with_capacity(session.windows().len());
    push_existing_window(&mut stack, session, session.active_window_index());
    if let Some(last_window) = session.last_window_index() {
        push_existing_window(&mut stack, session, last_window);
    }
    for window_index in session.windows().keys().copied() {
        push_existing_window(&mut stack, session, window_index);
    }
    stack
}

fn push_existing_window(stack: &mut Vec<u32>, session: &Session, window_index: u32) {
    if session.window_at(window_index).is_some() && !stack.contains(&window_index) {
        stack.push(window_index);
    }
}

fn current_user_name() -> Option<String> {
    user_name_from_env(|name| std::env::var(name).ok())
}

fn user_name_from_env(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<String> {
    lookup("USER")
        .filter(|value| !value.is_empty())
        .or_else(|| lookup("USERNAME").filter(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::user_name_from_env;

    fn lookup<'a>(values: &'a HashMap<&str, &str>) -> impl FnMut(&str) -> Option<String> + 'a {
        |name| values.get(name).map(|value| (*value).to_owned())
    }

    #[test]
    fn user_name_prefers_unix_user_when_present() {
        let values = HashMap::from([("USER", "alice"), ("USERNAME", "windows-alice")]);

        assert_eq!(
            user_name_from_env(lookup(&values)).as_deref(),
            Some("alice")
        );
    }

    #[test]
    fn user_name_falls_back_to_windows_username() {
        let values = HashMap::from([("USERNAME", "shadow")]);

        assert_eq!(
            user_name_from_env(lookup(&values)).as_deref(),
            Some("shadow")
        );
    }

    #[test]
    fn user_name_ignores_empty_values() {
        let values = HashMap::from([("USER", ""), ("USERNAME", "shadow")]);

        assert_eq!(
            user_name_from_env(lookup(&values)).as_deref(),
            Some("shadow")
        );
    }
}
