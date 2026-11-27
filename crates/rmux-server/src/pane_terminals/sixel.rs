use rmux_core::PaneId;
use rmux_proto::OptionName;

use super::HandlerState;

impl HandlerState {
    /// Announce transient SIXEL only where a native client can receive it.
    /// This does not claim the retained-image support of `#{sixel_support}`.
    pub(crate) fn sixel_passthrough_for_pane(&self, pane: PaneId) -> bool {
        self.sessions.iter().any(|session| {
            self.sixel_sessions.contains(&session.id())
                && session.active_pane_id() == Some(pane)
                && matches!(
                    self.options.resolve_for_pane(
                        session.name(),
                        session.active_window_index(),
                        session.active_pane_index(),
                        OptionName::AllowPassthrough,
                    ),
                    Some("on" | "all")
                )
        })
    }
}
