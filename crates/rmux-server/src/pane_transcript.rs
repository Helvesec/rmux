use std::sync::{Arc, Mutex};

use crate::clock_mode::{ClockModeState, CLOCK_MODE_NAME};
use crate::copy_mode::{CopyModeState, CopyModeSummary};
use rmux_core::{
    style::Style, GridRenderOptions, Screen, ScreenCaptureRange, TerminalPassthrough,
    TerminalScreen, Utf8Config,
};
use rmux_proto::TerminalSize;

pub(crate) type SharedPaneTranscript = Arc<Mutex<PaneTranscript>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneModeState {
    Copy(Box<CopyModeState>),
    Clock(ClockModeState),
    ModeTree(&'static str),
}

pub(crate) struct PaneTranscript {
    terminal: TerminalScreen,
    mode: Option<PaneModeState>,
    output_sequence: u64,
    next_clock_generation: u64,
    #[cfg(test)]
    utf8_config: Utf8Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PaneAppendResult {
    pub(crate) bell_count: u64,
    pub(crate) title_changed: bool,
    pub(crate) passthroughs: Vec<TerminalPassthrough>,
    pub(crate) dropped_passthrough_count: u64,
    pub(crate) replies: Vec<u8>,
}

pub(crate) struct PaneTranscriptRenderState {
    pub(crate) cursor_position: (u32, u32),
    pub(crate) cursor_style: u32,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) mode: u32,
    pub(crate) has_selected_cells: bool,
}

pub(crate) struct PaneVisibleLineCapture {
    pub(crate) revision: u64,
    pub(crate) rendered: Option<Vec<u8>>,
    pub(crate) previous_row: Option<usize>,
}

impl std::fmt::Debug for PaneTranscript {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneTranscript")
            .field("screen", self.terminal.screen())
            .finish_non_exhaustive()
    }
}

impl PaneTranscript {
    pub(crate) fn new(limit: usize, size: TerminalSize) -> Self {
        Self {
            terminal: TerminalScreen::new(size, limit),
            mode: None,
            output_sequence: 0,
            next_clock_generation: 1,
            #[cfg(test)]
            utf8_config: Utf8Config::default(),
        }
    }

    pub(crate) fn shared(limit: usize, size: TerminalSize) -> SharedPaneTranscript {
        Arc::new(Mutex::new(Self::new(limit, size)))
    }

    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.terminal.screen_mut().set_history_limit(limit);
    }

    pub(crate) fn append_bytes(&mut self, bytes: &[u8]) -> u64 {
        self.append_bytes_with_effects(bytes).bell_count
    }

    #[cfg(test)]
    pub(crate) fn append_bytes_and_take_replies(&mut self, bytes: &[u8]) -> PaneAppendResult {
        self.append_bytes_with_effects(bytes)
    }

    pub(crate) fn append_bytes_with_effects(&mut self, bytes: &[u8]) -> PaneAppendResult {
        if !bytes.is_empty() {
            self.output_sequence = self.output_sequence.saturating_add(1);
        }
        let title_before = self.terminal.screen().title().to_owned();
        self.terminal.feed(bytes);
        let title_changed = self.terminal.screen().title() != title_before;
        let passthroughs = self.terminal.take_terminal_passthrough();
        let dropped_passthrough_count = self.terminal.take_terminal_passthrough_dropped_count();
        let replies = self.terminal.take_replies();
        PaneAppendResult {
            bell_count: self.terminal.screen_mut().take_bell_count(),
            title_changed,
            passthroughs,
            dropped_passthrough_count,
            replies,
        }
    }

    pub(crate) fn reset_terminal_state(&mut self) {
        self.output_sequence = self.output_sequence.saturating_add(1);
        self.terminal.feed(b"\x1bc");
        let _ = self.terminal.take_terminal_passthrough();
        let _ = self.terminal.take_terminal_passthrough_dropped_count();
        let _ = self.terminal.take_replies();
        self.mode = None;
    }

    pub(crate) const fn output_sequence(&self) -> u64 {
        self.output_sequence
    }

    pub(crate) fn set_utf8_config(&mut self, utf8_config: Utf8Config) {
        self.terminal.set_utf8_config(utf8_config.clone());
        if let Some(PaneModeState::Copy(copy_mode)) = &mut self.mode {
            copy_mode.set_utf8_config(utf8_config.clone());
        }
        #[cfg(test)]
        {
            self.utf8_config = utf8_config;
        }
    }

    pub(crate) fn set_alternate_screen_enabled(&mut self, enabled: bool) {
        self.terminal.set_alternate_screen_enabled(enabled);
    }

    pub(crate) fn set_title_rename_enabled(&mut self, enabled: bool) {
        self.terminal.set_title_rename_enabled(enabled);
    }

    pub(crate) fn set_input_buffer_limit(&mut self, limit: usize) {
        self.terminal.set_input_buffer_limit(limit);
    }

    pub(crate) fn capture_main(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Vec<u8> {
        self.terminal.screen().capture_transcript(range, options)
    }

    pub(crate) fn capture_main_visible_line_changes(
        &self,
        rows: usize,
        options: GridRenderOptions,
        previous_revisions: Option<&[u64]>,
        default_style: Option<&Style>,
    ) -> Vec<PaneVisibleLineCapture> {
        let screen = self.terminal.screen();
        let revisions = (0..rows)
            .map(|row| screen.visible_line_revision(row).unwrap_or(0))
            .collect::<Vec<_>>();
        let previous_rows = previous_revisions.and_then(|previous| {
            visible_revision_reuse_map(previous, &revisions).filter(|map| map.len() == rows)
        });

        (0..rows)
            .map(|row| {
                let previous_row = previous_rows
                    .as_ref()
                    .and_then(|rows| rows.get(row))
                    .and_then(|row| *row);
                let rendered = if previous_row.is_some() {
                    None
                } else {
                    render_visible_line(screen, row, options, default_style)
                };
                PaneVisibleLineCapture {
                    revision: revisions[row],
                    rendered,
                    previous_row,
                }
            })
            .collect()
    }

    pub(crate) fn render_state(&self) -> PaneTranscriptRenderState {
        let screen = self.terminal.screen();
        PaneTranscriptRenderState {
            cursor_position: screen.cursor_position(),
            cursor_style: screen.cursor_style(),
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            mode: screen.mode(),
            has_selected_cells: screen.has_selected_cells(),
        }
    }

    pub(crate) fn capture_saved(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        self.terminal
            .screen()
            .capture_saved_transcript(range, options)
    }

    pub(crate) fn capture_copy_mode(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => {
                Some(mode.render_screen().capture_transcript(range, options))
            }
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree(_)) | None => None,
        }
    }

    pub(crate) fn pending_bytes(&self) -> Vec<u8> {
        self.terminal.pending_bytes()
    }

    pub(crate) fn clear_history(&mut self, reset_hyperlinks: bool) {
        self.terminal
            .screen_mut()
            .clear_history_and_hyperlinks(reset_hyperlinks);
    }

    pub(crate) fn trim_below_cursor(&mut self) -> bool {
        let trimmed = self.terminal.screen_mut().trim_below_cursor();
        if trimmed {
            self.output_sequence = self.output_sequence.saturating_add(1);
        }
        trimmed
    }

    pub(crate) fn delete_attached_submitted_line(
        &mut self,
        absolute_y: usize,
        submitted_text: &str,
    ) -> bool {
        if submitted_text.is_empty() {
            return false;
        }
        if self.absolute_line_matches(absolute_y, submitted_text) {
            return self.terminal.screen_mut().delete_absolute_line(absolute_y);
        }

        (0..self.terminal.screen().absolute_line_count())
            .rev()
            .find(|candidate| self.absolute_line_matches(*candidate, submitted_text))
            .is_some_and(|candidate| self.terminal.screen_mut().delete_absolute_line(candidate))
    }

    pub(crate) fn history_limit(&self) -> usize {
        self.terminal.screen().history_limit()
    }

    pub(crate) fn history_size(&self) -> usize {
        self.terminal.screen().history_size()
    }

    pub(crate) fn tmux_history_bytes(&self) -> usize {
        self.terminal.screen().tmux_history_bytes()
    }

    pub(crate) fn tmux_history_all_bytes(&self) -> String {
        self.terminal.screen().tmux_history_all_bytes()
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.terminal.resize(size);
        if let Some(PaneModeState::Copy(copy_mode)) = &mut self.mode {
            copy_mode.resize(size);
        }
    }

    pub(crate) fn clone_screen(&self) -> Screen {
        self.terminal.screen().clone()
    }

    pub(crate) fn screen(&self) -> &Screen {
        self.terminal.screen()
    }

    pub(crate) fn copy_mode_state(&self) -> Option<&CopyModeState> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => Some(mode.as_ref()),
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree(_)) | None => None,
        }
    }

    pub(crate) fn copy_mode_state_mut(&mut self) -> Option<&mut CopyModeState> {
        match &mut self.mode {
            Some(PaneModeState::Copy(mode)) => Some(mode.as_mut()),
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree(_)) | None => None,
        }
    }

    pub(crate) fn set_copy_mode_state(&mut self, state: Option<CopyModeState>) {
        self.mode = state.map(Box::new).map(PaneModeState::Copy);
    }

    pub(crate) fn copy_mode_summary(&self) -> Option<CopyModeSummary> {
        self.copy_mode_state().map(CopyModeState::summary)
    }

    pub(crate) fn copy_mode_render_screen(&self) -> Option<Screen> {
        self.copy_mode_state().map(CopyModeState::render_screen)
    }

    pub(crate) fn clear_copy_mode(&mut self) -> bool {
        match self.mode {
            Some(PaneModeState::Copy(_)) => {
                self.mode = None;
                true
            }
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree(_)) | None => false,
        }
    }

    pub(crate) fn enter_clock_mode(&mut self) -> u64 {
        let generation = self.next_clock_generation;
        self.next_clock_generation = self.next_clock_generation.saturating_add(1);
        self.mode = Some(PaneModeState::Clock(ClockModeState::new(generation)));
        generation
    }

    pub(crate) fn clock_mode_generation(&self) -> Option<u64> {
        match self.mode {
            Some(PaneModeState::Clock(mode)) => Some(mode.generation()),
            Some(PaneModeState::Copy(_) | PaneModeState::ModeTree(_)) | None => None,
        }
    }

    pub(crate) fn clear_clock_mode(&mut self) -> bool {
        match self.mode {
            Some(PaneModeState::Clock(_)) => {
                self.mode = None;
                true
            }
            Some(PaneModeState::Copy(_) | PaneModeState::ModeTree(_)) | None => false,
        }
    }

    pub(crate) fn enter_mode_tree(&mut self, mode_name: &'static str) -> bool {
        let changed = self.pane_mode_name() != Some(mode_name);
        self.mode = Some(PaneModeState::ModeTree(mode_name));
        changed
    }

    pub(crate) fn clear_mode_tree(&mut self) -> bool {
        match self.mode {
            Some(PaneModeState::ModeTree(_)) => {
                self.mode = None;
                true
            }
            Some(PaneModeState::Copy(_) | PaneModeState::Clock(_)) | None => false,
        }
    }

    pub(crate) fn pane_in_mode(&self) -> bool {
        self.mode.is_some()
    }

    pub(crate) fn pane_mode_name(&self) -> Option<&'static str> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => Some(if mode.view_mode() {
                "view-mode"
            } else {
                "copy-mode"
            }),
            Some(PaneModeState::Clock(_)) => Some(CLOCK_MODE_NAME),
            Some(PaneModeState::ModeTree(mode_name)) => Some(mode_name),
            None => None,
        }
    }

    pub(crate) fn mode(&self) -> u32 {
        self.terminal.screen().mode()
    }

    pub(crate) fn cursor_style(&self) -> u32 {
        self.terminal.screen().cursor_style()
    }

    pub(crate) fn cursor_position(&self) -> (u32, u32) {
        self.terminal.screen().cursor_position()
    }

    pub(crate) fn is_alternate(&self) -> bool {
        self.terminal.screen().is_alternate()
    }

    pub(crate) fn title(&self) -> &str {
        self.terminal.screen().title()
    }

    pub(crate) fn set_title(&mut self, title: impl Into<String>) {
        self.terminal.screen_mut().set_title(title);
    }

    pub(crate) fn path(&self) -> &str {
        self.terminal.screen().path()
    }

    fn absolute_line_matches(&self, absolute_y: usize, submitted_text: &str) -> bool {
        let Some(line) = self.terminal.screen().absolute_line_view(absolute_y) else {
            return false;
        };
        let rendered = line
            .cells()
            .iter()
            .filter(|cell| !cell.is_padding())
            .map(|cell| cell.text())
            .collect::<String>();
        rendered.trim_end().ends_with(submitted_text)
    }

    #[cfg(test)]
    pub(crate) fn set_copy_mode_screen_for_test(&mut self, screen: Option<Screen>) {
        self.mode = screen
            .map(CopyModeState::for_test)
            .map(Box::new)
            .map(PaneModeState::Copy);
    }

    #[cfg(test)]
    pub(crate) fn set_screen_for_test(&mut self, mut screen: Screen) {
        screen.set_utf8_config(self.utf8_config.clone());
        *self.terminal.screen_mut() = screen;
        self.mode = None;
    }

    #[cfg(test)]
    pub(crate) fn utf8_config(&self) -> &Utf8Config {
        &self.utf8_config
    }
}

fn render_visible_line(
    screen: &Screen,
    row: usize,
    options: GridRenderOptions,
    default_style: Option<&Style>,
) -> Option<Vec<u8>> {
    if let Some(style) = default_style {
        screen.render_visible_line_independent_with_default_style(row, options, style)
    } else {
        screen.render_visible_line_independent(row, options)
    }
}

fn visible_revision_reuse_map(previous: &[u64], next: &[u64]) -> Option<Vec<Option<usize>>> {
    if previous.len() != next.len() {
        return None;
    }
    let rows = next.len();
    let mut map = vec![None; rows];
    for (row, entry) in map.iter_mut().enumerate() {
        if previous[row] == next[row] {
            *entry = Some(row);
        }
    }

    if let Some(scroll_rows) =
        (1..rows).find(|scroll_rows| previous[*scroll_rows..] == next[..rows - *scroll_rows])
    {
        for (row, entry) in map.iter_mut().enumerate().take(rows - scroll_rows) {
            *entry = Some(row + scroll_rows);
        }
        return Some(map);
    }

    if let Some(scroll_rows) =
        (1..rows).find(|scroll_rows| previous[..rows - *scroll_rows] == next[*scroll_rows..])
    {
        for (row, entry) in map.iter_mut().enumerate().skip(scroll_rows) {
            *entry = Some(row - scroll_rows);
        }
    }

    Some(map)
}

#[cfg(test)]
mod tests {
    use super::{visible_revision_reuse_map, PaneTranscript};
    use rmux_core::{GridRenderOptions, ScreenCaptureRange, TerminalScreen};
    use rmux_proto::TerminalSize;

    fn transcript(cols: u16, rows: u16, limit: usize) -> PaneTranscript {
        PaneTranscript::new(limit, TerminalSize { cols, rows })
    }

    #[test]
    fn capture_defaults_to_visible_rows() {
        let mut transcript = transcript(8, 2, 10);
        transcript.append_bytes(b"one\r\ntwo\r\nthree\r\n");

        assert_eq!(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
            b"three\n\n"
        );
    }

    #[test]
    fn visible_line_capture_reuses_shifted_rows_after_scroll_up() {
        let previous = [10, 11, 12, 13];
        let next = [11, 12, 13, 14];

        assert_eq!(
            visible_revision_reuse_map(&previous, &next),
            Some(vec![Some(1), Some(2), Some(3), None])
        );
    }

    #[test]
    fn visible_line_capture_reuses_shifted_rows_after_scroll_down() {
        let previous = [10, 11, 12, 13];
        let next = [9, 10, 11, 12];

        assert_eq!(
            visible_revision_reuse_map(&previous, &next),
            Some(vec![None, Some(0), Some(1), Some(2)])
        );
    }

    #[test]
    fn append_bytes_reports_kitty_graphics_passthrough_without_capturing_text() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1b[2;3H\x1b_Gf=100;AAAA\x1b\\");

        assert_eq!(result.passthroughs.len(), 1);
        assert_eq!(result.passthroughs[0].payload(), b"Gf=100;AAAA");
        let capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("capture is utf8");
        assert!(!capture.contains("Gf=100"));
    }

    #[test]
    fn append_bytes_reports_sixel_passthrough_without_capturing_text() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1b[2;3H\x1bPq#0!10~\x1b\\");

        assert_eq!(result.passthroughs.len(), 1);
        assert_eq!(result.passthroughs[0].payload(), b"q#0!10~");
        let capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("capture is utf8");
        assert!(!capture.contains("#0!10~"));
    }

    #[test]
    fn append_bytes_reports_dcs_passthrough_without_capturing_text() {
        let mut transcript = transcript(40, 4, 10);
        let result =
            transcript.append_bytes_with_effects(b"\x1b[2;3H\x1bPtmux;\x1b]52;c;QQ==\x07\x1b\\");

        assert_eq!(result.passthroughs.len(), 1);
        assert_eq!(result.passthroughs[0].payload(), b"\x1b]52;c;QQ==\x07");
        let capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("capture is utf8");
        assert!(!capture.contains("52;c"));
    }

    #[test]
    fn append_bytes_decodes_dcs_passthrough_doubled_inner_escape() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript
            .append_bytes_with_effects(b"\x1b[2;3H\x1bPtmux;\x1b\x1b]52;c;QQ==\x07\x1b\\");

        assert_eq!(result.passthroughs.len(), 1);
        assert_eq!(result.passthroughs[0].payload(), b"\x1b]52;c;QQ==\x07");
    }

    #[test]
    fn append_bytes_reports_osc52_clipboard_without_capturing_text() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1b]52;c;QQ==\x07");

        assert_eq!(result.passthroughs.len(), 1);
        assert_eq!(result.passthroughs[0].payload(), b"\x1b]52;c;QQ==\x07");
        let capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("capture is utf8");
        assert!(!capture.contains("52;c"));
    }

    #[test]
    fn append_bytes_reports_dropped_oversized_kitty_passthroughs() {
        let mut transcript = transcript(40, 4, 10);
        assert_eq!(
            transcript
                .append_bytes_with_effects(b"\x1b_G")
                .dropped_passthrough_count,
            0
        );

        let chunk = vec![b'A'; 8 * 1024 * 1024 + 1];
        let result = transcript.append_bytes_with_effects(&chunk);

        assert!(result.passthroughs.is_empty());
        assert_eq!(result.dropped_passthrough_count, 1);
        assert_eq!(
            transcript
                .append_bytes_with_effects(b"\x1b\\")
                .dropped_passthrough_count,
            0
        );
    }

    #[test]
    fn append_bytes_reports_terminal_replies() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1b[c");

        assert_eq!(result.replies, b"\x1b[?1;2c");
        assert!(transcript.append_bytes_with_effects(b"").replies.is_empty());
    }

    #[test]
    fn kitty_passthrough_batches_keep_da_reply_for_child() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1b_Ga=q,f=24,i=1;MTIz\x1b\\\x1b[c");

        assert_eq!(result.replies, b"\x1b[?1;2c");
        assert_eq!(result.passthroughs.len(), 1);
        assert_eq!(
            result.passthroughs[0].render_sequence(),
            b"\x1b_Ga=q,f=24,i=1;MTIz\x1b\\"
        );
    }

    #[test]
    fn absolute_capture_includes_scrolled_history() {
        let mut transcript = transcript(8, 2, 10);
        transcript.append_bytes(b"one\r\ntwo\r\nthree\r\n");

        let range = ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        };
        assert_eq!(
            transcript.capture_main(range, GridRenderOptions::default()),
            b"one\ntwo\nthree\n\n"
        );
    }

    #[test]
    fn alternate_screen_keeps_saved_visible_grid() {
        let mut transcript = transcript(8, 2, 10);
        transcript.append_bytes(b"main\n");
        transcript.append_bytes(b"\x1b[?1049h");
        transcript.append_bytes(b"alt\n");

        let capture = String::from_utf8(
            transcript
                .capture_saved(ScreenCaptureRange::default(), GridRenderOptions::default())
                .expect("alternate capture exists"),
        )
        .expect("utf8");
        assert!(capture.contains("main"));
        assert!(!capture.contains("alt"));
    }

    #[test]
    fn disabled_alternate_screen_keeps_output_on_main_grid() {
        let mut transcript = transcript(16, 4, 10);
        transcript.set_alternate_screen_enabled(false);

        transcript.append_bytes(b"\x1b[?1049hALTLINE\r\n\x1b[?1049lMAINLINE\r\n");

        let capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("utf8");
        assert!(capture.contains("ALTLINE"));
        assert!(capture.contains("MAINLINE"));
        assert!(!transcript.is_alternate());
    }

    #[test]
    fn history_limit_evicts_oldest_rows() {
        let mut transcript = transcript(8, 1, 2);
        transcript.append_bytes(b"zero\r\none\r\ntwo\r\nthree\r\n");

        assert_eq!(transcript.history_size(), 2);
        let range = ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        };
        assert_eq!(
            transcript.capture_main(range, GridRenderOptions::default()),
            b"two\nthree\n\n"
        );
    }

    #[test]
    fn copy_mode_capture_prefers_mode_screen() {
        let mut transcript = transcript(8, 2, 10);
        transcript.append_bytes(b"base\n");

        let mut mode_terminal = TerminalScreen::new(TerminalSize { cols: 8, rows: 2 }, 10);
        mode_terminal.feed(b"mode\n");
        let mode_screen = mode_terminal.screen().clone();
        transcript.set_copy_mode_screen_for_test(Some(mode_screen));

        let capture = transcript
            .capture_copy_mode(ScreenCaptureRange::default(), GridRenderOptions::default())
            .expect("copy mode capture exists");
        assert!(String::from_utf8(capture).expect("utf8").contains("mode"));
    }

    #[test]
    fn append_bytes_drains_terminal_replies_once() {
        let mut transcript = transcript(8, 2, 10);

        let result = transcript.append_bytes_and_take_replies(b"\x1b[c");
        assert_eq!(result.replies, b"\x1b[?1;2c");

        let result = transcript.append_bytes_and_take_replies(b"");
        assert!(result.replies.is_empty());
    }

    #[test]
    fn append_bytes_reports_title_changes_once_per_change() {
        let mut transcript = transcript(8, 2, 10);

        let result = transcript.append_bytes_and_take_replies(b"\x1b]2;alpha\x07");
        assert!(result.title_changed);

        let result = transcript.append_bytes_and_take_replies(b"\x1b]2;alpha\x07");
        assert!(!result.title_changed);

        let result = transcript.append_bytes_and_take_replies(b"\x1b]2;beta\x07");
        assert!(result.title_changed);
    }
}
