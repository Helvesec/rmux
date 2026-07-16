use rmux_core::formats::{styled_text_width, truncate_styled_text_to_width};
use rmux_core::Utf8Config;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct StatusComponent {
    pub(super) expanded: String,
    pub(super) width: usize,
}

/// Apply a status component's cell limit without charging embedded `#[...]`
/// clauses against it. The clauses stay in the returned text so format_draw
/// can preserve styles, alignment, and mouse ranges when composing the line.
pub(super) fn truncate_status_component(
    expanded: &str,
    max_width: usize,
    utf8: &Utf8Config,
) -> StatusComponent {
    let expanded = truncate_styled_text_to_width(expanded, max_width, utf8);
    let width = styled_text_width(&expanded, utf8);
    StatusComponent { expanded, width }
}
