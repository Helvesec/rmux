use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rmux_core::formats::{FormatContext, FormatVariable, FormatVariables};
use rmux_core::Window;
use rmux_proto::{OptionName, SessionName};

use super::{render_runtime_template, RuntimeFormatContext};

/// A window as one session links it: the session name and the window index.
type WindowKey = (SessionName, u32);

/// The window names already resolved by one format expansion.
///
/// tmux answers `#W`, the `S:`/`W:` loop bodies and `N:` from the one stored
/// `w->name`, which `names.c` derives from the window's active pane. RMUX
/// renders `automatic-rename-format` live instead, so without this record each
/// reference read the foreground process again (and from whichever pane the
/// context held): one expansion could print two names for one window. Every
/// context derived from the same expansion shares this record, so a window is
/// named once and every reference agrees.
#[derive(Clone, Default)]
pub(super) struct ExpansionWindowNames(Rc<RefCell<HashMap<WindowKey, Option<String>>>>);

impl ExpansionWindowNames {
    fn get(&self, key: &WindowKey) -> Option<Option<String>> {
        self.0.borrow().get(key).cloned()
    }

    fn insert(&self, key: WindowKey, name: Option<String>) {
        self.0.borrow_mut().insert(key, name);
    }
}

impl<'a> RuntimeFormatContext<'a> {
    /// Renders `automatic-rename-format` in this context, uncached.
    ///
    /// The automatic-rename sync calls this to compute the name it stores.
    pub(super) fn automatic_window_name(&self) -> Option<String> {
        if self.expanding_automatic_name {
            return None;
        }
        let state = self.state?;
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        let window = self.window?;
        let tracked = state.tracks_auto_named_window(session_name, window_index);
        if !crate::automatic_rename::window_allows_automatic_rename(
            &state.options,
            session_name,
            window_index,
            window,
            tracked,
        ) {
            return None;
        }

        let template = state
            .options
            .resolve_for_window(
                session_name,
                window_index,
                OptionName::AutomaticRenameFormat,
            )
            .unwrap_or_default();
        if template.is_empty() {
            return None;
        }

        let mut runtime = self.clone();
        runtime.expanding_automatic_name = true;
        let rendered = render_runtime_template(template, &runtime, false);
        (!rendered.is_empty()).then_some(rendered)
    }

    /// `#{window_name}`: the expansion's single name for this context's window.
    pub(super) fn window_name(&self) -> Option<String> {
        match self
            .window_index
            .zip(self.window)
            .and_then(|(window_index, window)| self.shared_window_name(window_index, window))
        {
            Some(name) => name,
            None => self.window_name_in_this_context(),
        }
    }

    /// The name `N:` and the window loops compare and print for a window.
    pub(super) fn rendered_window_name(
        &self,
        window_index: u32,
        window: &'a Window,
    ) -> Option<String> {
        self.shared_window_name(window_index, window).flatten()
    }

    fn window_name_in_this_context(&self) -> Option<String> {
        self.automatic_window_name()
            .or_else(|| self.base.format_value(FormatVariable::WindowName))
    }

    /// Resolves a window's name once per expansion, in the window's own context
    /// with its active pane (tmux `w->name`). `None` when this context has no
    /// session to build that window context from.
    fn shared_window_name(&self, window_index: u32, window: &'a Window) -> Option<Option<String>> {
        let window_context = self.window_context(window_index, window)?;
        // Inside an automatic-rename-format render the window reads its stored
        // name; that answer is not the expansion's resolved name.
        if self.expanding_automatic_name {
            return Some(window_context.window_name_in_this_context());
        }
        let key = (self.session?.name().clone(), window_index);
        if let Some(name) = self.window_names.get(&key) {
            return Some(name);
        }
        let name = window_context.window_name_in_this_context();
        self.window_names.insert(key, name.clone());
        Some(name)
    }

    fn window_context(
        &self,
        window_index: u32,
        window: &'a Window,
    ) -> Option<RuntimeFormatContext<'a>> {
        let state = self.state?;
        let session = self.session?;
        let mut context = FormatContext::new()
            .with_managed_session(session)
            .with_session_attached(self.session_attached_count())
            .with_window(
                window_index,
                window,
                window_index == session.active_window_index(),
                Some(window_index) == session.last_window_index(),
            );
        if let Some(pane) = window.active_pane() {
            context = context.with_window_pane(window, pane);
        }
        let runtime = RuntimeFormatContext::new(context)
            .with_state(state)
            .with_session(session)
            .with_window(window_index, window);
        let runtime = self.inherit_client_loop_context(runtime);
        Some(match window.active_pane() {
            Some(pane) => runtime.with_pane(pane),
            None => runtime,
        })
    }
}
