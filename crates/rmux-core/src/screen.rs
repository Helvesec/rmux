//! Screen state and `ScreenWriter` implementation backed by [`Grid`].

use crate::grid::Grid;
use crate::hyperlinks::Hyperlinks;
use crate::input::{mode, CellState, COLOUR_DEFAULT};
use crate::terminal_passthrough::TerminalPassthrough;
use crate::utf8::Utf8Config;
use rmux_proto::TerminalSize;

mod acs;
mod capture;
mod combine;
mod cursor;
mod erase;
mod history_bytes;
mod line_deletion;
mod metadata;
mod passthrough;
mod print;
mod selection;
mod style_overlay;
mod synchronized_output;
mod tty_draw;
mod view;
mod wrap;
mod writer;

pub use capture::{LineFormatCapture, RecoveryRow, RecoveryRowRenderer};
pub use view::{ScreenCellRef, ScreenCellView, ScreenLineView};

pub(crate) const MAX_TERMINAL_PASSTHROUGH_EVENTS: usize = 256;
const TITLE_STACK_MAX: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SavedGrid {
    grid: Grid,
    history_enabled: bool,
}

/// One pane screen, including scrollback, alternate-screen state, cursor
/// position, modes, tab stops, and hyperlink storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    cell_pixels: Option<rmux_types::TerminalPixels>,
    sixel_passthrough_available: bool,
    grid: Grid,
    cursor_x: u32,
    cursor_y: u32,
    pending_wrap: bool,
    saved_cursor_x: Option<u32>,
    saved_cursor_y: Option<u32>,
    saved_cursor_pending_wrap: bool,
    /// Rendition remembered on alternate-screen entry, tmux `saved_cell`.
    saved_rendition: CellState,
    saved_grid: Option<SavedGrid>,
    projected_capture_boundary_after: Option<usize>,
    rupper: u32,
    rlower: u32,
    mode: u32,
    keyboard: crate::kitty_keyboard::PaneKeyboard,
    extended_key_policy: crate::input::ExtendedKeyPolicy,
    cursor_style: u32,
    cursor_default_style: u32,
    title: String,
    program_status: crate::ProgramStatus,
    window_name: String,
    path: String,
    title_stack: Vec<String>,
    title_rename_enabled: bool,
    allow_rename: bool,
    window_rename_requests: Vec<String>,
    tabs: Vec<bool>,
    hyperlinks: Hyperlinks,
    active_hyperlink: u32,
    metadata_revision: u64,
    bell_count: u64,
    terminal_passthrough: Vec<TerminalPassthrough>,
    dropped_terminal_passthrough_count: u64,
    has_selected_cells: bool,
    utf8_config: Utf8Config,
    alternate_screen_enabled: bool,
    preserve_alternate_screen_cursor: bool,
    scroll_on_clear: bool,
    synchronized_output_snapshot: Option<Box<Screen>>,
    synchronized_output_started: bool,
}

impl Screen {
    /// Sets whether queries may advertise a native SIXEL passthrough path.
    pub fn set_sixel_passthrough_available(&mut self, available: bool) {
        self.sixel_passthrough_available = available;
    }

    pub(crate) fn render_cell_state_ansi(&self, state: &CellState) -> Vec<u8> {
        crate::grid::render_cell_state_ansi(state, &self.hyperlinks)
    }

    pub(crate) fn render_cell_state_ansi_bounded(
        &self,
        state: &CellState,
        max_hyperlink_bytes: usize,
    ) -> (Vec<u8>, bool) {
        let mut bounded = state.clone();
        let complete = self
            .hyperlinks
            .entry_fits(bounded.link(), max_hyperlink_bytes);
        if !complete {
            bounded.cell.link = 0;
        }
        (
            crate::grid::render_cell_state_ansi(&bounded, &self.hyperlinks),
            complete,
        )
    }

    /// Records the outer client's pixel dimensions per cell for window queries.
    pub fn set_cell_pixels(&mut self, pixels: Option<rmux_types::TerminalPixels>) {
        self.cell_pixels = pixels;
    }

    /// Creates a new screen with the given geometry and history limit.
    #[must_use]
    pub fn new(size: TerminalSize, history_limit: usize) -> Self {
        let grid = Grid::new(size, history_limit);
        let mut screen = Self {
            grid,
            cell_pixels: None,
            sixel_passthrough_available: false,
            cursor_x: 0,
            cursor_y: 0,
            pending_wrap: false,
            saved_cursor_x: None,
            saved_cursor_y: None,
            saved_cursor_pending_wrap: false,
            saved_rendition: CellState::default(),
            saved_grid: None,
            projected_capture_boundary_after: None,
            rupper: 0,
            rlower: u32::from(size.rows.max(1)).saturating_sub(1),
            mode: mode::MODE_CURSOR | mode::MODE_WRAP,
            keyboard: crate::kitty_keyboard::PaneKeyboard::default(),
            extended_key_policy: crate::input::ExtendedKeyPolicy::default(),
            cursor_style: 0,
            cursor_default_style: 0,
            title: String::new(),
            program_status: crate::ProgramStatus::default(),
            window_name: String::new(),
            path: String::new(),
            title_stack: Vec::new(),
            title_rename_enabled: true,
            allow_rename: false,
            window_rename_requests: Vec::new(),
            tabs: Vec::new(),
            hyperlinks: Hyperlinks::new(),
            active_hyperlink: 0,
            metadata_revision: 0,
            bell_count: 0,
            terminal_passthrough: Vec::new(),
            dropped_terminal_passthrough_count: 0,
            has_selected_cells: false,
            utf8_config: Utf8Config::default(),
            alternate_screen_enabled: true,
            preserve_alternate_screen_cursor: false,
            scroll_on_clear: true,
            synchronized_output_snapshot: None,
            synchronized_output_started: false,
        };
        screen.reset_tabs();
        screen
    }

    /// Returns the current terminal mode flags.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Puts this screen in the state a pane with no process has.
    ///
    /// The source does exactly this where it declines to fork
    /// (`spawn.c:373-376`): the cursor of an empty pane is not shown, and a
    /// line feed also returns the carriage, so text written into such a pane
    /// reads as its writer wrote it without anything rewriting its bytes.
    pub const fn enter_empty_pane_state(&mut self) {
        self.mode &= !crate::input::mode::MODE_CURSOR;
        self.mode |= crate::input::mode::MODE_CRLF;
    }

    /// Hides the terminal cursor without writing bytes or changing line mode.
    pub fn hide_cursor(&mut self) -> bool {
        let was_visible = self.mode & crate::input::mode::MODE_CURSOR != 0;
        self.mode &= !crate::input::mode::MODE_CURSOR;
        was_visible
    }

    /// Application-requested enhancements; zero without `kitty-keyboard`.
    #[must_use]
    pub fn keyboard_flags(&self) -> u8 {
        self.keyboard.requested(self.keyboard_screen())
    }

    fn keyboard_screen(&self) -> crate::kitty_keyboard::Screen {
        if self.is_alternate() {
            crate::kitty_keyboard::Screen::Alternate
        } else {
            crate::kitty_keyboard::Screen::Main
        }
    }

    /// Returns the most recent DECSCUSR cursor style parameter.
    #[must_use]
    pub const fn cursor_style(&self) -> u32 {
        self.cursor_style
    }

    /// Returns the pane-configured DECSCUSR default used when the application
    /// has selected style zero.
    #[must_use]
    pub const fn cursor_default_style(&self) -> u32 {
        self.cursor_default_style
    }

    /// Returns the effective cursor style after inheritance and DEC mode 12.
    ///
    /// This pure read is the shared boundary for rendering and future terminal
    /// state queries; it does not mutate parser or server state.
    #[must_use]
    pub const fn resolved_cursor_style(&self) -> u32 {
        crate::resolve_cursor_style(self.cursor_style, self.mode, self.cursor_default_style)
    }

    /// Updates the pane-configured cursor default without changing the raw
    /// application DECSCUSR value, returning whether the default changed.
    pub fn set_cursor_default_style(&mut self, style: u32) -> bool {
        if !crate::cursor_style_is_valid(style) || self.cursor_default_style == style {
            return false;
        }
        self.cursor_default_style = style;
        if let Some(presentation) = self.synchronized_output_snapshot.as_mut() {
            presentation.cursor_default_style = style;
        }
        true
    }

    /// Returns the DECSTBM scroll region as `(top, bottom)` rows, 0-based and
    /// inclusive. A full-screen region is `(0, rows - 1)`.
    #[must_use]
    pub const fn scroll_region(&self) -> (u32, u32) {
        (self.rupper, self.rlower)
    }

    pub(crate) fn plain_output_forwarding_safe(&self) -> bool {
        let unsafe_modes = mode::MODE_INSERT | mode::MODE_CRLF | mode::MODE_SYNC;
        !self.pending_wrap
            && !self.combined_sequence_discard_at_cursor().is_active()
            && self.mode & mode::MODE_WRAP != 0
            && self.mode & unsafe_modes == 0
            && self.rupper == 0
            && self.rlower == self.grid.sy().saturating_sub(1)
    }

    /// Returns the screen size.
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        self.grid.size()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(crate) fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Sets the policy for subsequent application key reporting requests.
    pub fn set_extended_key_policy(&mut self, policy: crate::input::ExtendedKeyPolicy) {
        self.extended_key_policy = policy;
    }

    /// Enables or disables title changes requested by pane output.
    pub fn set_title_rename_enabled(&mut self, enabled: bool) {
        self.title_rename_enabled = enabled;
    }

    /// Enables or disables DEC alternate-screen entry for this screen.
    ///
    /// Exit sequences are still honored by the writer so disabling the option
    /// while a pane is already in alternate screen does not trap it there.
    pub fn set_alternate_screen_enabled(&mut self, enabled: bool) {
        self.alternate_screen_enabled = enabled;
    }

    pub(crate) fn set_preserve_alternate_screen_cursor(&mut self, enabled: bool) {
        self.preserve_alternate_screen_cursor = enabled;
    }

    /// Enables or disables scrolling the visible rows into history when the
    /// whole screen is erased, i.e. the tmux `scroll-on-clear` option.
    ///
    /// The synchronized-output snapshot is deliberately not updated: erases
    /// reach the screen through the [`ScreenWriter`] impl on `self`, never on
    /// the snapshot, which is discarded rather than restored when the
    /// synchronized update ends.
    ///
    /// [`ScreenWriter`]: crate::input::ScreenWriter
    pub fn set_scroll_on_clear(&mut self, enabled: bool) {
        self.scroll_on_clear = enabled;
    }

    /// Returns whether a whole-screen erase scrolls the visible rows into
    /// history.
    #[must_use]
    pub const fn scroll_on_clear(&self) -> bool {
        self.scroll_on_clear
    }

    /// Enables or disables window renames requested by pane output.
    ///
    /// This is the `allow-rename` option, which tmux resolves per pane and
    /// which gates `ESC k` alone. Titles keep their own `allow-set-title`
    /// policy, so a pane may rename its window with titles forbidden, or the
    /// reverse. The constructed default is tmux's `off`.
    pub fn set_allow_rename(&mut self, enabled: bool) {
        self.allow_rename = enabled;
    }

    /// Returns whether pane output may rename its window.
    #[must_use]
    pub const fn allow_rename(&self) -> bool {
        self.allow_rename
    }

    /// Returns whether the alternate screen is active.
    #[must_use]
    pub fn is_alternate(&self) -> bool {
        self.saved_grid.is_some()
    }

    fn capture_boundary_after(&self) -> Option<usize> {
        self.projected_capture_boundary_after.or_else(|| {
            self.is_alternate()
                .then(|| self.grid.hsize().checked_sub(1))
                .flatten()
        })
    }

    fn projected_capture_boundary(&self, top_line: usize, rows: usize) -> Option<usize> {
        self.capture_boundary_after()
            .filter(|boundary| (top_line..top_line.saturating_add(rows)).contains(boundary))
            .map(|boundary| boundary - top_line)
    }

    /// Returns the cursor saved by DEC alternate-screen entry, when present.
    #[must_use]
    pub fn alternate_saved_cursor(&self) -> Option<(u32, u32, bool)> {
        self.saved_cursor_x.zip(self.saved_cursor_y).map(|(x, y)| {
            (
                x,
                y,
                self.saved_cursor_pending_wrap && self.mode & mode::MODE_WRAP != 0,
            )
        })
    }

    /// Returns whether the next printable character first performs autowrap.
    #[must_use]
    pub const fn pending_wrap(&self) -> bool {
        self.pending_wrap
    }

    /// Returns the active tab-stop bitmap.
    #[must_use]
    pub fn tab_stops(&self) -> &[bool] {
        &self.tabs
    }

    /// Clones all state needed to reconstruct a renderer, including bounded
    /// scrollback and the saved main buffer beneath alternate screen.
    ///
    /// Ephemeral notifications and passthrough side effects are discarded:
    /// replaying a recovery keyframe must not replay clipboard, graphics, bell,
    /// or other host-side effects.
    #[must_use]
    pub(crate) fn clone_recovery_state(&self) -> Self {
        let mut recovery = self.clone();
        recovery.synchronized_output_snapshot = None;
        recovery.synchronized_output_started = false;
        recovery.bell_count = 0;
        recovery.terminal_passthrough.clear();
        recovery.dropped_terminal_passthrough_count = 0;
        recovery.has_selected_cells = false;
        recovery.clear_combined_sequence_discards();
        recovery
    }

    /// Returns the configured history limit.
    #[must_use]
    pub fn history_limit(&self) -> usize {
        self.grid.hlimit()
    }

    /// Returns the current history size in rows.
    #[must_use]
    pub fn history_size(&self) -> usize {
        self.grid.hsize()
    }

    /// Returns the current cursor position within the visible viewport.
    #[must_use]
    pub const fn cursor_position(&self) -> (u32, u32) {
        (self.cursor_x, self.cursor_y)
    }

    /// Returns the absolute cursor row including history.
    #[must_use]
    pub fn cursor_absolute_y(&self) -> usize {
        self.grid.hsize() + self.cursor_y as usize
    }

    /// Returns the total number of absolute lines retained by the screen.
    #[must_use]
    pub fn absolute_line_count(&self) -> usize {
        self.grid.hsize() + self.grid.sy() as usize
    }

    /// Returns the current retained history size in bytes.
    #[must_use]
    pub fn history_bytes(&self) -> usize {
        self.grid.history_byte_size()
    }

    /// Drains and returns the number of BEL notifications observed since the last drain.
    pub fn take_bell_count(&mut self) -> u64 {
        std::mem::take(&mut self.bell_count)
    }

    /// Returns the stored OSC 8 URI for a hyperlink inner ID.
    #[must_use]
    pub fn hyperlink_uri(&self, inner_id: u32) -> Option<&str> {
        self.hyperlinks
            .get(inner_id)
            .map(|entry| entry.uri.as_str())
    }

    /// Updates the history limit.
    pub fn set_history_limit(&mut self, limit: usize) {
        if let Some(presentation) = self.synchronized_output_snapshot.as_mut() {
            presentation.set_history_limit(limit);
        }
        self.grid.set_hlimit(limit);
    }

    /// Updates the tmux-style UTF-8 width and combining configuration.
    pub fn set_utf8_config(&mut self, utf8_config: Utf8Config) {
        if let Some(presentation) = self.synchronized_output_snapshot.as_mut() {
            presentation.set_utf8_config(utf8_config.clone());
        }
        self.utf8_config = utf8_config;
    }

    /// Resizes the screen and resets the scroll region.
    pub fn resize(&mut self, size: TerminalSize) {
        if let Some(presentation) = self.synchronized_output_snapshot.as_mut() {
            presentation.resize(size);
        }
        self.clear_selected_cells();
        let cols = u32::from(size.cols.max(1));
        let rows = u32::from(size.rows.max(1));
        if cols != self.grid.sx() {
            let remapped = if self.is_alternate() {
                self.grid.resize_visible_width_preserving_cursor(
                    cols,
                    COLOUR_DEFAULT,
                    self.cursor_y,
                    self.cursor_x,
                    self.pending_wrap,
                )
            } else {
                let cursor =
                    self.grid
                        .logical_cursor(self.cursor_y, self.cursor_x, self.pending_wrap);
                self.grid
                    .resize_width_remapping_cursor(cols, COLOUR_DEFAULT, cursor)
            };
            self.apply_grid_cursor(remapped);
            self.reset_tabs();
        }
        if rows != self.grid.sy() {
            self.grid
                .resize_height(rows, &mut self.cursor_y, COLOUR_DEFAULT);
        }
        self.rupper = 0;
        self.rlower = rows.saturating_sub(1);
        self.cursor_x = self.cursor_x.min(self.max_cursor_x());
        self.pending_wrap = self.pending_wrap
            && (self.mode & mode::MODE_WRAP) != 0
            && self.cursor_x == self.max_cursor_x();
    }

    /// Clears history and optionally resets stored hyperlinks.
    pub fn clear_history_and_hyperlinks(&mut self, reset_hyperlinks: bool) {
        if let Some(presentation) = self.synchronized_output_snapshot.as_mut() {
            presentation.clear_history_and_hyperlinks(reset_hyperlinks);
        }
        self.clear_selected_cells();
        self.grid.clear_history();
        if reset_hyperlinks {
            self.hyperlinks.reset();
            self.bump_metadata_revision();
        }
    }

    fn reset_tabs(&mut self) {
        self.tabs = vec![false; self.grid.sx() as usize];
        for column in (8..self.grid.sx()).step_by(8) {
            self.tabs[column as usize] = true;
        }
    }

    fn parse_hyperlink(data: &str) -> (Option<String>, String) {
        let (params, uri) = data.split_once(';').unwrap_or((data, ""));
        let mut internal_id = None;
        for part in params.split(':') {
            if let Some(value) = part.strip_prefix("id=") {
                internal_id = Some(value.to_owned());
            }
        }
        (internal_id, uri.to_owned())
    }
}

#[cfg(test)]
#[path = "screen/allow_rename_tests.rs"]
mod allow_rename_tests;

#[cfg(test)]
#[path = "screen/capture_ascii_space_tests.rs"]
mod capture_ascii_space_tests;
#[cfg(test)]
#[path = "screen/capture_mutation_tests.rs"]
mod capture_mutation_tests;
#[cfg(test)]
#[path = "screen/erase_background_tests.rs"]
mod erase_background_tests;
#[cfg(test)]
#[path = "screen/erase_tests.rs"]
mod erase_tests;
#[cfg(test)]
#[path = "screen/erase_usage_tests.rs"]
mod erase_usage_tests;
#[cfg(test)]
#[path = "screen/line_edit_outside_region_tests.rs"]
mod line_edit_outside_region_tests;
#[cfg(test)]
#[path = "screen/pending_wrap_edit_tests.rs"]
mod pending_wrap_edit_tests;
#[cfg(test)]
#[path = "screen/scroll_on_clear_tests.rs"]
mod scroll_on_clear_tests;
#[cfg(test)]
#[path = "screen/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "screen/reflow_extent_tests.rs"]
mod reflow_extent_tests;
