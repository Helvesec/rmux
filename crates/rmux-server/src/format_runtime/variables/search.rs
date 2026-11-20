//! Pane content search (`#{C:...}`) for runtime formats.

use regex::RegexBuilder;
use rmux_core::{GridRenderOptions, ScreenCaptureRange};

use super::RuntimeFormatContext;
use crate::pane_transcript::PaneTranscript;

const PANE_CONTENT_SEARCH_REGEX_SIZE_LIMIT: usize = 1_000_000;
const PANE_CONTENT_SEARCH_REGEX_DFA_SIZE_LIMIT: usize = 1_000_000;

impl RuntimeFormatContext<'_> {
    pub(super) fn pane_content_search(&self, options: &str, pattern: &str) -> Option<String> {
        if let Some(screen) = self
            .observe_pane_transcript(PaneTranscript::copy_mode_render_screen)
            .flatten()
        {
            let lines = screen.capture_transcript_lines_independent(
                ScreenCaptureRange::default(),
                GridRenderOptions::default(),
            );
            return Some(search_visible_lines(&lines, options, pattern).to_string());
        }
        let screen = self.observe_pane_transcript(|transcript| transcript.screen().clone())?;
        let lines = screen.capture_transcript_lines_independent(
            ScreenCaptureRange::default(),
            GridRenderOptions::default(),
        );
        Some(search_visible_lines(&lines, options, pattern).to_string())
    }
}

fn search_visible_lines(lines: &[Vec<u8>], options: &str, pattern: &str) -> usize {
    if pattern.is_empty() {
        return 0;
    }

    let ignore_case = options.contains('i');
    if options.contains('r') {
        let mut builder = RegexBuilder::new(pattern);
        builder.case_insensitive(ignore_case);
        builder.size_limit(PANE_CONTENT_SEARCH_REGEX_SIZE_LIMIT);
        builder.dfa_size_limit(PANE_CONTENT_SEARCH_REGEX_DFA_SIZE_LIMIT);
        let Ok(regex) = builder.build() else {
            return 0;
        };
        return search_lines(lines, |line| regex.is_match(line));
    }

    if ignore_case {
        let pattern = pattern.to_lowercase();
        return search_lines(lines, |line| line.to_lowercase().contains(&pattern));
    }

    search_lines(lines, |line| line.contains(pattern))
}

fn search_lines(lines: &[Vec<u8>], mut matches: impl FnMut(&str) -> bool) -> usize {
    lines
        .iter()
        .map(|line| String::from_utf8_lossy(line))
        .position(|line| matches(line.as_ref()))
        .map_or(0, |index| index + 1)
}
