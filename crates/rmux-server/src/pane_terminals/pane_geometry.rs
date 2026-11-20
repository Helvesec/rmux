use rmux_core::{OptionStore, PaneGeometry, Window};
use rmux_proto::SessionName;

use crate::pane_visible_geometry::visible_pane_content_geometry;

/// The addressed occurrence and its borrowed window, independent of the store
/// that owns it. Content geometry comes from the window, never outer session size.
pub(super) struct PaneWindowGeometry<'a> {
    session_name: &'a SessionName,
    window_index: u32,
    window: &'a Window,
}

impl<'a> PaneWindowGeometry<'a> {
    pub(super) fn new(
        session_name: &'a SessionName,
        window_index: u32,
        window: &'a Window,
    ) -> Self {
        Self {
            session_name,
            window_index,
            window,
        }
    }

    pub(super) fn content_rows(&self) -> u16 {
        self.window.size().rows
    }

    pub(super) fn content_geometry(
        &self,
        options: &OptionStore,
        pane_index: u32,
        geometry: PaneGeometry,
        alternate_on: bool,
        copy_mode_active: bool,
    ) -> PaneGeometry {
        let floating = self.window.pane(pane_index).is_some_and(|pane| {
            self.window.floating_stack().placement(pane.id()).is_some()
                && !(self.window.is_zoomed() && pane.index() == self.window.active_pane_index())
        });
        // A floating endpoint owns its whole content height, even off-screen.
        // Border-status rows are reserved by the tiled (or zoomed) window only.
        let geometry = if floating {
            geometry
        } else {
            visible_pane_content_geometry(
                options,
                self.session_name,
                self.window_index,
                geometry,
                self.content_rows(),
            )
        };
        crate::pane_scrollbar::PaneScrollbarConfig::resolve(
            options,
            self.session_name,
            self.window_index,
            pane_index,
        )
        .content_geometry(geometry, alternate_on, copy_mode_active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_core::ManagedSessionStore;
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, TerminalSize, WindowTarget};

    #[test]
    fn floating_border_scrollbar_option_geometry_preserves_native_rows() {
        let mut fixture = crate::session_fixture::SessionFixture::new(
            SessionName::new("floating-option-pty").unwrap(),
            TerminalSize::new(80, 24),
        );
        let target = WindowTarget::with_window(fixture.name().clone(), 0);
        let mut options = OptionStore::new();
        for (option, value) in [
            (OptionName::PaneBorderStatus, "top"),
            (OptionName::PaneScrollbars, "on"),
            (OptionName::PaneScrollbarsPosition, "left"),
            (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
        ] {
            options
                .set(
                    ScopeSelector::Window(target.clone()),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .unwrap();
        }
        for (x, y) in [(-3, -2), (0, 0), (3, 4), (70, 20)] {
            let created = fixture
                .store
                .new_floating_pane(
                    &target,
                    rmux_core::FloatingPaneRequest::new(Some(x), Some(y), 30, 8).unwrap(),
                    true,
                )
                .unwrap();
            let session = fixture.view();
            let pane = session.window().pane(created.pane_index).unwrap();
            let consumer = PaneWindowGeometry::new(session.name(), 0, session.window());
            let geometry =
                consumer.content_geometry(&options, pane.index(), pane.geometry(), false, false);
            // Linux tmux 3.7b TIOCGWINSZ, including panes beyond the window.
            assert_eq!(
                (geometry.cols(), geometry.rows()),
                (27, 8),
                "position {x},{y}"
            );
        }
    }

    #[test]
    fn managed_alias_uses_addressed_options_and_canonical_content_size() {
        let a = SessionName::new("a").unwrap();
        let b = SessionName::new("b").unwrap();
        let mut store = ManagedSessionStore::new();
        store
            .create_session(a.clone(), TerminalSize::new(80, 24))
            .unwrap();
        store
            .create_session(b.clone(), TerminalSize::new(100, 30))
            .unwrap();
        let id = store.session(&a).unwrap().window().id();
        let alias = WindowTarget::with_window(b.clone(), 7);
        let _ = store.insert_alias(&alias, id).unwrap();
        store
            .attached_window_geometry_mut(&WindowTarget::with_window(a.clone(), 0))
            .unwrap()
            .resize(TerminalSize::new(80, 24), TerminalSize::new(80, 22));
        let mut options = OptionStore::new();
        for (option, value) in [
            (OptionName::PaneScrollbars, "on"),
            (OptionName::PaneScrollbarsPosition, "left"),
            (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
        ] {
            options
                .set(
                    ScopeSelector::Window(alias.clone()),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .unwrap();
        }
        let source = store.session(&a).unwrap();
        let peer = store.session(&b).unwrap();
        let source_geometry = PaneWindowGeometry::new(&a, 0, source.window());
        let alias_geometry = PaneWindowGeometry::new(&b, 7, peer.window_at(7).unwrap());
        assert_eq!(source_geometry.content_rows(), 22);
        assert_eq!(alias_geometry.content_rows(), 22);
        let full = PaneGeometry::new(0, 0, 80, 22);
        assert_eq!(
            source_geometry.content_geometry(&options, 0, full, false, false),
            full
        );
        assert_eq!(
            alias_geometry.content_geometry(&options, 0, full, false, false),
            PaneGeometry::new(3, 0, 77, 22)
        );
        assert_eq!(
            alias_geometry.content_geometry(&options, 0, full, true, false),
            full
        );
        assert_eq!(peer.terminal_size(), TerminalSize::new(100, 30));
    }
}
