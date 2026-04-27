#[cfg(unix)]
use rmux_core::formats::FormatContext;

use super::RuntimeFormatContext;
#[cfg(unix)]
use super::{bool_string, render_runtime_template};

impl RuntimeFormatContext<'_> {
    #[cfg(unix)]
    pub(super) fn render_session_loop(&self, body: &str, count_only: bool) -> Option<String> {
        let state = self.state?;
        let sessions = self.session_store?;
        if count_only {
            return Some(sessions.len().to_string());
        }

        let total = sessions.len();
        let mut rendered = String::new();
        for (position, (session_name, session)) in sessions.iter().enumerate() {
            let active_window_index = session.active_window_index();
            let active_window = session.window();
            let is_last = position + 1 == total;
            let attached_count = if self
                .session_name()
                .is_some_and(|current| current == session_name)
            {
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
            rendered.push_str(&render_runtime_template(body, &runtime, false));
        }
        Some(rendered)
    }

    #[cfg(windows)]
    pub(super) fn render_session_loop(&self, _body: &str, count_only: bool) -> Option<String> {
        count_only.then(|| {
            self.session_store
                .map_or(0, |sessions| sessions.len())
                .to_string()
        })
    }

    #[cfg(unix)]
    pub(super) fn render_window_loop(&self, body: &str, count_only: bool) -> Option<String> {
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
            let is_last = position + 1 == total;
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    *window_index,
                    window,
                    *window_index == active_window_index,
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
            rendered.push_str(&render_runtime_template(body, &runtime, false));
        }
        Some(rendered)
    }

    #[cfg(windows)]
    pub(super) fn render_window_loop(&self, _body: &str, count_only: bool) -> Option<String> {
        count_only.then(|| {
            self.session
                .map_or(0, |session| session.windows().len())
                .to_string()
        })
    }

    #[cfg(unix)]
    pub(super) fn render_pane_loop(&self, body: &str, count_only: bool) -> Option<String> {
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
            let is_last = position + 1 == total;
            let context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window_index,
                    Some(window_index) == last_window_index,
                )
                .with_pane(pane, pane.index() == window.active_pane_index())
                .with_named_value("loop_last_flag", bool_string(is_last));
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window)
                .with_pane(pane);
            rendered.push_str(&render_runtime_template(body, &runtime, false));
        }
        Some(rendered)
    }

    #[cfg(windows)]
    pub(super) fn render_pane_loop(&self, _body: &str, count_only: bool) -> Option<String> {
        count_only.then(|| {
            let count = self
                .session
                .and_then(|session| {
                    let window_index = self
                        .window_index
                        .unwrap_or_else(|| session.active_window_index());
                    self.window
                        .or_else(|| session.window_at(window_index))
                        .map(|window| window.pane_count())
                })
                .unwrap_or(0);
            count.to_string()
        })
    }
}
