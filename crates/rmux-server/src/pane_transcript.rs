use std::sync::{Arc, Mutex};

use crate::clock_mode::{ClockModeState, CLOCK_MODE_NAME};
use crate::copy_mode::{CopyModeState, CopyModeSummary};
use rmux_core::{input::InputParser, GridRenderOptions, Screen, ScreenCaptureRange, Utf8Config};
use rmux_proto::TerminalSize;

pub(crate) type SharedPaneTranscript = Arc<Mutex<PaneTranscript>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneModeState {
    Copy(Box<CopyModeState>),
    Clock(ClockModeState),
    ModeTree(&'static str),
}

pub(crate) struct PaneTranscript {
    parser: InputParser,
    screen: Screen,
    mode: Option<PaneModeState>,
    output_sequence: u64,
    next_clock_generation: u64,
    clear_on_dead_exit: bool,
    #[cfg(test)]
    utf8_config: Utf8Config,
}

impl std::fmt::Debug for PaneTranscript {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneTranscript")
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

impl PaneTranscript {
    pub(crate) fn new(limit: usize, size: TerminalSize) -> Self {
        Self {
            parser: InputParser::new(),
            screen: Screen::new(size, limit),
            mode: None,
            output_sequence: 0,
            next_clock_generation: 1,
            clear_on_dead_exit: false,
            #[cfg(test)]
            utf8_config: Utf8Config::default(),
        }
    }

    pub(crate) fn shared(limit: usize, size: TerminalSize) -> SharedPaneTranscript {
        Arc::new(Mutex::new(Self::new(limit, size)))
    }

    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.screen.set_history_limit(limit);
    }

    pub(crate) fn append_bytes(&mut self, bytes: &[u8]) -> u64 {
        if !bytes.is_empty() {
            self.output_sequence = self.output_sequence.saturating_add(1);
        }
        self.parser.parse(bytes, &mut self.screen);
        self.screen.take_bell_count()
    }

    pub(crate) const fn output_sequence(&self) -> u64 {
        self.output_sequence
    }

    pub(crate) fn set_utf8_config(&mut self, utf8_config: Utf8Config) {
        self.screen.set_utf8_config(utf8_config.clone());
        if let Some(PaneModeState::Copy(copy_mode)) = &mut self.mode {
            copy_mode.set_utf8_config(utf8_config.clone());
        }
        #[cfg(test)]
        {
            self.utf8_config = utf8_config;
        }
    }

    pub(crate) fn capture_main(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Vec<u8> {
        self.screen.capture_transcript(range, options)
    }

    pub(crate) fn capture_saved(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        self.screen.capture_saved_transcript(range, options)
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
        self.parser.pending_bytes()
    }

    pub(crate) fn clear_history(&mut self, reset_hyperlinks: bool) {
        self.screen.clear_history_and_hyperlinks(reset_hyperlinks);
    }

    pub(crate) fn mark_clear_on_dead_exit(&mut self) {
        self.clear_on_dead_exit = true;
    }

    pub(crate) fn clear_for_dead_exit_if_marked(&mut self) -> bool {
        if !std::mem::take(&mut self.clear_on_dead_exit) {
            return false;
        }

        self.parser = InputParser::new();
        self.mode = None;
        self.screen.clear_history_and_hyperlinks(true);
        // tmux drops the top visible row for remain-on-exit respawn deaths.
        let _ = self.screen.delete_visible_line(0);
        true
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
            return self.screen.delete_absolute_line(absolute_y);
        }

        (0..self.screen.absolute_line_count())
            .rev()
            .find(|candidate| self.absolute_line_matches(*candidate, submitted_text))
            .is_some_and(|candidate| self.screen.delete_absolute_line(candidate))
    }

    pub(crate) fn history_limit(&self) -> usize {
        self.screen.history_limit()
    }

    pub(crate) fn history_size(&self) -> usize {
        self.screen.history_size()
    }

    pub(crate) fn history_bytes(&self) -> usize {
        self.screen.history_bytes()
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) {
        self.screen.resize(size);
        if let Some(PaneModeState::Copy(copy_mode)) = &mut self.mode {
            copy_mode.resize(size);
        }
    }

    pub(crate) fn clone_screen(&self) -> Screen {
        self.screen.clone()
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
        self.screen.mode()
    }

    pub(crate) fn cursor_style(&self) -> u32 {
        self.screen.cursor_style()
    }

    pub(crate) fn is_alternate(&self) -> bool {
        self.screen.is_alternate()
    }

    pub(crate) fn title(&self) -> &str {
        self.screen.title()
    }

    pub(crate) fn set_title(&mut self, title: impl Into<String>) {
        self.screen.set_title(title);
    }

    pub(crate) fn path(&self) -> &str {
        self.screen.path()
    }

    fn absolute_line_matches(&self, absolute_y: usize, submitted_text: &str) -> bool {
        let Some(line) = self.screen.absolute_line_view(absolute_y) else {
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
        self.screen = screen;
        self.mode = None;
    }

    #[cfg(test)]
    pub(crate) fn utf8_config(&self) -> &Utf8Config {
        &self.utf8_config
    }
}

#[cfg(test)]
mod tests {
    use super::PaneTranscript;
    use rmux_core::{GridRenderOptions, Screen, ScreenCaptureRange};
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

        let mut mode_screen = Screen::new(TerminalSize { cols: 8, rows: 2 }, 10);
        let mut parser = rmux_core::input::InputParser::new();
        parser.parse(b"mode\n", &mut mode_screen);
        transcript.set_copy_mode_screen_for_test(Some(mode_screen));

        let capture = transcript
            .capture_copy_mode(ScreenCaptureRange::default(), GridRenderOptions::default())
            .expect("copy mode capture exists");
        assert!(String::from_utf8(capture).expect("utf8").contains("mode"));
    }
}
