use rmux_core::formats::{FormatVariable, FormatVariableKey, FormatVariables};
use rmux_core::input::mode;
use rmux_core::{Window, WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE};

use crate::host_name::local_hostname;

use super::dispatch::runtime_dispatch_typed;
use super::{bool_string, server_start_time, RuntimeFormatContext};
use runtime_value::{IntoRuntimeValue, RuntimeValue};
use window_stack::session_stack;

mod format_variables;
mod pane_values;
mod runtime_value;
mod search;
mod window_stack;

impl RuntimeFormatContext<'_> {
    runtime_dispatch_typed! {
        fn pre_base_registered_format_value(&self, key: FormatVariableKey) => key;
        coverage covers_pre_base_registered_format_value;
        {
            PaneFloatingFlag => self.floating_format_value(FormatVariableKey::PaneFloatingFlag),
            PaneAtBottom => self.pane_edge_format_value(key),
            PaneBottom => self.visible_pane_geometry().map(|geometry| {
                (geometry.y() + geometry.rows())
                    .saturating_sub(1)
                    .to_string()
            }),
            PaneHeight => self
                .visible_pane_geometry()
                .map(|geometry| geometry.rows().to_string()),
            PaneWidth => self
                .visible_pane_geometry()
                .map(|geometry| geometry.cols().to_string()),
            SessionActivityFlag => self.session_flag(WINLINK_ACTIVITY),
            SessionAlert => self.session_alert(),
            SessionAlerts => self.session_alerts(),
            SessionBellFlag => self.session_flag(WINLINK_BELL),
            // A hidden session's size, and the size of one nobody is attached
            // to, are this server's own empty answer, given before the base
            // could speak: they stop here, as they always did.
            SessionHeight => (self.hide_session_size || self.session_attached_count() == 0)
                .then_some(RuntimeValue::CompatibilityEmpty)
                .or_else(|| {
                    self.visible_session_snapshot().map(|session| {
                        RuntimeValue::Found(session.window().size().rows.to_string())
                    })
                }),
            SessionSilenceFlag => self.session_flag(WINLINK_SILENCE),
            SessionWidth => (self.hide_session_size || self.session_attached_count() == 0)
                .then_some(RuntimeValue::CompatibilityEmpty)
                .or_else(|| {
                    self.visible_session_snapshot().map(|session| {
                        RuntimeValue::Found(session.window().size().cols.to_string())
                    })
                }),
            WindowActivityFlag => self.window_flag(WINLINK_ACTIVITY),
            WindowBellFlag => self.window_flag(WINLINK_BELL),
            // The flags are computed from the base context's own window_active
            // and window_last_flag, so a caller that supplies a window there and
            // no runtime window still computes a real `*` or `-`: that is a
            // value and stays one. Only an empty computation with no runtime
            // window at all is the answer this server has always given for an
            // unavailable target, and it stops there. A window that is really
            // there keeps its flags even when they are empty.
            WindowFlags => Some(match self.window_flags() {
                Some(flags) if !flags.is_empty() || self.window.is_some() => {
                    RuntimeValue::Found(flags)
                }
                _ => RuntimeValue::CompatibilityEmpty,
            }),
            WindowHeight => self
                .visible_window_snapshot()
                .map(|window| window.size().rows.to_string()),
            WindowLayout => self
                .layout_window_snapshot()
                .map(|window| window.layout_dump()),
            WindowVisibleLayout => self
                .visible_window_snapshot()
                .map(|window| window.visible_layout_dump()),
            WindowLinked => self.window_linked(),
            WindowLinkedSessions => self.window_linked_sessions(),
            WindowLinkedSessionsList => self.window_linked_sessions_list(),
            WindowName => self.window_name(),
            WindowRawFlags => Some({
                let flags = self.printable_window_flags(false);
                if !flags.is_empty() || self.window.is_some() {
                    RuntimeValue::Found(flags)
                } else {
                    RuntimeValue::CompatibilityEmpty
                }
            }),
            WindowSilenceFlag => self.window_flag(WINLINK_SILENCE),
            WindowWidth => self
                .visible_window_snapshot()
                .map(|window| window.size().cols.to_string()),
        }
    }

    runtime_dispatch_typed! {
        fn registered_runtime_format_value(&self, key: FormatVariableKey) => key;
        coverage covers_registered_runtime_format_value;
        {
            ActiveWindowIndex => self
                .session
                .map(|session| session.active_window_index().to_string()),
            AlternateSavedX => self
                .pane_alternate_saved_cursor()
                .map(|(saved_x, _)| saved_x.to_string()),
            AlternateSavedY => self
                .pane_alternate_saved_cursor()
                .map(|(_, saved_y)| saved_y.to_string()),
            BufferFull => self
                .buffer_head()
                .map(|(_, content)| String::from_utf8_lossy(&content).into_owned()),
            BufferModeFormat => Some("#{t/p:buffer_created}: #{buffer_sample}".to_owned()),
            BufferName => self.buffer_head().map(|(name, _)| name),
            // The sample says which string it collects into: the dispatcher's
            // answer is no longer one single type, so this one no longer reads
            // it from the return type. The fifty characters are unchanged.
            BufferSample => self.buffer_head().map(|(_, content)| {
                let text = String::from_utf8_lossy(&content);
                text.chars().take(50).collect::<String>()
            }),
            BufferSize => self
                .buffer_head()
                .map(|(_, content)| content.len().to_string()),
            ClientModeFormat => Some("#{t/p:client_activity}: session #{session_name}".to_owned()),
            ClientHeight => self.client_size.map(|size| size.rows.to_string()),
            ClientWidth => self.client_size.map(|size| size.cols.to_string()),
            Command => Some("display-message".to_owned()),
            // A server's configuration text is a value, even when empty;
            // no state at all keeps the old compatibility empty answer.
            ConfigFiles => Some(
                self.state
                    .map(|state| state.startup_config_files().to_owned())
                    .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            CursorCharacter => self.pane_cursor_character(),
            CursorFlag => self.pane_mode_flag(mode::MODE_CURSOR),
            CursorX => self
                .pane_cursor_position()
                .map(|(cursor_x, _)| cursor_x.to_string()),
            CursorY => self
                .pane_cursor_position()
                .map(|(_, cursor_y)| cursor_y.to_string()),
            DefaultShell => self.resolved_default_shell(),
            HistoryBytes => self.pane_history_bytes(),
            HistoryAllBytes => self.pane_history_all_bytes(),
            HistoryLimit => self.pane_history_limit(),
            HistorySize => self.pane_history_size(),
            Host => local_hostname(),
            HostShort => {
                local_hostname().map(|host| host.split('.').next().unwrap_or_default().to_owned())
            },
            InsertFlag => self.pane_mode_flag(mode::MODE_INSERT),
            KeypadCursorFlag => self.pane_mode_flag(mode::MODE_KCURSOR),
            KeypadFlag => self.pane_mode_flag(mode::MODE_KKEYPAD),
            LastWindowIndex => self
                .session
                .and_then(|session| session.windows().next_back().map(|(index, _)| index))
                .map(|value| value.to_string()),
            NextSessionId => self
                .session_store
                .map(|sessions| sessions.next_session_id().to_string()),
            OriginFlag => self.pane_mode_flag(mode::MODE_ORIGIN),
            PaneAtLeft | PaneAtRight | PaneAtTop => self.pane_edge_format_value(key),
            AlternateOn => self.pane_alternate_on(),
            BracketPasteFlag => self.pane_mode_flag(mode::MODE_BRACKETPASTE),
            MouseAllFlag => self.pane_mode_flag(mode::MODE_MOUSE_ALL),
            MouseAnyFlag => self
                .pane_screen_mode()
                .map(|mode_value| bool_string(mode_value & mode::ALL_MOUSE_MODES != 0)),
            MouseButtonFlag => self.pane_mode_flag(mode::MODE_MOUSE_BUTTON),
            MouseSgrFlag => self.pane_mode_flag(mode::MODE_MOUSE_SGR),
            MouseStandardFlag => self.pane_mode_flag(mode::MODE_MOUSE_STANDARD),
            MouseUtf8Flag => self.pane_mode_flag(mode::MODE_MOUSE_UTF8),
            PaneCurrentPath => self
                .pane_current_path()
                .or_else(|| self.environment_value_by_name("PWD"))
                .or_else(|| self.environment_value_by_name("HOME")),
            SessionPath => self
                .session
                .and_then(|session| session.cwd())
                .map(|path| path.to_string_lossy().into_owned())
                .or_else(|| self.environment_value_by_name("PWD"))
                .or_else(|| self.environment_value_by_name("HOME")),
            PanePath => Some(self.pane_announced_path().map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found)),
            PaneCurrentCommand => self.pane_current_command(),
            PaneDead => Some(bool_string(self.pane_dead())),
            PaneDeadSignal => self.pane_dead_signal(),
            PaneDeadStatus => self.pane_dead_status(),
            PaneDeadTime => self.pane_dead_time(),
            PaneBg => self.pane_style_colour(false),
            PaneActivity => self.pane.map(|pane| pane.activity_at().to_string()),
            PaneCreated => self.pane.map(|pane| pane.created_at().to_string()),
            PaneFg => self.pane_style_colour(true),
            PaneFlags => self.pane.map(|pane| {
                let mut flags = String::new();
                if self
                    .base
                    .format_value(FormatVariable::PaneActive)
                    .is_some_and(|value| value == "1")
                {
                    flags.push('*');
                }
                if self
                    .window
                    .and_then(Window::last_pane_index)
                    .is_some_and(|last| last == pane.index())
                {
                    flags.push('-');
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
            PaneFormat => Some(bool_string(self.pane.is_some())),
            PaneInMode => Some(bool_string(self.pane_in_mode())),
            PaneInputOff => Some(bool_string(self.pane.is_some_and(|pane| {
                self.state
                    .is_some_and(|state| state.pane_input_is_disabled(pane.id()))
            }))),
            PaneMarked => self.pane_marked(),
            PaneMarkedSet => Some(bool_string(self.marked_pane_set())),
            PaneLast => self.pane.map(|pane| {
                bool_string(
                    self.window
                        .and_then(Window::last_pane_index)
                        .is_some_and(|last| last == pane.index()),
                )
            }),
            PaneLeft => self
                .visible_pane_geometry()
                .map(|geometry| geometry.x().to_string()),
            PaneMode => self.pane_mode_name(),
            PaneLifecycleGeneration | PaneGeneration => self.pane_lifecycle_generation(),
            PaneLifecycleRevision | PaneRevision => self.pane_lifecycle_revision(),
            PaneOutputSequence => self.pane_output_sequence(),
            SessionProgramStatusJson => self.session_program_status_json(),
            PaneProgramStatus => self.pane_program_status("state"),
            PaneProgramStatusJson => self.pane_program_status("json"),
            PaneProgramStatusApp => self.pane_program_status("app"),
            PaneProgramStatusId => self.pane_program_status("id"),
            PaneProgramStatusKind => self.pane_program_status("kind"),
            PaneProgramStatusProgress => self.pane_program_status("progress"),
            PaneProgramStatusTitle => self.pane_program_status("title"),
            PaneProgramStatusMessage => self.pane_program_status("message"),
            PanePid => self.pane_pid(),
            PanePipe => self.pane_pipe(),
            PaneRight => self.visible_pane_geometry().map(|geometry| {
                (geometry.x() + geometry.cols())
                    .saturating_sub(1)
                    .to_string()
            }),
            PaneSearchString => self
                .pane_copy_mode_summary()
                .map(|summary| summary.pane_search_string),
            PaneStartCommand => self.pane_start_command(),
            PaneStartPath => self
                .pane_start_path()
                .or_else(|| self.environment_value_by_name("PWD"))
                .or_else(|| self.environment_value_by_name("HOME")),
            PaneSynchronized => self.pane_synchronized(),
            PaneTabs => self.pane_tabs(),
            PaneTty => self.pane_tty(),
            PaneTitle => self.pane_title(),
            PaneTop => self
                .visible_pane_geometry()
                .map(|geometry| geometry.y().to_string()),
            PaneUnseenChanges => self.pane_unseen_changes(),
            PaneX | PaneY | PaneZ => self.pane_position_format_value(key),
            PaneZoomedFlag => self.pane_zoomed_flag(),
            Pid => Some(std::process::id().to_string()),
            ScrollRegionLower => self.pane_scroll_region().map(|(_, lower)| lower.to_string()),
            ScrollRegionUpper => self.pane_scroll_region().map(|(upper, _)| upper.to_string()),
            ScrollPosition => self
                .pane_copy_mode_summary()
                .map(|summary| summary.scroll_position.to_string()),
            CopyPosition => self
                .copy_mode_position_values()
                .map(|(position, _)| position.to_string()),
            CopyPositionLimit => self
                .copy_mode_position_values()
                .map(|(_, limit)| limit.to_string()),
            RectangleToggle => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.rectangle_toggle)),
            CopyCursorX => self
                .pane_copy_mode_summary()
                .map(|summary| summary.cursor_x.to_string()),
            CopyCursorY => self
                .pane_copy_mode_summary()
                .map(|summary| summary.cursor_y.to_string()),
            SelectionStartX => self.pane_copy_mode_summary().and_then(|summary| {
                summary
                    .selection_start
                    .map(|position| position.x.to_string())
            }),
            SelectionStartY => self.pane_copy_mode_summary().and_then(|summary| {
                summary
                    .selection_start
                    .map(|position| position.y.to_string())
            }),
            SelectionEndX => self
                .pane_copy_mode_summary()
                .and_then(|summary| summary.selection_end.map(|position| position.x.to_string())),
            SelectionEndY => self
                .pane_copy_mode_summary()
                .and_then(|summary| summary.selection_end.map(|position| position.y.to_string())),
            SelectionActive => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.selection_active)),
            SelectionPresent => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.selection_present)),
            SelectionMode => self.pane_copy_mode_summary().and_then(|summary| {
                summary
                    .selection_mode
                    .map(|selection_mode| selection_mode.as_str().to_owned())
            }),
            SearchPresent => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.search_present)),
            SearchTimedOut => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.search_timed_out)),
            SearchCount => self
                .pane_copy_mode_summary()
                .map(|summary| summary.search_count.to_string()),
            SearchCountPartial => self
                .pane_copy_mode_summary()
                .map(|summary| bool_string(summary.search_count_partial)),
            SearchMatch => self
                .pane_copy_mode_summary()
                .and_then(|summary| summary.search_match),
            CopyCursorWord => self
                .pane_copy_mode_summary()
                .map(|summary| summary.copy_cursor_word),
            CopyCursorLine => self
                .pane_copy_mode_summary()
                .map(|summary| summary.copy_cursor_line),
            CopyCursorHyperlink => self
                .pane_copy_mode_summary()
                .map(|summary| summary.copy_cursor_hyperlink),
            TopLineTime => self
                .pane_copy_mode_summary()
                .map(|summary| summary.top_line_time.to_string()),
            ServerSessions => self
                .session_store
                .map(|sessions| sessions.len().to_string()),
            SessionActive => Some(bool_string(self.session.is_some())),
            SessionActivity => self.session.map(|session| session.activity_at().to_string()),
            SessionCreated => self.session.map(|session| session.created_at().to_string()),
            SessionFormat => Some(bool_string(
                self.session.is_some() && self.window.is_none() && self.pane.is_none(),
            )),
            // Where there is no group at all, the empty answer is this
            // server's own and stops before any option or environment: tmux
            // answers NULL there (format.c:2568-2615).
            SessionGroup => Some(
                self.session_group_name()
                    .map(|name| name.to_string())
                    .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            SessionGroupAttached => Some(
                self.session_group_attached_count()
                    .map(|count| count.to_string())
                    .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            SessionGroupAttachedList => Some(
                self.session_group_attached_clients()
                    .map(|clients| {
                        clients
                            .iter()
                            .map(|client| client.name())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|names| !names.is_empty())
                    .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            SessionGroupList => Some(
                Some(
                    self.session_group_list_names()
                        .into_iter()
                        .map(|session_name| session_name.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .filter(|names| !names.is_empty())
                .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            SessionGroupManyAttached => Some(
                self.session_group_attached_count()
                    .map(|count| bool_string(count > 1))
                    .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            SessionGroupSize => Some(
                self.session_group_name()
                    .map(|_| self.session_group_members().len().to_string())
                    .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found),
            ),
            SessionGrouped => Some(bool_string(self.session_group_name().is_some())),
            SessionAttached => Some(self.session_attached_count().to_string()),
            SessionId => self.session.map(|session| session.id().to_string()),
            SessionLastAttached => self
                .session
                .and_then(|session| session.last_attached_at())
                .map(|timestamp| timestamp.to_string()),
            SessionManyAttached => Some(bool_string(self.session_attached_count() > 1)),
            SessionMarked => Some(bool_string(self.session_marked())),
            SessionStack => self.session.map(session_stack),
            // Nobody supplied a socket path here: this is the contextless
            // stub's own empty answer, not a path that happens to be empty.
            SocketPath => Some(RuntimeValue::CompatibilityEmpty),
            StartTime => Some(server_start_time().to_string()),
            TreeModeFormat => Some("#{?pane_format,#{?pane_marked,#[reverse],}#{pane_current_command}#{?pane_active,*,}#{?pane_marked,M,}#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",},#{?window_format,#{?window_marked_flag,#[reverse],}#{window_name}#{window_flags}#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",},#{session_windows} windows#{?session_grouped, (group #{session_group}: #{session_group_list}),}#{?session_attached, (attached),}}}".to_owned()),
            Uid => Some(crate::server_access::current_owner_uid().to_string()),
            User => current_user_name(),
            Version => Some("3.4".to_owned()),
            // `None` says the inputs never reached this runtime and keeps the
            // answer that had; `Some` of a complete list is joined once. A
            // complete list that really holds nobody renders empty, as it
            // always has, but it is not a name that was found: the primary
            // starts that value at NULL and writes it only when its buffer has
            // a size (`format.c:797-831`), so the empty stops here and a path
            // modifier gets nothing from it. A name the caller supplies itself
            // is a value and keeps its earlier precedence.
            WindowActiveClientsList => self.window_active_client_names().map(|names| {
                let names = names.join(",");
                if names.is_empty() {
                    RuntimeValue::CompatibilityEmpty
                } else {
                    RuntimeValue::Found(names)
                }
            }),
            WindowActiveClients => Some(match self.window_active_client_count() {
                Some(count) => count.to_string(),
                // No target window, no session store or no client list ever
                // reached this runtime, so no global count can be supplied and
                // the historical local calculation is kept exactly as it was -
                // including the public 0 a bare runtime prints. The primary
                // answers nothing at all when it has no winlink
                // (`format.c:780-781`); that internal bound is deliberately
                // left open here rather than turned into a measured zero.
                None => {
                    if self
                        .base
                        .format_value(FormatVariable::WindowActive)
                        .is_some_and(|value| value == "1")
                    {
                        self.session_attached_count().to_string()
                    } else {
                        "0".to_owned()
                    }
                }
            }),
            WindowActiveSessions => self
                .window_active_session_names()
                .map(|sessions| sessions.len().to_string()),
            // The oracle prints the count even when it is zero and answers
            // nothing at all for the list in that case, so an empty set keeps the
            // list absent rather than empty.
            WindowActiveSessionsList => self
                .window_active_session_names()
                .filter(|sessions| !sessions.is_empty())
                .map(|sessions| {
                    sessions
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                }),
            WindowActivity => self.window.map(|window| window.activity_at().to_string()),
            WindowBigger => Some(bool_string(self.window_viewport_offset().is_some())),
            WindowCellHeight => Some("32".to_owned()),
            WindowCellWidth => Some("16".to_owned()),
            WindowEndFlag => self.window_index.map(|window_index| {
                bool_string(
                    self.session
                        .and_then(|session| session.windows().next_back().map(|(index, _)| index))
                        .is_some_and(|last| last == window_index),
                )
            }),
            WindowFormat => Some(bool_string(self.window.is_some() && self.pane.is_none())),
            WindowCreated => self.window.map(|window| window.created_at().to_string()),
            WindowMarkedFlag => Some(bool_string(self.window_marked())),
            WindowOffsetX | WindowOffsetY => Some(self.window_viewport_offset()
                .map(|(x, y)| if key == FormatVariableKey::WindowOffsetX { x } else { y }.to_string())
                .map_or(RuntimeValue::CompatibilityEmpty, RuntimeValue::Found)),
            WindowStackIndex => self.window_stack_index(),
            WindowStartFlag => self.window_index.map(|window_index| {
                bool_string(
                    self.session
                        .and_then(|session| session.windows().next().map(|(index, _)| index))
                        .is_some_and(|first| first == window_index),
                )
            }),
            WindowZoomedFlag => Some(bool_string(self.window.is_some_and(Window::is_zoomed))),
            WrapFlag => self.pane_mode_flag(mode::MODE_WRAP),
        }
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
#[path = "pzoom1_tests.rs"]
mod pzoom1_tests;

#[cfg(test)]
#[path = "was1_format_tests.rs"]
mod was1_format_tests;

#[cfg(test)]
#[path = "wstack1_tests.rs"]
mod wstack1_tests;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::user_name_from_env;
    use super::RuntimeFormatContext;
    use rmux_core::formats::{render_template, FormatContext, FormatVariables};

    #[test]
    fn config_files_present_empty_keeps_path_presence() {
        let mut state = crate::pane_terminals::HandlerState::default();
        state.set_startup_config_files(&[]);
        let present = RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
        assert_eq!(
            present.format_value_by_name("config_files").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{config_files}", &present), "");
        // tmux 3.7b with `-f ''` holds a present empty `config_files`: its
        // short name and its folder are both `.`, unlike an absent value.
        assert_eq!(render_template("#{d:config_files}", &present), ".");
        assert_eq!(render_template("#{b:config_files}", &present), ".");
        assert_eq!(render_template("#{b;d:config_files}", &present), ".");

        state.environment.set(
            rmux_proto::ScopeSelector::Global,
            "config_files".to_owned(),
            "/shadow/ignored.conf".to_owned(),
        );
        let shadowed = RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
        assert_eq!(
            render_template("#{config_files}|#{d:config_files}", &shadowed),
            "|."
        );

        state.set_startup_config_files(&["/work/rmux.conf".to_owned()]);
        let nonempty = RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
        assert_eq!(
            render_template("#{config_files}", &nonempty),
            "/work/rmux.conf"
        );
        assert_eq!(render_template("#{d:config_files}", &nonempty), "/work");
        assert_eq!(render_template("#{b:config_files}", &nonempty), "rmux.conf");
        assert_eq!(render_template("#{b;d:config_files}", &nonempty), ".");

        let absent = RuntimeFormatContext::new(FormatContext::new());
        assert_eq!(
            absent.format_value_by_name("config_files").as_deref(),
            Some("")
        );
        assert_eq!(
            render_template(
                "#{config_files}|#{d:config_files}|#{b:config_files}",
                &absent
            ),
            "||"
        );
        let supplied =
            RuntimeFormatContext::new(FormatContext::new().with_named_value("config_files", ""))
                .with_state(&state);
        assert_eq!(
            render_template("#{config_files}|#{d:config_files}", &supplied),
            "|."
        );
    }

    /// tmux returns NULL for `cursor_flag` when the format tree has no pane
    /// (`format.c:1727-1737`), which the registered name renders as the empty
    /// value. A context with no state and no pane must give that, and never the
    /// constant `1` the old branch returned whatever the pane did.
    ///
    /// This is the primary source contract plus this server's existing
    /// known-name fallback; no contextless tmux CLI cell is claimed for it.
    #[test]
    fn cursor_flag_is_empty_without_a_pane_context() {
        let context = RuntimeFormatContext::new(FormatContext::new());

        assert_eq!(
            context.format_value_by_name("cursor_flag").as_deref(),
            Some("")
        );
    }

    /// A value that is really there and holds nothing is not the same thing as
    /// a name nobody answered.
    ///
    /// The registry answers a known name nobody supplied with the historical
    /// empty value (`has_known_empty_fallback`), and that empty is a
    /// compatibility stop: its folder must stay empty, exactly as it was. A
    /// value someone really supplied - even an empty one - is a value, so its
    /// folder is the current directory, which is what the engine answered for an
    /// option really set to the empty string (FDIR2's twelve closed readings).
    ///
    /// The whole OSC 2 route of a real pane title is proved by this lot's native
    /// journey, not claimed from this guard.
    #[test]
    fn present_empty_path_values_are_not_registry_fallbacks() {
        let bare = RuntimeFormatContext::new(FormatContext::new());

        // A bare runtime: these known names keep their historical empty answer,
        // and that answer is a stop, not a found value.
        assert_eq!(
            bare.format_value_by_name("client_name").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{client_name}", &bare), "");
        assert_eq!(render_template("#{d:client_name}", &bare), "");
        assert_eq!(
            bare.format_value_by_name("cursor_flag").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{cursor_flag}", &bare), "");
        assert_eq!(render_template("#{d:cursor_flag}", &bare), "");

        // A supplied empty value IS there, so it has a folder.
        let supplied =
            RuntimeFormatContext::new(FormatContext::new().with_named_value("client_name", ""));
        assert_eq!(
            supplied.format_value_by_name("client_name").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{client_name}", &supplied), "");
        assert_eq!(render_template("#{d:client_name}", &supplied), ".");

        let titled =
            RuntimeFormatContext::new(FormatContext::new().with_named_value("pane_title", ""));
        assert_eq!(
            titled.format_value_by_name("pane_title").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{d:pane_title}", &titled), ".");

        // A supplied path keeps the folder it always had.
        let path = RuntimeFormatContext::new(
            FormatContext::new().with_named_value("pane_title", "/work/logs///app.log"),
        );
        assert_eq!(render_template("#{d:pane_title}", &path), "/work/logs");

        // A name nobody answers to is absent everywhere.
        assert_eq!(bare.format_value_by_name("no_such_variable_at_all"), None);
        assert_eq!(render_template("#{d:no_such_variable_at_all}", &bare), "");
    }

    /// The arms that manufacture an empty answer keep the public answer and the
    /// precedence they always had.
    ///
    /// Each of these is a legacy empty produced before a later tier could speak:
    /// no session group, no configuration text, no viewport, a hidden or
    /// detached session's dimensions, an unavailable window target and the
    /// context-free socket path. Their public value stays the empty string,
    /// their folder stays empty, and - this is the point - an environment
    /// variable of the same name must NOT take over, because the stop comes
    /// first.
    #[test]
    fn synthetic_empty_path_inputs_keep_their_old_public_precedence() {
        let bare = RuntimeFormatContext::new(FormatContext::new());

        for name in [
            "session_group",
            "session_group_attached",
            "session_group_many_attached",
            "session_group_size",
            "session_group_list",
            "session_group_attached_list",
            "config_files",
            "socket_path",
            "window_offset_x",
            "window_offset_y",
            "window_flags",
            "window_raw_flags",
        ] {
            assert_eq!(
                bare.format_value_by_name(name).as_deref(),
                Some(""),
                "{name} must keep its historical empty public value"
            );
            assert_eq!(
                render_template(&format!("#{{d:{name}}}"), &bare),
                "",
                "{name} is a compatibility empty, so it has no folder"
            );
        }

        // A hidden or detached session's dimensions stop the same way.
        let detached = RuntimeFormatContext::new(FormatContext::new().with_session_attached(0));
        assert_eq!(
            detached.format_value_by_name("session_width").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{d:session_width}", &detached), "");
        assert_eq!(
            detached.format_value_by_name("session_height").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{d:session_height}", &detached), "");

        // And an environment variable of the same name does not take over: the
        // stop precedes the environment, as it always did.
        let mut state = crate::pane_terminals::HandlerState::default();
        state.environment.set(
            rmux_proto::ScopeSelector::Global,
            "session_group".to_owned(),
            "/shadow/should-not-win".to_owned(),
        );
        state.environment.set(
            rmux_proto::ScopeSelector::Global,
            "session_width".to_owned(),
            "/shadow/should-not-win".to_owned(),
        );
        let shadowed = RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
        assert_eq!(
            shadowed.format_value_by_name("session_group").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{d:session_group}", &shadowed), "");
        let shadowed_size =
            RuntimeFormatContext::new(FormatContext::new().with_session_attached(0))
                .with_state(&state);
        assert_eq!(
            shadowed_size
                .format_value_by_name("session_width")
                .as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{d:session_width}", &shadowed_size), "");

        // A value someone really supplied is not one of those stops, empty or
        // not, and a nonempty one answers exactly as it always did.
        let supplied =
            RuntimeFormatContext::new(FormatContext::new().with_named_value("session_group", ""));
        assert_eq!(
            supplied.format_value_by_name("session_group").as_deref(),
            Some("")
        );
        assert_eq!(render_template("#{d:session_group}", &supplied), ".");
        let named = RuntimeFormatContext::new(
            FormatContext::new().with_named_value("session_group", "alpha"),
        );
        assert_eq!(render_template("#{session_group}", &named), "alpha");
        assert_eq!(render_template("#{d:session_group}", &named), ".");
    }

    /// tmux returns NULL for `pane_tabs` when the format tree has no pane
    /// (`format.c:981-1006`), which renders as the empty value. A context with no
    /// state and no pane must give that, and never a grid rebuilt from a width it
    /// does not have either.
    #[test]
    fn pane_tabs_is_empty_without_a_pane_context() {
        let context = RuntimeFormatContext::new(FormatContext::new());

        assert_eq!(
            context.format_value_by_name("pane_tabs").as_deref(),
            Some("")
        );
    }

    /// tmux returns NULL for both names when the format tree has no pane
    /// (`format.c:2458-2473`), which renders as the empty value. A context with
    /// no state and no pane must keep the registry's existing empty contract for
    /// both of them, not a constant.
    #[test]
    fn the_scroll_region_names_are_empty_without_a_pane_context() {
        let context = RuntimeFormatContext::new(FormatContext::new());

        assert_eq!(
            context
                .format_value_by_name("scroll_region_upper")
                .as_deref(),
            Some("")
        );
        assert_eq!(
            context
                .format_value_by_name("scroll_region_lower")
                .as_deref(),
            Some("")
        );
    }

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
        let values = HashMap::from([("USERNAME", "rmux-user")]);

        assert_eq!(
            user_name_from_env(lookup(&values)).as_deref(),
            Some("rmux-user")
        );
    }

    #[test]
    fn user_name_ignores_empty_values() {
        let values = HashMap::from([("USER", ""), ("USERNAME", "rmux-user")]);

        assert_eq!(
            user_name_from_env(lookup(&values)).as_deref(),
            Some("rmux-user")
        );
    }
}
