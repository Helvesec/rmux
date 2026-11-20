use rmux_core::formats::{FormatContext, FormatVariables};

use super::RuntimeFormatContext;
use super::{bool_string, render_runtime_template};

impl RuntimeFormatContext<'_> {
    pub(super) fn render_session_loop(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        let mut expand = |template: &str, variables: &dyn FormatVariables| {
            render_runtime_template(template, variables, false)
        };
        self.render_session_loop_with_expand(body, current_body, count_only, &mut expand)
    }

    pub(super) fn render_session_loop_with_expand(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
        expand: &mut dyn FnMut(&str, &dyn FormatVariables) -> String,
    ) -> Option<String> {
        let state = self.state?;
        let sessions = self.session_store?;
        if count_only {
            return Some(sessions.len().to_string());
        }

        let mut sessions = sessions
            .iter()
            .map(|session| (session.name(), session))
            .collect::<Vec<_>>();
        sessions.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        let total = sessions.len();
        let mut rendered = String::new();
        for (position, (session_name, session)) in sessions.into_iter().enumerate() {
            let active = self
                .session_name()
                .is_some_and(|current| current == session_name);
            let active_window_index = session.active_window_index();
            let active_window = session.window();
            let is_last = position + 1 == total;
            let attached_count = if active {
                self.session_attached_count()
            } else {
                0
            };
            let mut context = FormatContext::new()
                .with_managed_session(session)
                .with_session_attached(attached_count)
                .with_window(active_window_index, active_window, true, false)
                .with_named_value("loop_last_flag", bool_string(is_last));
            if let Some(pane) = active_window.active_pane() {
                context = context.with_window_pane(active_window, pane);
            }
            let runtime = self.inherit_client_loop_context(
                RuntimeFormatContext::new(context)
                    .with_state(state)
                    .with_session(session)
                    .with_window(active_window_index, active_window),
            );
            let runtime = if let Some(pane) = active_window.active_pane() {
                runtime.with_pane(pane)
            } else {
                runtime
            };
            rendered.push_str(&expand(loop_body(body, current_body, active), &runtime));
        }
        Some(rendered)
    }

    pub(super) fn render_window_loop(
        &self,
        arguments: &[String],
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        let mut expand = |template: &str, variables: &dyn FormatVariables| {
            render_runtime_template(template, variables, false)
        };
        self.render_window_loop_with_expand(arguments, body, current_body, count_only, &mut expand)
    }

    pub(super) fn render_window_loop_with_expand(
        &self,
        arguments: &[String],
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
        expand: &mut dyn FnMut(&str, &dyn FormatVariables) -> String,
    ) -> Option<String> {
        let state = self.state?;
        let session = self.session?;
        if count_only {
            return Some(session.windows().count().to_string());
        }

        // The order this loop renders in, decided before anything is rendered:
        // the sorted view is what the active body, `loop_last_flag` and each
        // item's own context all follow (`format.c:4758-4787`). Only this local
        // view is ordered; the session keeps its windows exactly as they are.
        let mut windows = session.windows().collect::<Vec<_>>();
        WindowLoopOrdering::parse(arguments).arrange(&mut windows);
        let total = windows.len();
        let active_window_index = session.active_window_index();
        let last_window_index = session.last_window_index();
        let attached_count = self.session_attached_count();
        let mut rendered = String::new();
        for (position, (window_index, window)) in windows.into_iter().enumerate() {
            let active = window_index == active_window_index;
            let is_last = position + 1 == total;
            let mut context = FormatContext::new()
                .with_managed_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    active,
                    Some(window_index) == last_window_index,
                )
                .with_named_value("loop_last_flag", bool_string(is_last));
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let runtime = self.inherit_client_loop_context(
                RuntimeFormatContext::new(context)
                    .with_state(state)
                    .with_session(session)
                    .with_window(window_index, window),
            );
            let runtime = if let Some(pane) = window.active_pane() {
                runtime.with_pane(pane)
            } else {
                runtime
            };
            rendered.push_str(&expand(loop_body(body, current_body, active), &runtime));
        }
        Some(rendered)
    }

    pub(super) fn render_pane_loop(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        let mut expand = |template: &str, variables: &dyn FormatVariables| {
            render_runtime_template(template, variables, false)
        };
        self.render_pane_loop_with_expand(body, current_body, count_only, &mut expand)
    }

    pub(super) fn render_pane_loop_with_expand(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
        expand: &mut dyn FnMut(&str, &dyn FormatVariables) -> String,
    ) -> Option<String> {
        let state = self.state?;
        let session = self.session?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        let window = self.window.or_else(|| session.window_at(window_index))?;
        if count_only {
            return Some(window.pane_count().to_string());
        }

        let total = window.pane_count();
        let active_window_index = session.active_window_index();
        let last_window_index = session.last_window_index();
        let attached_count = self.session_attached_count();
        let mut rendered = String::new();
        for (position, pane) in window.panes().iter().enumerate() {
            let active = pane.index() == window.active_pane_index();
            let is_last = position + 1 == total;
            let context = FormatContext::new()
                .with_managed_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window_index,
                    Some(window_index) == last_window_index,
                )
                .with_pane(pane, active)
                .with_named_value("loop_last_flag", bool_string(is_last));
            let runtime = self.inherit_client_loop_context(
                RuntimeFormatContext::new(context)
                    .with_state(state)
                    .with_session(session)
                    .with_window(window_index, window)
                    .with_pane(pane),
            );
            rendered.push_str(&expand(loop_body(body, current_body, active), &runtime));
        }
        Some(rendered)
    }

    pub(super) fn render_client_loop(
        &self,
        arguments: &[String],
        body: &str,
        expand: &mut dyn FnMut(&str, &dyn FormatVariables) -> String,
    ) -> Option<String> {
        let clients = self.client_loop_snapshots?;
        let mut clients = clients
            .iter()
            .filter(|client| client.is_attached())
            .collect::<Vec<_>>();
        let (sort_order, reversed) = ClientLoopSortOrder::parse(arguments);
        clients.sort_by(|left, right| {
            let ordering = match sort_order {
                ClientLoopSortOrder::Order => left.order.cmp(&right.order),
                ClientLoopSortOrder::Name => left.name().cmp(right.name()),
                ClientLoopSortOrder::Activity => right.activity_at.cmp(&left.activity_at),
            }
            .then_with(|| left.name().cmp(right.name()));
            if reversed {
                ordering.reverse()
            } else {
                ordering
            }
        });

        let total = clients.len();
        let mut rendered = String::new();
        for (position, client) in clients.into_iter().enumerate() {
            let runtime = client.bindings.apply(
                self.clone()
                    .with_named_value("loop_last_flag", bool_string(position + 1 == total)),
            );
            rendered.push_str(&expand(body, &runtime));
        }
        Some(rendered)
    }
}

fn loop_body<'a>(body: &'a str, current_body: Option<&'a str>, active: bool) -> &'a str {
    if active {
        current_body.unwrap_or(body)
    } else {
        body
    }
}

#[cfg(test)]
#[path = "automatic_name_tests.rs"]
mod automatic_name_tests;

#[cfg(test)]
#[path = "window_loop_name_tests.rs"]
mod window_loop_name_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
    use crate::pane_terminals::HandlerState;
    use rmux_core::formats::FormatVariables;
    use rmux_proto::{
        OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
    };

    fn client(name: &str, order: u64, activity_at: i64, session: &str) -> ClientLoopSnapshot {
        ClientLoopSnapshot {
            bindings: ClientFormatBindings {
                name: name.to_owned(),
                session: session.to_owned(),
                width: "80".to_owned(),
                height: "24".to_owned(),
                ..ClientFormatBindings::default()
            },
            order,
            activity_at: crate::client_activity::ClientActivityTimestamp::from_parts(
                activity_at as u64,
                0,
            ),
        }
    }

    fn render(clients: &[ClientLoopSnapshot], template: &str) -> String {
        let runtime = RuntimeFormatContext::new(
            FormatContext::new().with_named_value("session_name", "outer"),
        )
        .with_client_loop_snapshots(clients);
        render_runtime_template(template, &runtime, false)
    }

    #[test]
    fn client_loop_offsets_follow_each_binding_and_restore_outer_viewport() {
        let mut panned = client("panned", 0, 0, "s");
        panned.bindings.viewport = crate::client_viewport::ClientViewport {
            x: 14,
            y: 5,
            bigger: true,
            ..Default::default()
        };
        let mut automatic = client("automatic", 1, 0, "s");
        automatic.bindings.viewport.bigger = true;
        let control = client("control", 2, 0, "s");
        let clients = [panned, automatic, control];
        let runtime = clients[0].bindings.apply(
            RuntimeFormatContext::new(FormatContext::new()).with_client_loop_snapshots(&clients),
        );
        assert_eq!(
            render_runtime_template(
                "#{window_offset_x}|#{L:#{client_name}:#{window_offset_x},#{window_offset_y},#{window_bigger};}|#{window_offset_y}",
                &runtime,
                false,
            ),
            "14|panned:14,5,1;automatic:0,0,1;control:,,0;|5"
        );
    }

    #[test]
    fn client_loop_matches_sort_scope_and_body_matrix() {
        let clients = [
            client("tty018", 2, 30, "alpha"),
            client("tty019", 3, 20, "beta"),
            client("client", 1, 20, "beta"),
        ];

        assert_eq!(render(&[], "#{L:#{client_name}}"), "");
        assert_eq!(
            render(&clients, "#{L:#{client_name}}"),
            "clienttty018tty019"
        );
        assert_eq!(
            render(&clients, "#{L/i:#{client_name}}"),
            "clienttty018tty019"
        );
        assert_eq!(
            render(&clients, "#{L/ir:#{client_name}}"),
            "tty019tty018client"
        );
        assert_eq!(
            render(&clients, "#{L/n:#{client_name}}"),
            "clienttty018tty019"
        );
        assert_eq!(
            render(&clients, "#{L/nr:#{client_name}}"),
            "tty019tty018client"
        );
        assert_eq!(
            render(&clients, "#{L/t:#{client_name}}"),
            "tty018clienttty019"
        );
        assert_eq!(
            render(&clients, "#{L/tr:#{client_name}}"),
            "tty019clienttty018"
        );
        assert_eq!(
            render(
                &clients,
                "#{L:#{client_session}/#{session_name}/#{client_width}x#{client_height};}",
            ),
            "beta/outer/80x24;alpha/outer/80x24;beta/outer/80x24;"
        );
        assert_eq!(
            render(
                &clients,
                "#{L:MID:#{client_name}|#{?loop_last_flag,LAST:,}}",
            ),
            "MID:client|MID:tty018|MID:tty019|LAST:"
        );
        let clients_with_unattached = [
            client("tty018", 2, 30, "alpha"),
            client("orphan", 4, 40, ""),
        ];
        assert_eq!(
            render(&clients_with_unattached, "#{L:#{client_name}}"),
            "tty018"
        );
        let runtime = RuntimeFormatContext::new(FormatContext::new())
            .with_client_loop_snapshots(&clients_with_unattached);
        assert_eq!(
            runtime.format_loop_with_args('L', &[], "ignored", None, true),
            Some("1".to_owned())
        );
        let runtime =
            RuntimeFormatContext::new(FormatContext::new()).with_client_loop_snapshots(&clients);
        assert_eq!(
            runtime.format_loop_with_args('L', &[], "ignored", None, true),
            Some("3".to_owned())
        );
    }

    #[test]
    fn client_loop_body_keeps_the_outer_recursion_guard() {
        let clients = [client("tty018", 1, 1, "alpha")];
        let runtime = RuntimeFormatContext::new(
            FormatContext::new().with_named_value("@recursive", "#{L:#{E:@recursive}}"),
        )
        .with_client_loop_snapshots(&clients);

        assert_eq!(
            render_runtime_template("#{L:#{E:@recursive}}", &runtime, false),
            ""
        );
    }

    #[test]
    fn nested_session_window_and_pane_loops_keep_the_outer_recursion_guard() {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("loop-recursion").expect("valid session name");
        state
            .sessions
            .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session creates");
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        let clients = [client("tty018", 1, 1, "loop-recursion")];

        for nested_scope in ['S', 'W', 'P'] {
            let template = format!("#{{L:#{{{nested_scope}:#{{E:@r}}}}}}");
            let runtime = RuntimeFormatContext::new(
                FormatContext::new().with_named_value("@r", template.clone()),
            )
            .with_state(&state)
            .with_session(session)
            .with_client_loop_snapshots(&clients);

            assert_eq!(render_runtime_template(&template, &runtime, false), "");
        }
    }

    #[test]
    fn reconstructed_window_name_keeps_client_loop_context() {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("window-name-context").expect("valid session name");
        state
            .sessions
            .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session creates");
        state
            .options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(session_name.clone(), 0)),
                OptionName::AutomaticRenameFormat,
                "#{L:#{client_name}}".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("automatic rename format sets");
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        let window = session.window();
        let clients = [client("tty018", 1, 1, session_name.as_str())];
        let runtime = RuntimeFormatContext::new(FormatContext::new())
            .with_state(&state)
            .with_session(session)
            .with_window(0, window)
            .with_client_loop_snapshots(&clients);

        assert_eq!(runtime.format_name_exists(None, "tty018"), Some(true));
    }
}

/// The order a `#{W...}` loop renders a session's windows in.
///
/// `format.c:5200-5219` reads the argument letters in one order - `i` first,
/// then `n`, then `t`, else the session's own order - and `r` reverses whatever
/// was chosen. `sort.c:29-51` never sorts the session's own order at all: it
/// only reverses the collected list, which is why a `#{W:...}` with no argument
/// stays in index order. `sort.c:277-279` compares names with `strcmp`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WindowLoopSortOrder {
    /// The session's own order, by window index.
    Index,
    /// By window name, the name the loop body itself would print.
    Name,
    /// By activity. This parcours does not claim that sort: a `t` argument is
    /// left in the order it renders in today, and its `r` is not applied
    /// either, so nothing unmeasured moves.
    Activity,
}

/// One `#{W...}` argument, read once and applied once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WindowLoopOrdering {
    order: WindowLoopSortOrder,
    reversed: bool,
}

impl WindowLoopOrdering {
    fn parse(arguments: &[String]) -> Self {
        let Some(argument) = arguments.first() else {
            return Self {
                order: WindowLoopSortOrder::Index,
                reversed: false,
            };
        };
        let order = if argument.contains('i') {
            WindowLoopSortOrder::Index
        } else if argument.contains('n') {
            WindowLoopSortOrder::Name
        } else if argument.contains('t') {
            WindowLoopSortOrder::Activity
        } else {
            WindowLoopSortOrder::Index
        };
        Self {
            order,
            reversed: argument.contains('r'),
        }
    }

    /// Arranges this loop's own view of the windows. The session is not touched.
    fn arrange(self, windows: &mut [(u32, &rmux_core::Window)]) {
        match self.order {
            // Not claimed here: left exactly as it renders today.
            WindowLoopSortOrder::Activity => {}
            // The session's own order is never sorted, only reversed.
            WindowLoopSortOrder::Index => {
                if self.reversed {
                    windows.reverse();
                }
            }
            WindowLoopSortOrder::Name => {
                // The key is the name the body itself would print, and the sort
                // is stable, so windows sharing a name keep their index order.
                windows.sort_by(|(_, left), (_, right)| {
                    let ordering = window_loop_name(left).cmp(window_loop_name(right));
                    if self.reversed {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
            }
        }
    }
}

/// The name a window answers with in a loop body, and the key it sorts on.
fn window_loop_name(window: &rmux_core::Window) -> &str {
    window.name().unwrap_or("")
}

#[derive(Clone, Copy)]
enum ClientLoopSortOrder {
    Order,
    Name,
    Activity,
}

impl ClientLoopSortOrder {
    fn parse(arguments: &[String]) -> (Self, bool) {
        let Some(argument) = arguments.first() else {
            return (Self::Order, false);
        };
        let sort_order = if argument.contains('i') {
            Self::Order
        } else if argument.contains('n') {
            Self::Name
        } else if argument.contains('t') {
            Self::Activity
        } else {
            Self::Order
        };
        (sort_order, argument.contains('r'))
    }
}
