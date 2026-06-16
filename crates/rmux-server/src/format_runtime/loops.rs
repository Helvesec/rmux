use rmux_core::formats::FormatContext;

use super::RuntimeFormatContext;
use super::{bool_string, render_runtime_template};

impl RuntimeFormatContext<'_> {
    pub(super) fn render_session_loop(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        let state = self.state?;
        let sessions = self.session_store?;
        if count_only {
            return Some(sessions.len().to_string());
        }

        let mut sessions = sessions.iter().collect::<Vec<_>>();
        sessions.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        let total = sessions.len();
        let mut rendered = String::new();
        for (position, (session_name, session)) in sessions.iter().enumerate() {
            let active = self
                .session_name()
                .is_some_and(|current| current == *session_name);
            let active_window_index = session.active_window_index();
            let active_window = session.window();
            let is_last = position + 1 == total;
            let attached_count = if active {
                self.session_attached_count()
            } else {
                0
            };
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(active_window_index, active_window, true, false)
                .with_named_value("loop_last_flag", bool_string(is_last));
            if let Some(pane) = active_window.active_pane() {
                context = context.with_window_pane(active_window, pane);
            }
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(active_window_index, active_window);
            let runtime = if let Some(pane) = active_window.active_pane() {
                runtime.with_pane(pane)
            } else {
                runtime
            };
            rendered.push_str(&render_runtime_template(
                loop_body(body, current_body, active),
                &runtime,
                false,
            ));
        }
        Some(rendered)
    }

    pub(super) fn render_window_loop(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        let state = self.state?;
        let session = self.session?;
        if count_only {
            return Some(session.windows().len().to_string());
        }

        let total = session.windows().len();
        let active_window_index = session.active_window_index();
        let last_window_index = session.last_window_index();
        let attached_count = self.session_attached_count();
        let mut rendered = String::new();
        for (position, (window_index, window)) in session.windows().iter().enumerate() {
            let active = *window_index == active_window_index;
            let is_last = position + 1 == total;
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    *window_index,
                    window,
                    active,
                    Some(*window_index) == last_window_index,
                )
                .with_named_value("loop_last_flag", bool_string(is_last));
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(*window_index, window);
            let runtime = if let Some(pane) = window.active_pane() {
                runtime.with_pane(pane)
            } else {
                runtime
            };
            rendered.push_str(&render_runtime_template(
                loop_body(body, current_body, active),
                &runtime,
                false,
            ));
        }
        Some(rendered)
    }

    pub(super) fn render_pane_loop(
        &self,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
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
            let context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window_index,
                    Some(window_index) == last_window_index,
                )
                .with_pane(pane, active)
                .with_named_value("loop_last_flag", bool_string(is_last));
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window)
                .with_pane(pane);
            rendered.push_str(&render_runtime_template(
                loop_body(body, current_body, active),
                &runtime,
                false,
            ));
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
