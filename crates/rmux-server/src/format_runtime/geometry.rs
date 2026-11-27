use rmux_core::{FloatingPaneRect, ManagedSessionView, Pane, PaneGeometry, Window};

use crate::pane_visible_geometry::{clip_pane_geometry, visible_pane_content_geometry};

use super::RuntimeFormatContext;
use rmux_proto::SessionName;

impl RuntimeFormatContext<'_> {
    /// Scrollbar geometry observes the pane as formats do: a poisoned pane
    /// reads as neither alternate nor copying instead of failing the request.
    fn observed_pane_viewport_state(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane: &Pane,
    ) -> (bool, bool) {
        self.state
            .and_then(|state| {
                state.observe_pane_transcript_in_window(
                    session_name,
                    window_index,
                    pane.id(),
                    self.pane_transcript_reach(pane.id()),
                    |transcript| {
                        (
                            transcript.is_alternate(),
                            transcript.copy_mode_state().is_some(),
                        )
                    },
                )
            })
            .unwrap_or((false, false))
    }

    /// tmux format.c reads signed pane offsets and window.c numbers floating
    /// panes from the front, followed by the tiled layer at float_count + 1.
    /// Read the canonical placement and stack without building another layout.
    pub(super) fn pane_position_format_value(
        &self,
        key: rmux_core::formats::FormatVariableKey,
    ) -> Option<String> {
        use rmux_core::formats::FormatVariableKey::{PaneX, PaneY, PaneZ};

        let pane = self.pane?;
        let window = match self.session {
            Some(session) => session.window_at(self.window_index?)?,
            None => self.window?,
        };
        // An absent or mismatched pane must not borrow the active pane's value.
        if !window.panes().iter().any(|member| member.id() == pane.id()) {
            return None;
        }
        // tmux's displayed zoom layer contains one tiled pane. Hidden panes
        // retain their saved coordinates, but all panes report that layer's z.
        if window.is_zoomed() && key == PaneZ {
            return Some("1".to_owned());
        }
        let zoomed_pane = window.is_zoomed()
            && window
                .active_pane()
                .is_some_and(|active| active.id() == pane.id());
        let stack = window.floating_stack();
        if key == PaneZ {
            let depth = stack
                .placements()
                .iter()
                .rev()
                .position(|placement| placement.pane_id() == pane.id())
                .unwrap_or(stack.len() + 1);
            return Some(depth.to_string());
        }
        let (x, y) = match stack.placement(pane.id()).filter(|_| !zoomed_pane) {
            Some(placement) => (placement.rect().x(), placement.rect().y()),
            None => {
                let geometry = self
                    .visible_pane_geometry()
                    .unwrap_or_else(|| pane.geometry());
                (i32::from(geometry.x()), i32::from(geometry.y()))
            }
        };
        match key {
            PaneX => Some(x.to_string()),
            PaneY => Some(y.to_string()),
            _ => None,
        }
    }

    pub(super) fn window_viewport_offset(&self) -> Option<(u16, u16)> {
        match self.client_bindings.as_ref() {
            Some(client) => client
                .viewport
                .bigger
                .then_some((client.viewport.x, client.viewport.y)),
            None => self.session.and_then(|session| session.viewport_offset()),
        }
    }

    pub(super) fn floating_format_value(
        &self,
        key: rmux_core::formats::FormatVariableKey,
    ) -> Option<String> {
        use rmux_core::formats::FormatVariableKey::*;
        if !matches!(
            key,
            PaneFloatingFlag
                | PaneLeft
                | PaneTop
                | PaneWidth
                | PaneHeight
                | PaneRight
                | PaneBottom
                | PaneX
                | PaneY
                | PaneAtLeft
                | PaneAtRight
                | PaneAtTop
                | PaneAtBottom
        ) {
            return None;
        }
        let window = self.visible_window_snapshot()?;
        let pane = match key {
            PaneX | PaneY | PaneAtLeft | PaneAtRight | PaneAtTop | PaneAtBottom => self.pane?,
            _ => self.pane.or_else(|| window.active_pane())?,
        };
        let placement = window.floating_stack().placement(pane.id());
        if key == PaneFloatingFlag {
            return Some(super::bool_string(
                placement.is_some() && !window.is_zoomed(),
            ));
        }
        if window.is_zoomed()
            && window
                .active_pane()
                .is_some_and(|active| active.id() == pane.id())
        {
            // Use the normal visible geometry for the pane filling the window;
            // its canonical floating rectangle remains available for unzoom.
            return None;
        }
        let rect = placement?.rect();
        let content = self.floating_scrollbar_layout(pane, rect).content;
        let reserved = rect.cols().min(i32::from(u16::MAX)) - i32::from(content.cols());
        let left = i64::from(rect.x()) + i64::from(content.x());
        let top = i64::from(rect.y());
        let width = i64::from(rect.cols() - reserved);
        let height = i64::from(rect.rows());
        let border = self.session.and_then(|session| {
            self.option_store()?.resolve_for_window(
                session.name(),
                self.window_index
                    .unwrap_or_else(|| session.active_window_index()),
                rmux_proto::OptionName::PaneBorderStatus,
            )
        });
        Some(
            match key {
                PaneLeft | PaneX => left,
                PaneTop | PaneY => top,
                PaneWidth => width,
                PaneHeight => height,
                PaneRight => left + width - 1,
                PaneBottom => top + height - 1,
                PaneAtLeft => i64::from(left == 0),
                PaneAtRight => i64::from(left + width == i64::from(window.size().cols)),
                PaneAtTop => i64::from(top == i64::from(border == Some("top"))),
                PaneAtBottom => i64::from(
                    top + height
                        == i64::from(window.size().rows) - i64::from(border == Some("bottom")),
                ),
                _ => return None,
            }
            .to_string(),
        )
    }

    fn floating_scrollbar_layout(
        &self,
        pane: &Pane,
        rect: FloatingPaneRect,
    ) -> crate::pane_scrollbar::PaneScrollbarLayout {
        // Work locally so clipping or unsigned offsets cannot change a signed
        // placement. Floating panes do not surrender an outer pane-status row.
        let geometry = PaneGeometry::new(0, 0, u16::try_from(rect.cols()).unwrap_or(u16::MAX), 0);
        let (Some(session), Some(options)) = (self.session, self.option_store()) else {
            return crate::pane_scrollbar::PaneScrollbarLayout {
                content: geometry,
                width: 0,
                pad: 0,
            };
        };
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        let (alternate, copying) =
            self.observed_pane_viewport_state(session.name(), window_index, pane);
        crate::pane_scrollbar::PaneScrollbarConfig::resolve(
            options,
            session.name(),
            window_index,
            pane.index(),
        )
        .layout(geometry, alternate, copying)
    }

    pub(super) fn visible_session_snapshot(&self) -> Option<ManagedSessionView<'_>> {
        self.session
    }

    pub(super) fn visible_window_snapshot(&self) -> Option<Window> {
        let session = self.visible_session_snapshot()?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        session.window_at(window_index).cloned()
    }

    pub(super) fn layout_window_snapshot(&self) -> Option<Window> {
        let session = self.session?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        session.layout_window_snapshot(window_index)
    }

    pub(super) fn visible_pane_snapshot(&self) -> Option<Pane> {
        let session = self.visible_session_snapshot()?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        let window = session.window_at(window_index)?;
        let pane_index = self
            .pane
            .map(Pane::index)
            .unwrap_or_else(|| window.active_pane_index());
        window.pane(pane_index).cloned()
    }

    pub(super) fn visible_pane_geometry(&self) -> Option<PaneGeometry> {
        let session = self.session?;
        let window = self.visible_window_snapshot()?;
        let window_index = self
            .window_index
            .unwrap_or_else(|| session.active_window_index());
        let pane = self.visible_pane_snapshot()?;
        let zoomed = window.is_zoomed() && pane.index() == window.active_pane_index();
        let geometry = if zoomed {
            PaneGeometry::new(0, 0, window.size().cols, window.size().rows)
        } else {
            pane.geometry()
        };
        // Preserve the full content of panes extending beyond the visible edge.
        let content_rows = window
            .size()
            .rows
            .max(geometry.y().saturating_add(geometry.rows()));
        let Some(options) = self.option_store() else {
            return Some(clip_pane_geometry(geometry, content_rows));
        };
        let geometry = visible_pane_content_geometry(
            options,
            session.name(),
            window_index,
            geometry,
            content_rows,
        );
        let (alternate, copying) =
            self.observed_pane_viewport_state(session.name(), window_index, &pane);
        Some(
            crate::pane_scrollbar::PaneScrollbarConfig::resolve(
                options,
                session.name(),
                window_index,
                pane.index(),
            )
            .content_geometry(geometry, alternate, copying),
        )
    }

    pub(super) fn pane_edge_format_value(
        &self,
        key: rmux_core::formats::FormatVariableKey,
    ) -> Option<String> {
        use rmux_core::formats::FormatVariableKey::{
            PaneAtBottom, PaneAtLeft, PaneAtRight, PaneAtTop,
        };
        let pane = self.pane?;
        let geometry = self.visible_pane_geometry()?;
        let window = self.visible_window_snapshot()?;
        // Pane-status rows belong to the vertical boundary; scrollbar columns
        // are excluded from the horizontal boundary, as in tmux 3.7b.
        let vertical = if window.is_zoomed() && pane.index() == window.active_pane_index() {
            PaneGeometry::new(0, 0, window.size().cols, window.size().rows)
        } else {
            pane.geometry()
        };
        let at_edge = match key {
            PaneAtLeft => geometry.x() == 0,
            PaneAtRight => geometry.x() + geometry.cols() >= window.size().cols,
            PaneAtTop => vertical.y() == 0,
            PaneAtBottom => vertical.y() + vertical.rows() >= window.size().rows,
            _ => return None,
        };
        Some(super::bool_string(at_edge))
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeFormatContext;
    use crate::session_fixture::SessionFixture;
    use rmux_core::formats::{render_template, FormatContext, FormatVariables};
    use rmux_core::FloatingPaneRequest;
    use rmux_proto::{SessionName, TerminalSize, WindowTarget};

    const POSITION: &str = "#{pane_x}|#{pane_y}|#{pane_z}";

    fn fixture() -> SessionFixture {
        SessionFixture::new(
            SessionName::new("format-position").unwrap(),
            TerminalSize { cols: 80, rows: 24 },
        )
    }

    fn position(fixture: &SessionFixture, index: u32) -> String {
        let session = fixture.view();
        let window = session.window();
        let pane = window.pane(index).unwrap();
        let context = RuntimeFormatContext::new(FormatContext::new())
            .with_session(session)
            .with_window(session.active_window_index(), window)
            .with_pane(pane);
        render_template(POSITION, &context)
    }

    #[test]
    fn pane_position_requires_the_targeted_pane() {
        let fixture = fixture();
        let session = fixture.view();
        let bare = RuntimeFormatContext::new(FormatContext::new());
        let window_only = bare
            .clone()
            .with_session(session)
            .with_window(session.active_window_index(), session.window());
        for context in [bare, window_only] {
            assert_eq!(render_template(POSITION, &context), "||");
            for name in ["pane_x", "pane_y", "pane_z"] {
                assert_eq!(context.format_value_by_name(name).as_deref(), Some(""));
                assert_eq!(context.format_path_value_by_name(name), None);
            }
        }
        assert_eq!(position(&fixture, 0), "0|0|1");
    }

    #[test]
    fn scrollbar_option_formats_use_the_pty_content_rectangle() {
        use rmux_core::OptionStore;
        use rmux_proto::{OptionName, ScopeSelector, SetOptionMode};

        let fixture = fixture();
        let mut options = OptionStore::new();
        for (option, value) in [
            (OptionName::PaneScrollbars, "on"),
            (OptionName::PaneScrollbarsPosition, "left"),
            (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
        ] {
            options
                .set(
                    ScopeSelector::Window(WindowTarget::with_window(fixture.name().clone(), 0)),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .unwrap();
        }
        let session = fixture.view();
        let window = session.window();
        let context = RuntimeFormatContext::new(FormatContext::new())
            .with_session(session)
            .with_window(0, window)
            .with_pane(window.pane(0).unwrap())
            .with_options(&options);
        // Linux tmux 3.7b, 80x24, status off, left width=2,pad=1.
        assert_eq!(
            render_template(
                "#{pane_left},#{pane_top}:#{pane_width}x#{pane_height}:#{pane_right},#{pane_bottom}:#{pane_at_left},#{pane_at_right},#{pane_at_top},#{pane_at_bottom}:#{pane_x},#{pane_y}",
                &context,
            ),
            "3,0:77x24:79,23:0,1,1,1:3,0"
        );
    }

    #[test]
    fn pane_position_reads_signed_placements_and_front_first_depth() {
        // tmux 3.7b format.c pane_x/y and window.c window_pane_zindex:
        // negative offsets survive, frontmost float is 0, tiled layer is N+1.
        let mut fixture = fixture();
        let target = WindowTarget::with_window(fixture.name().clone(), 0);
        let rear = fixture
            .store
            .new_floating_pane(
                &target,
                FloatingPaneRequest::new(Some(-3), Some(-2), 12, 5).unwrap(),
                true,
            )
            .unwrap();
        assert_eq!(position(&fixture, rear.pane_index), "-3|-2|0");
        assert_eq!(position(&fixture, 0), "0|0|2");
        let front = fixture
            .store
            .new_floating_pane(
                &target,
                FloatingPaneRequest::new(Some(7), Some(3), 10, 4).unwrap(),
                true,
            )
            .unwrap();
        assert_eq!(position(&fixture, rear.pane_index), "-3|-2|1");
        assert_eq!(position(&fixture, front.pane_index), "7|3|0");
        assert_eq!(position(&fixture, 0), "0|0|3");

        fixture.select_pane(rear.pane_index).unwrap();
        assert_eq!(position(&fixture, rear.pane_index), "-3|-2|0");
        assert_eq!(position(&fixture, front.pane_index), "7|3|1");
        assert_eq!(position(&fixture, 0), "0|0|3");
    }

    #[test]
    fn pane_position_does_not_read_a_pane_from_another_window() {
        let mut fixture = fixture();
        let old_pane = fixture.window().pane(0).unwrap().clone();
        let (index, _) = fixture
            .create_window(TerminalSize { cols: 40, rows: 12 })
            .unwrap();
        let session = fixture.view();
        let window = session.window_at(index).unwrap();
        let context = RuntimeFormatContext::new(FormatContext::new())
            .with_session(session)
            .with_window(index, window)
            .with_pane(&old_pane);
        assert_eq!(render_template(POSITION, &context), "||");
    }
}

#[cfg(test)]
#[path = "floating_zoom_tests.rs"]
mod floating_zoom_tests;
