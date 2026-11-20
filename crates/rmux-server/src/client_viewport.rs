//! Per-client panning. Canonical window sizes and pane coordinates are inputs only.
use rmux_core::{input::mode, ManagedSessionView};
use rmux_proto::{TerminalSize, WindowId};

use crate::{pane_terminals::HandlerState, renderer::StatusGeometry};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClientPan {
    manual: Option<(WindowId, u16, u16)>,
    revision: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClientViewport {
    pub(crate) window: Option<WindowId>,
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) bigger: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PanAction {
    Reset,
    Left(u32),
    Right(u32),
    Up(u32),
    Down(u32),
}

impl ClientPan {
    pub(crate) fn resolve(
        &mut self,
        state: &HandlerState,
        session: ManagedSessionView<'_>,
        size: TerminalSize,
    ) -> ClientViewport {
        let mut presentation = rmux_core::ManagedSessionPresentation::new(session);
        presentation.set_terminal_size(size);
        let visible =
            StatusGeometry::for_session(presentation.view(), &state.options).content_size();
        let window = session.window();
        let max_x = window.size().cols.saturating_sub(visible.cols);
        let max_y = window.size().rows.saturating_sub(visible.rows);
        let bigger = max_x != 0 || max_y != 0;
        let mut result = ClientViewport {
            window: Some(window.id()),
            bigger,
            ..ClientViewport::default()
        };
        if !bigger {
            self.manual = None;
            return result;
        }
        if let Some((id, x, y)) = self.manual.filter(|(id, _, _)| *id == window.id()) {
            result.x = x.min(max_x);
            result.y = y.min(max_y);
            self.manual = Some((id, result.x, result.y));
            return result;
        }
        self.manual = None;
        let Some(pane) = window.active_pane() else {
            return result;
        };
        let cursor = state
            .pane_copy_mode_render_snapshot(session.name(), pane.id())
            .map(|snapshot| {
                (
                    snapshot.screen.cursor_position().0,
                    snapshot.screen.cursor_position().1,
                    snapshot.screen.mode(),
                )
            });
        #[cfg(feature = "web")]
        let cursor = cursor.or_else(|| {
            state.with_pane_screen(session.name(), pane.id(), |screen| {
                (
                    screen.cursor_position().0,
                    screen.cursor_position().1,
                    screen.mode(),
                )
            })
        });
        let Some((cx, cy, screen_mode)) = cursor else {
            return result;
        };
        if screen_mode & mode::MODE_CURSOR == 0 {
            return result;
        }
        let geometry = pane.geometry();
        let cx = u32::from(geometry.x()).saturating_add(cx);
        let cy = u32::from(geometry.y()).saturating_add(cy);
        result.x = if cx < u32::from(visible.cols) {
            0
        } else if cx > u32::from(max_x) {
            max_x
        } else {
            cx.saturating_sub(u32::from(visible.cols / 2))
                .min(u32::from(max_x)) as u16
        };
        result.y = if cy < u32::from(visible.rows) {
            0
        } else if cy > u32::from(max_y) {
            max_y
        } else {
            cy.saturating_sub(u32::from(visible.rows))
                .saturating_add(1)
                .min(u32::from(max_y)) as u16
        };
        result
    }

    pub(crate) fn apply(&mut self, action: PanAction, current: ClientViewport) {
        self.revision = self.revision.saturating_add(1);
        let Some(window) = current.window else { return };
        let add = |a: u16, b: u32| u32::from(a).saturating_add(b).min(u32::from(u16::MAX)) as u16;
        let sub = |a: u16, b: u32| u32::from(a).saturating_sub(b) as u16;
        self.manual = match action {
            PanAction::Reset => None,
            PanAction::Left(n) => Some((window, sub(current.x, n), current.y)),
            PanAction::Right(n) => Some((window, add(current.x, n), current.y)),
            PanAction::Up(n) => Some((window, current.x, sub(current.y, n))),
            PanAction::Down(n) => Some((window, current.x, add(current.y, n))),
        };
    }
}
