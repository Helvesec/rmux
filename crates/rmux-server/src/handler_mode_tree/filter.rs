//! Shared filtering helpers for mode-tree builders.

use rmux_core::formats::is_truthy;

use super::mode_tree_model::ModeTreeClientState;

pub(super) fn matches_mode_tree_filter(
    mode: &ModeTreeClientState,
    search_text: &str,
    format_filter: Option<&str>,
) -> bool {
    if let Some(filter) = format_filter
        && !is_truthy(filter) {
            return false;
        }
    let Some(filter) = mode.filter_text.as_deref() else {
        return true;
    };
    if filter.is_empty() {
        return true;
    }
    let lowered = filter.to_ascii_lowercase();
    search_text.to_ascii_lowercase().contains(&lowered)
}
