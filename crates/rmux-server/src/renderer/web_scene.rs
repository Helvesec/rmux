use rmux_core::{OptionStore, Pane, PaneGeometry, PaneId, Utf8Config};
use rmux_proto::RmuxError;

use super::cursor_position_bytes;
use super::pane_row::{append_pane_row_tail, crop_rendered_pane_row};
use super::projection::{
    ProjectedPaneScrollbarContext, TerminalPaneProjection, TerminalSceneProjection,
};

/// Immutable renderer projection retained by a Web session presentation.
///
/// Equality deliberately includes the canonical scene and its resolution
/// context, so a rectangular update cannot be applied to a presentation that
/// was captured before an identity, placement, order, or occlusion change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebSceneProjection {
    projection: TerminalSceneProjection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebPaneProjectionContext {
    history_size: usize,
    alternate_on: bool,
    scroll_offset: usize,
}

impl WebPaneProjectionContext {
    pub(crate) const fn new(history_size: usize, alternate_on: bool, scroll_offset: usize) -> Self {
        Self {
            history_size,
            alternate_on,
            scroll_offset,
        }
    }
}

impl WebSceneProjection {
    pub(crate) fn resolve(
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
    ) -> Self {
        #[cfg(test)]
        RENDER_OBSERVER.with_borrow_mut(|observer| {
            if let Some(observer) = observer.take() {
                observer();
            }
        });
        Self {
            projection: TerminalSceneProjection::resolve(session, options),
        }
    }

    pub(crate) fn snapshot(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
    ) -> Result<rmux_proto::scene::SceneSnapshot, RmuxError> {
        self.projection.snapshot(session)
    }

    pub(crate) fn has_floating_panes(&self) -> bool {
        self.projection.has_floating_panes()
    }

    pub(crate) fn pane_ids_rear_to_front(&self) -> Vec<PaneId> {
        self.projection.pane_ids_rear_to_front()
    }

    pub(crate) fn pane_geometry(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        pane: &Pane,
        options: &OptionStore,
        context: WebPaneProjectionContext,
    ) -> Option<PaneGeometry> {
        self.pane_projection(session, pane, options, context)
            .and_then(|projection| projection.terminal_geometry())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn overlay_scrollback(
        &self,
        frame: &mut Vec<u8>,
        session: rmux_core::ManagedSessionView<'_>,
        pane: &Pane,
        lines: &[Vec<u8>],
        options: &OptionStore,
        context: WebPaneProjectionContext,
        terminal_cols: u16,
        maximum: usize,
    ) -> Result<(), RmuxError> {
        let Some(projection) = self.pane_projection(session, pane, options, context) else {
            return Ok(());
        };
        let utf8 = Utf8Config::from_options(options);
        let mut patch = Vec::new();
        for span in projection.spans() {
            let line = lines.get(span.source_row).map_or(&[][..], Vec::as_slice);
            let row = crop_rendered_pane_row(line, span.source_col, usize::from(span.cols), &utf8);
            patch.extend_from_slice(cursor_position_bytes(span.y, span.x).as_slice());
            patch.extend_from_slice(b"\x1b[0m");
            patch.extend_from_slice(row.bytes());
            append_pane_row_tail(
                &mut patch,
                span.y,
                span.x,
                span.cols,
                terminal_cols,
                row.columns(),
            );
        }
        if let Some(scrollbar) = projection.scrollbar() {
            patch.extend_from_slice(&scrollbar.frame());
        }
        patch.extend_from_slice(b"\x1b[?25l");
        let encoded_len = frame.len().saturating_add(patch.len());
        if encoded_len > maximum {
            return Err(RmuxError::FrameTooLarge {
                length: encoded_len,
                maximum,
            });
        }
        frame.extend_from_slice(&patch);
        Ok(())
    }

    fn pane_projection(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        pane: &Pane,
        options: &OptionStore,
        context: WebPaneProjectionContext,
    ) -> Option<TerminalPaneProjection> {
        self.projection.pane_projection(
            session,
            options,
            pane,
            ProjectedPaneScrollbarContext {
                history_size: context.history_size,
                alternate_on: context.alternate_on,
                copy_mode_scroll_position: (context.scroll_offset > 0)
                    .then_some(context.scroll_offset),
            },
        )
    }
}

#[cfg(test)]
thread_local! {
    static RENDER_OBSERVER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
impl WebSceneProjection {
    pub(crate) fn observe_next_render(observer: impl FnOnce() + 'static) {
        RENDER_OBSERVER.with_borrow_mut(|slot| {
            assert!(slot.is_none(), "previous render observer was not consumed");
            *slot = Some(Box::new(observer));
        });
    }
}
