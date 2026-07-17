use super::{
    CopyModeOverlayRange, CopyModeRenderOverlays, CopyModeRenderSnapshot, CopyModeState,
    SearchMatch,
};

impl CopyModeState {
    pub(crate) fn render_screen(&self) -> rmux_core::Screen {
        let mut screen = self
            .backing
            .clone_viewport(self.top_line, self.cursor.x, self.cursor.y);
        if let Some(selection) = self.selection_snapshot() {
            self.mark_selection_in_viewport(&mut screen, selection);
        }
        screen
    }

    pub(crate) fn render_snapshot(&self) -> CopyModeRenderSnapshot {
        CopyModeRenderSnapshot {
            screen: self.render_screen(),
            overlays: self.render_overlays(),
        }
    }

    fn render_overlays(&self) -> CopyModeRenderOverlays {
        let mark = (self.show_mark && self.cols() > 0)
            .then_some(self.mark)
            .flatten()
            .and_then(|mark| self.visible_full_row(mark.y));
        if !self.search_highlighted {
            return CopyModeRenderOverlays {
                mark,
                ..CopyModeRenderOverlays::default()
            };
        }

        let matches = self
            .search_results
            .iter()
            .filter_map(|matched| self.visible_match_range(matched))
            .collect();
        let current_match = self
            .search_current
            .and_then(|index| self.search_results.get(index))
            .and_then(|matched| self.visible_match_range(matched));
        CopyModeRenderOverlays {
            mark,
            matches,
            current_match,
        }
    }

    fn visible_full_row(&self, absolute_y: usize) -> Option<CopyModeOverlayRange> {
        let row = self.visible_row(absolute_y)?;
        Some(CopyModeOverlayRange {
            row,
            start_x: 0,
            end_x: self.cols().saturating_sub(1),
        })
    }

    fn visible_match_range(&self, matched: &SearchMatch) -> Option<CopyModeOverlayRange> {
        if matched.start.y != matched.end.y {
            return None;
        }
        Some(CopyModeOverlayRange {
            row: self.visible_row(matched.start.y)?,
            start_x: matched.start.x,
            end_x: matched.end.x,
        })
    }

    fn visible_row(&self, absolute_y: usize) -> Option<u32> {
        let row = absolute_y.checked_sub(self.top_line)?;
        (row < usize::from(self.rows().max(1)))
            .then_some(row)
            .and_then(|row| u32::try_from(row).ok())
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::Screen;
    use rmux_proto::TerminalSize;

    use super::*;
    use crate::copy_mode::types::{CopyPosition, SearchMatch};

    #[test]
    fn snapshot_projects_visible_search_current_and_full_mark_row() {
        let screen = Screen::new(TerminalSize { cols: 20, rows: 3 }, 20);
        let mut state = CopyModeState::for_test(screen);
        state.top_line = 0;
        state.mark = Some(CopyPosition { x: 7, y: 1 });
        state.show_mark = true;
        state.search_results = vec![
            SearchMatch {
                start: CopyPosition { x: 2, y: 0 },
                end: CopyPosition { x: 4, y: 0 },
                text: "one".to_owned(),
            },
            SearchMatch {
                start: CopyPosition { x: 8, y: 1 },
                end: CopyPosition { x: 10, y: 1 },
                text: "two".to_owned(),
            },
        ];
        state.search_current = Some(1);
        state.search_highlighted = true;

        let overlays = state.render_snapshot().overlays;

        assert_eq!(
            overlays.mark,
            Some(CopyModeOverlayRange {
                row: 1,
                start_x: 0,
                end_x: 19,
            })
        );
        assert_eq!(overlays.matches.len(), 2);
        assert_eq!(overlays.current_match, overlays.matches.get(1).copied());
    }

    #[test]
    fn hidden_search_results_and_offscreen_mark_are_not_projected() {
        let screen = Screen::new(TerminalSize { cols: 10, rows: 2 }, 20);
        let mut state = CopyModeState::for_test(screen);
        state.top_line = 2;
        state.mark = Some(CopyPosition { x: 0, y: 0 });
        state.show_mark = true;
        state.search_results = vec![SearchMatch {
            start: CopyPosition { x: 0, y: 2 },
            end: CopyPosition { x: 2, y: 2 },
            text: "hit".to_owned(),
        }];
        state.search_current = Some(0);
        state.search_highlighted = false;

        assert_eq!(
            state.render_snapshot().overlays,
            CopyModeRenderOverlays::default()
        );
    }
}
