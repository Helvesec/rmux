use rmux_core::formats::FormatVariable;

use super::RuntimeFormatContext;

impl RuntimeFormatContext<'_> {
    pub(super) fn visible_pane_index(&self) -> Option<String> {
        let session = self.session?;
        let options = self.options?;
        let window_index = self.window_index?;
        let pane = self.pane?;
        Some(
            crate::pane_indices::visible_pane_index(session, options, window_index, pane.index())
                .to_string(),
        )
    }

    pub(super) fn runtime_format_value(&self, variable: FormatVariable) -> Option<String> {
        match variable {
            FormatVariable::PaneIndex => self.visible_pane_index(),
            FormatVariable::WindowName => self.window_name(),
            _ => None,
        }
    }
}
