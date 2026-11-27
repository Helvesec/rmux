//! Pane and window values read from the live pane screens and options.

use rmux_core::{colour_to_string, Pane, Screen, Style, StyleCell};
use rmux_proto::OptionName;

use super::super::bool_string;
use super::RuntimeFormatContext;

impl RuntimeFormatContext<'_> {
    pub(super) fn pane_history_all_bytes(&self) -> Option<String> {
        self.observe_pane_transcript(|transcript| transcript.tmux_history_all_bytes())
    }

    /// Whether the *targeted* pane received output while it was in a mode.
    ///
    /// The source answers `0` or `1` for a pane and nothing at all without one
    /// (`tmux-3.7b/format.c:2179-2190`), so an absent pane, session or state
    /// leaves this variable unset rather than answering for another pane.
    pub(super) fn pane_unseen_changes(&self) -> Option<String> {
        self.observe_pane_transcript(|transcript| bool_string(transcript.unseen_changes()))
    }

    /// Whether the *targeted* pane is the one its zoomed window is showing.
    ///
    /// The source marks the single pane `window_zoom` made active and clears
    /// that mark on every pane when the window is unzoomed
    /// (`tmux-3.7b/window.c:719-771`), answering `0` for a pane that carries no
    /// mark and nothing at all without a pane (`format.c:2444-2456`). The value
    /// is therefore the window's own zoom AND the identity of the pane that
    /// window holds active - never a window flag handed to every pane it
    /// contains. The window is taken from the session that holds this pane, so
    /// this never reads the unzoomed geometric projection a format reader may
    /// be carrying (`session/managed/view.rs:129-141`).
    pub(super) fn pane_zoomed_flag(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let window = session.window_at(session.window_index_for_pane_id(pane.id())?)?;
        Some(bool_string(
            window.is_zoomed() && window.active_pane().map(Pane::id) == Some(pane.id()),
        ))
    }

    /// The DECSTBM region of the *targeted* pane's own terminal screen, 0-based
    /// and inclusive, borrowed under the transcript lock instead of cloning the
    /// whole screen for two integers.
    pub(super) fn pane_scroll_region(&self) -> Option<(u32, u32)> {
        self.with_observed_pane_screen(Screen::scroll_region)
    }

    /// The text of the cell under the cursor of the *targeted* pane's own
    /// terminal screen.
    ///
    /// The position and the cell come from a single borrow of that one screen,
    /// so they cannot describe different moments, and it is the terminal screen
    /// rather than the copy-mode render: tmux reads `wp->base` at its own cursor
    /// (`format.c:1195-1209`). A real blank cell is one space, exactly as the
    /// default cell is (`grid.c:38-41`), and a column past the line's stored
    /// cells is that same default cell (`grid.c:581-589`). The padding half of a
    /// wide character has no text of its own, so it answers nothing at all, as
    /// does a row outside the viewport or a pane with no screen.
    /// The cursor the DEC alternate screen saved on the *targeted* pane's own
    /// terminal screen.
    ///
    /// tmux keeps this pair on the pane's base screen: `screen_init` starts it at
    /// `UINT_MAX` (`screen.c:122-123`), `screen_alternate_on` takes the cursor at
    /// `1049h` and `screen_alternate_off` leaves the pair as it is at `1049l`
    /// (`screen.c:676-765`), so it is kept after the cycle. The value is absent
    /// only when there is no pane to read (`format.c:1369-1397`).
    pub(super) fn pane_alternate_saved_cursor(&self) -> Option<(u32, u32)> {
        self.with_observed_pane_screen(|screen| {
            screen
                .alternate_saved_cursor()
                .map_or((u32::MAX, u32::MAX), |(saved_x, saved_y, _)| {
                    (saved_x, saved_y)
                })
        })
    }

    pub(super) fn pane_cursor_character(&self) -> Option<String> {
        self.with_observed_pane_screen(|screen| {
            let (cursor_x, cursor_y) = screen.cursor_position();
            let column = usize::try_from(cursor_x).ok()?;
            let row = usize::try_from(cursor_y).ok()?;
            let mut index = 0_usize;
            let mut found = None;
            let visited = screen.visit_visible_line_cells(row, column + 1, |cell| {
                if index == column && !cell.is_padding() {
                    found = Some(cell.text().to_owned());
                }
                index += 1;
            });
            visited.then_some(found).flatten()
        })
        .flatten()
    }

    pub(super) fn pane_pipe(&self) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let window_index = self.window_index?;
        Some(bool_string(self.state.is_some_and(|state| {
            state.pane_has_pipe(session.name(), window_index, pane.id())
        })))
    }

    pub(super) fn pane_synchronized(&self) -> Option<String> {
        let session = self.session?;
        let window_index = self.window_index?;
        Some(bool_string(
            self.options?.resolve_for_window(
                session.name(),
                window_index,
                OptionName::SynchronizePanes,
            ) == Some("on"),
        ))
    }

    pub(super) fn pane_style_colour(&self, foreground: bool) -> Option<String> {
        let session = self.session?;
        let pane = self.pane?;
        let window_index = self.window_index?;
        let options = self.options?;
        let mut style = Style::default();
        let base = StyleCell::default();
        for option in [OptionName::WindowStyle, OptionName::WindowActiveStyle] {
            if option == OptionName::WindowActiveStyle
                && self
                    .window
                    .is_some_and(|window| window.active_pane_index() != pane.index())
            {
                continue;
            }
            let Some(value) =
                options.resolve_for_pane(session.name(), window_index, pane.index(), option)
            else {
                continue;
            };
            if value.is_empty() || value == "default" {
                continue;
            }
            let _ = style.parse_in_place(&base, value);
        }
        Some(colour_to_string(if foreground {
            style.cell.fg
        } else {
            style.cell.bg
        }))
    }

    /// The rank of this window in the session's visit history, counted from 1.
    ///
    /// tmux walks `s->lastw` counting from one and answers 0 for a link that is
    /// not in it (`format.c:662-684`), which is the current window and any window
    /// that was never visited.
    pub(super) fn window_stack_index(&self) -> Option<String> {
        let session = self.session?;
        let window_index = self.window_index?;
        let rank = session
            .window_visit_stack()
            .iter()
            .position(|visited| *visited == window_index)
            .map_or(0, |position| position + 1);
        Some(rank.to_string())
    }

    /// The columns where the *targeted* pane actually holds a tab stop, read
    /// from its live bitmap under the transcript lock. tmux enumerates the set
    /// bits from column 0 and yields nothing at all when none is set
    /// (`format.c:981-1006`), so a pane that has cleared its stops reports an
    /// empty value rather than the default grid.
    pub(super) fn pane_tabs(&self) -> Option<String> {
        self.with_observed_pane_screen(|screen| {
            screen
                .tab_stops()
                .iter()
                .enumerate()
                .filter(|(_, stop)| **stop)
                .map(|(column, _)| column.to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
    }
}
