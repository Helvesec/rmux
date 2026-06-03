use rmux_core::{OptionStore, Session};
use rmux_proto::OptionName;

pub(crate) fn visible_pane_index(
    session: &Session,
    options: &OptionStore,
    window_index: u32,
    pane_index: u32,
) -> u32 {
    pane_index.saturating_add(pane_base_index(session, options, window_index))
}

pub(crate) fn pane_base_index(session: &Session, options: &OptionStore, window_index: u32) -> u32 {
    options
        .resolve_for_window(session.name(), window_index, OptionName::PaneBaseIndex)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0)
}
