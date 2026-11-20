use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clock_mode::ClockModeState;
use crate::copy_mode::CopyModeState;
use crate::pane_rename_queue::AcceptedWindowRename;
#[cfg(test)]
use rmux_core::style::Style;
#[cfg(test)]
use rmux_core::GridRenderOptions;
use rmux_core::{
    Screen, TerminalPaletteIndex, TerminalPassthrough, TerminalScreen, Utf8Config, WindowId,
};
use rmux_proto::TerminalSize;

mod palette_queries;

use palette_queries::PendingPaletteQueries;

mod dead_footer;

#[cfg(test)]
#[path = "pane_transcript/presentation_test_clock.rs"]
pub(crate) mod presentation_test_clock;

mod presentation;
#[cfg(test)]
use presentation::{visible_revision_reuse_map, PaneVisibleLineCapture};
pub(crate) use presentation::{FramePresentation, PresentationCapture, PresentationRead};

mod shared;
pub(crate) use shared::{
    with_unavailable_pane_answered, with_unavailable_pane_unanswered, PaneTranscriptReach,
    PaneTranscriptUnavailable, SharedPaneTranscript,
};

mod capture;
mod modes;
mod timers;

pub(crate) const PANE_INPUT_GROUND_TIMEOUT: Duration = Duration::from_secs(5);
// A child cannot hold pane publication forever after a malformed write or
// abrupt failure. This exceeds normal synchronized redraws (including the
// measured 250 ms fixture) while providing deterministic fail-open progress.
pub(crate) const PANE_SYNCHRONIZED_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Which mode a pane is in, without the name a format would print.
///
/// `copy-mode -q` addresses a pane and has to leave whatever is on it, so it
/// asks this rather than matching on a mode's printed name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneModeKind {
    Copy,
    Clock,
    ModeTree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneModeState {
    Copy(Box<CopyModeState>),
    Clock(ClockModeState),
    ModeTree {
        name: &'static str,
        previous: Option<Box<PaneModeState>>,
    },
}

impl PaneModeState {
    fn resize(&mut self, size: TerminalSize) {
        match self {
            Self::Copy(copy_mode) => copy_mode.resize(size),
            Self::ModeTree { previous, .. } => {
                if let Some(previous) = previous.as_mut() {
                    previous.resize(size);
                }
            }
            Self::Clock(_) => {}
        }
    }
}

pub(crate) struct PaneTranscript {
    terminal: TerminalScreen,
    pub(crate) control_colours: crate::control_colours::PaneControlColours,
    pub(crate) theme: crate::theme::PaneTheme,
    mode: Option<PaneModeState>,
    /// Whether this pane received input while a mode was up and nothing has
    /// left the last mode since. The source raises it on any non-empty input
    /// that arrives while a mode remains (`tmux-3.7b/input.c:1036-1038`) and
    /// clears it when the last mode is left (`window.c:1275-1277`).
    unseen_changes: bool,
    mode_revision: u64,
    output_sequence: u64,
    next_clock_generation: u64,
    ground_timer_started_at: Option<Instant>,
    ground_timer_token: u64,
    synchronized_output_started_at: Option<Instant>,
    synchronized_output_timer_token: u64,
    synchronized_output_timer_job_claimed: Arc<AtomicBool>,
    pending_palette_queries: PendingPaletteQueries,
    /// The window this pane belonged to when its policy was last resolved.
    /// Every accepted `ESC k` request is stamped with it at acceptance.
    rename_owner: Option<WindowId>,
    process_exited: bool,
    foreground_group: Option<u32>,
    foreground_transition_pending: bool,
    #[cfg(test)]
    utf8_config: Utf8Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneGroundTimer {
    pub(crate) deadline: Instant,
    token: u64,
}

/// A uniquely owned, fail-open wake for one synchronized-output interval.
///
/// Timer ownership is the pending-job invariant: once this value leaves a
/// transcript, exactly one append result, scheduler error, or worker job owns
/// its claim. Dropping it returns the claim, so a discarded hand-off cannot
/// leave the pane permanently unable to arm another wake.
#[derive(Debug)]
pub(crate) struct PaneSynchronizedOutputTimer {
    pub(crate) deadline: Instant,
    token: u64,
    job_ownership: PaneSynchronizedOutputTimerJobOwnership,
}

/// The sole owner of a synchronized-output timer claim.
///
/// This guard deliberately is neither `Clone` nor `Copy`. It moves with the
/// timer through each hand-off: append result, scheduler send, worker queue,
/// and stale-job replacement. Any discarded result, failed send, queue
/// shutdown, or unwinding panic drops the guard and releases the claim.
#[derive(Debug)]
struct PaneSynchronizedOutputTimerJobOwnership {
    claimed: Arc<AtomicBool>,
}

impl Drop for PaneSynchronizedOutputTimerJobOwnership {
    fn drop(&mut self) {
        self.claimed.store(false, Ordering::Release);
    }
}

#[derive(Debug, Default)]
pub(crate) struct PaneAppendResult {
    pub(crate) bell_count: u64,
    /// Window-rename requests this batch's output accepted, in emission order,
    /// each stamped with the window that owned the pane at that moment. They
    /// are complete requests, not a difference of stored name: an empty or
    /// repeated name renames the window just as a new one does.
    pub(crate) window_rename_requests: Vec<AcceptedWindowRename>,
    pub(crate) title_changed: bool,
    pub(crate) title_change: Option<(String, String)>,
    /// The pane reported a different OSC 7 working directory. It reaches the
    /// outer terminal only through a client re-render, exactly like the title
    /// (issue #182).
    pub(crate) path_changed: bool,
    pub(crate) passthroughs: Vec<TerminalPassthrough>,
    pub(crate) dropped_passthrough_count: u64,
    pub(crate) replies: Vec<u8>,
    pub(crate) ground_timer: Option<PaneGroundTimer>,
    pub(crate) synchronized_output_timer: Option<PaneSynchronizedOutputTimer>,
    pub(crate) alternate_mode_changed: bool,
    pub(crate) recovery_rebase_required: bool,
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
    pub(crate) fn keyboard_flags(&self) -> u8 {
        self.terminal.screen().keyboard_flags()
    }

    pub(crate) fn new(limit: usize, size: TerminalSize) -> Self {
        let mut terminal = TerminalScreen::new(size, limit);
        terminal.enable_theme_events();
        Self {
            terminal,
            control_colours: crate::control_colours::PaneControlColours::default(),
            theme: crate::theme::PaneTheme::default(),
            mode: None,
            // A new transcript has seen everything it holds.
            unseen_changes: false,
            mode_revision: 0,
            output_sequence: 0,
            next_clock_generation: 1,
            ground_timer_started_at: None,
            ground_timer_token: 0,
            synchronized_output_started_at: None,
            synchronized_output_timer_token: 0,
            synchronized_output_timer_job_claimed: Arc::new(AtomicBool::new(false)),
            pending_palette_queries: PendingPaletteQueries::default(),
            rename_owner: None,
            process_exited: false,
            foreground_group: None,
            foreground_transition_pending: false,
            #[cfg(test)]
            utf8_config: Utf8Config::default(),
        }
    }

    pub(crate) fn shared_for_pane(
        pane_id: rmux_core::PaneId,
        limit: usize,
        size: TerminalSize,
    ) -> SharedPaneTranscript {
        SharedPaneTranscript::new(pane_id, Self::new(limit, size))
    }

    /// Unit fixtures that never route a request to the pane use `%0`.
    #[cfg(test)]
    pub(crate) fn shared(limit: usize, size: TerminalSize) -> SharedPaneTranscript {
        Self::shared_for_pane(rmux_core::PaneId::new(0), limit, size)
    }

    pub(crate) fn apply_control_colour(
        &mut self,
        report: crate::control_colours::ColourReport,
        clients: &crate::control_colours::ControlColourClients,
    ) {
        self.control_colours.apply(report, clients);
    }

    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.terminal.screen_mut().set_history_limit(limit);
    }

    pub(crate) fn set_cursor_default_style(&mut self, style: u32) -> bool {
        self.terminal.screen_mut().set_cursor_default_style(style)
    }

    pub(crate) fn append_bytes(&mut self, bytes: &[u8]) -> u64 {
        self.append_bytes_with_effects(bytes).bell_count
    }

    #[cfg(test)]
    pub(crate) fn append_bytes_and_take_replies(&mut self, bytes: &[u8]) -> PaneAppendResult {
        self.append_bytes_with_effects(bytes)
    }

    pub(crate) fn append_bytes_with_effects(&mut self, bytes: &[u8]) -> PaneAppendResult {
        let now = Instant::now();
        let ground_timer_expired = self.expire_ground_timer_if_due(now);
        let synchronized_output_expired = self.expire_synchronized_output_if_due(now);
        if !bytes.is_empty() {
            self.output_sequence = self.output_sequence.saturating_add(1);
        }
        let title_before = self.terminal.screen().title().to_owned();
        let path_before = self.terminal.screen().path().to_owned();
        let alternate_before = self.terminal.screen().is_alternate();
        let query_colours = self.control_colours.query_colours();
        self.terminal.feed_with_query_colours(bytes, query_colours);
        let synchronized_output_started = self.terminal.take_synchronized_output_started();
        let recovery_rebase_required = ground_timer_expired
            || synchronized_output_expired
            || self.terminal.take_recovery_rebase_required();
        let title_after = self.terminal.screen().title().to_owned();
        let title_changed = title_after != title_before;
        let path_changed = self.terminal.screen().path() != path_before;
        // Exit can be observed before the last buffered PTY bytes are drained.
        // Preserve done/error results but never resurrect an active dead process.
        if self.process_exited || self.foreground_transition_pending {
            self.terminal.screen_mut().expire_program_status();
        }
        let passthroughs = self.terminal.take_terminal_passthrough();
        self.pending_palette_queries.register(&passthroughs, now);
        let dropped_passthrough_count = self.terminal.take_terminal_passthrough_dropped_count();
        let replies = self.terminal.take_replies();
        let replies =
            self.theme
                .apply(replies, self.terminal.take_theme_events(), query_colours[1]);
        let ground_timer = self.refresh_ground_timer(now);
        let synchronized_output_timer =
            self.refresh_synchronized_output_timer(now, synchronized_output_started);
        PaneAppendResult {
            bell_count: self.terminal.screen_mut().take_bell_count(),
            window_rename_requests: self.accepted_window_renames(),
            title_changed,
            title_change: title_changed.then_some((title_before, title_after)),
            path_changed,
            passthroughs,
            dropped_passthrough_count,
            replies,
            ground_timer,
            synchronized_output_timer,
            alternate_mode_changed: self.terminal.screen().is_alternate() != alternate_before,
            recovery_rebase_required,
        }
    }

    /// Consumes one correlated OSC 4 response for a recently emitted query.
    /// Unsolicited, expired, and duplicate terminal responses are rejected so
    /// attach input cannot become an arbitrary control-sequence injection path.
    pub(crate) fn consume_palette_query_response(&mut self, index: TerminalPaletteIndex) -> bool {
        self.pending_palette_queries.consume(index, Instant::now())
    }

    /// A query emitted within tmux's request window and not yet answered.
    pub(crate) fn has_active_palette_query(&self) -> bool {
        self.pending_palette_queries
            .has_active_query(Instant::now())
    }

    #[cfg(test)]
    fn has_active_palette_query_at(&self, now: Instant) -> bool {
        self.pending_palette_queries.has_active_query(now)
    }

    #[cfg(test)]
    fn consume_palette_query_response_at(
        &mut self,
        index: TerminalPaletteIndex,
        now: Instant,
    ) -> bool {
        self.pending_palette_queries.consume(index, now)
    }

    pub(crate) fn reset_terminal_state(&mut self) {
        self.output_sequence = self.output_sequence.saturating_add(1);
        self.terminal.feed(b"\x1bc");
        let _ = self.terminal.take_terminal_passthrough();
        let _ = self.terminal.take_terminal_passthrough_dropped_count();
        let _ = self.terminal.take_replies();
        let _ = self.theme.apply(
            Vec::new(),
            self.terminal.take_theme_events(),
            self.control_colours.query_colours()[1],
        );
        self.ground_timer_started_at = None;
        self.ground_timer_token = self.ground_timer_token.saturating_add(1);
        self.clear_synchronized_output_timer();
        self.pending_palette_queries.clear();
        // This point leaves the mode without going through the revision, so it
        // clears what leaving the last mode would have cleared.
        self.mode = None;
        self.unseen_changes = false;
    }

    pub(crate) fn program_status(&self) -> &rmux_core::ProgramStatus {
        self.terminal.screen().program_status()
    }

    /// Foreground process-group changes invalidate active reports even for
    /// shells without OSC 133. This is a lifecycle heuristic, not writer identity.
    pub(crate) fn observe_foreground_group(&mut self, group: Option<u32>) {
        let Some(group) = group.filter(|group| *group > 0) else {
            return;
        };
        if self.foreground_group.is_some_and(|old| old != group) {
            // Bytes already queued (or read but not yet parsed) may still belong
            // to the old group. Fail closed until that queue AND any fragmented
            // sequence have drained; parsing them must not resurrect active state.
            // Even an entire first report may still be in the reader's buffer.
            // Absence of parsed records/fragments does not prove a clean drain.
            self.foreground_transition_pending = true;
            self.terminal.screen_mut().expire_program_status();
        }
        self.foreground_group = Some(group);
    }

    pub(crate) fn program_status_transition_pending(&self) -> bool {
        self.foreground_transition_pending
    }

    pub(crate) fn finish_program_status_drain(&mut self) {
        if self.foreground_transition_pending && self.terminal.pending_bytes().is_empty() {
            self.foreground_transition_pending = false;
        }
    }

    pub(crate) fn expire_program_status(&mut self) {
        self.process_exited = true;
        self.terminal.screen_mut().expire_program_status();
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

    /// Sets the pane's `allow-rename` policy together with the window that
    /// owns it right now.
    ///
    /// The two travel together on purpose. A request is stamped with the owner
    /// the pane had when its output was parsed, so a transfer that happens
    /// afterwards cannot make an old request rename the destination window;
    /// letting the policy and the owner be set from different places is how
    /// they would drift apart.
    pub(crate) fn set_rename_policy(&mut self, enabled: bool, owner: Option<WindowId>) {
        self.terminal.set_allow_rename(enabled);
        self.rename_owner = owner;
    }

    #[cfg(test)]
    pub(crate) fn rename_owner(&self) -> Option<WindowId> {
        self.rename_owner
    }

    /// Drains the accepted requests and stamps each with the pane's owner.
    ///
    /// Without a known owner a request cannot be attributed to a window at
    /// all, so it is dropped here rather than applied to whatever window the
    /// pane is found in later; the count is reported instead of being lost
    /// silently. Every path that turns `allow-rename` on sets the owner in the
    /// same call, so this is a wiring fault, not a reachable policy.
    fn accepted_window_renames(&mut self) -> Vec<AcceptedWindowRename> {
        let accepted = self.terminal.take_window_rename_requests();
        if accepted.is_empty() {
            return Vec::new();
        }
        let Some(owner) = self.rename_owner else {
            tracing::warn!(
                dropped = accepted.len(),
                "pane accepted window-rename requests with no known owning window"
            );
            return Vec::new();
        };
        accepted
            .into_iter()
            .map(|name| AcceptedWindowRename { owner, name })
            .collect()
    }

    pub(crate) fn set_scroll_on_clear(&mut self, enabled: bool) {
        self.terminal.set_scroll_on_clear(enabled);
    }

    pub(crate) fn set_extended_key_policy(&mut self, value: Option<&str>) {
        self.terminal
            .screen_mut()
            .set_extended_key_policy(rmux_core::input::ExtendedKeyPolicy::from_option(value));
    }

    pub(crate) fn set_input_buffer_limit(&mut self, limit: usize) {
        self.terminal.set_input_buffer_limit(limit);
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
        if let Some(mode) = self.mode.as_mut() {
            mode.resize(size);
        }
    }

    pub(crate) fn clone_screen(&self) -> Screen {
        self.terminal.screen().clone()
    }

    pub(crate) fn clone_presentation_screen(&self) -> Screen {
        self.presentation_screen().clone()
    }

    /// Marks this pane's screen as the screen of a pane with no process.
    ///
    /// `new-pane -I` creates such a pane and then writes a finished payload
    /// into it. The state belongs to the screen, not to the payload, so the
    /// bytes themselves are never rewritten on their way in.
    pub(crate) fn mark_as_empty_pane_screen(&mut self) {
        self.terminal.screen_mut().enter_empty_pane_state();
    }

    pub(crate) fn hide_cursor(&mut self) -> bool {
        self.terminal.screen_mut().hide_cursor()
    }

    pub(crate) fn screen(&self) -> &Screen {
        self.terminal.screen()
    }

    pub(crate) fn presentation_history_size(&self) -> usize {
        self.presentation_screen().history_size()
    }

    pub(crate) fn presentation_is_alternate(&self) -> bool {
        self.presentation_screen().is_alternate()
    }

    /// Appends bytes that came from this pane's own terminal.
    ///
    /// This is the only entry that marks them unseen, and it marks them exactly
    /// as the source does: any non-empty input, visible or not, while a mode
    /// remains. Everything this server writes into a transcript itself - a dead
    /// pane's status, a diagnostic - goes through the common append and marks
    /// nothing.
    pub(crate) fn append_pty_bytes_with_effects(&mut self, bytes: &[u8]) -> PaneAppendResult {
        if !bytes.is_empty() && self.mode.is_some() {
            self.unseen_changes = true;
        }
        self.append_bytes_with_effects(bytes)
    }

    pub(crate) fn mode(&self) -> u32 {
        self.terminal.screen().mode()
    }

    pub(crate) fn is_alternate(&self) -> bool {
        self.terminal.screen().is_alternate()
    }

    pub(crate) fn title(&self) -> &str {
        self.terminal.screen().title()
    }

    pub(crate) fn set_title(&mut self, title: impl Into<String>) -> Option<(String, String)> {
        let old_title = self.terminal.screen().title().to_owned();
        let new_title = title.into();
        self.terminal.screen_mut().set_title(new_title.clone());
        (old_title != new_title).then_some((old_title, new_title))
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
        self.clear_synchronized_output_timer();
        self.mode = None;
    }

    #[cfg(test)]
    pub(crate) fn utf8_config(&self) -> &Utf8Config {
        &self.utf8_config
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn capture_main_visible_line_changes(
        &self,
        rows: usize,
        options: GridRenderOptions,
        cols: u16,
        previous_revisions: Option<&[u64]>,
        default_style: Option<&Style>,
    ) -> Vec<PaneVisibleLineCapture> {
        self.read_presentation().capture_projected_line_changes(
            0,
            rows,
            options,
            cols,
            previous_revisions,
            default_style,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{visible_revision_reuse_map, PaneTranscript};
    use rmux_core::{GridRenderOptions, ScreenCaptureRange, TerminalPaletteIndex, TerminalScreen};
    use rmux_proto::TerminalSize;
    use std::time::{Duration, Instant};

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
    fn dead_pane_footer_joins_the_row_above_like_tmux() {
        let mut transcript = transcript(20, 3, 10);
        transcript.append_bytes(b"\x1b[1;2H\x1b7\x1b[1;2rerror");
        let saved_cursor = transcript.saved_cursor_state();
        transcript.append_dead_pane_footer(b"Pane is dead");
        assert_eq!(transcript.saved_cursor_state(), saved_cursor);
        let joined = GridRenderOptions {
            join_wrapped: true,
            ..GridRenderOptions::default()
        };

        assert_eq!(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
            b"\n\nPane is dead\n"
        );
        assert_eq!(
            transcript.capture_main(ScreenCaptureRange::default(), joined),
            b"\nPane is dead\n"
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
    fn append_reports_rep_that_crosses_an_output_boundary() {
        let mut transcript = transcript(8, 2, 10);
        assert!(
            !transcript
                .append_bytes_with_effects("界".as_bytes())
                .recovery_rebase_required
        );

        let result = transcript.append_bytes_with_effects(b"\x1b[2b");

        assert!(result.recovery_rebase_required);
        assert_eq!(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
            "界界界\n\n".as_bytes()
        );
    }

    #[test]
    fn append_keeps_same_boundary_rep_raw_replayable() {
        let mut transcript = transcript(8, 2, 10);

        let result = transcript.append_bytes_with_effects(b"X\x1b[2b");

        assert!(!result.recovery_rebase_required);
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
    fn synchronized_output_reentry_rearms_timeout_at_the_exact_parser_transition() {
        let mut transcript = transcript(48, 4, 100);
        transcript.append_bytes(b"\x1b[2J\x1b[HSTABLE_BEFORE_SYNC");
        let mut first_timer = transcript
            .append_bytes_with_effects(b"\x1b[?2026h\x1b[2J\x1b[HFIRST_PARTIAL")
            .synchronized_output_timer
            .expect("first mode admission arms a timer");
        let first_timer_token = first_timer.token;

        let second_timer = transcript
            .append_bytes_with_effects(
                b"\x1b[2J\x1b[HFIRST_FINAL\x1b[?2026l\x1b[?2026h\x1b[2J\x1b[HSECOND_PARTIAL",
            )
            .synchronized_output_timer;
        assert!(
            second_timer.is_none(),
            "one owned timer job must cover rapid synchronized-output reentry"
        );

        let presented = transcript
            .presentation_screen()
            .capture_transcript(ScreenCaptureRange::default(), GridRenderOptions::default());
        assert!(presented
            .windows(b"FIRST_FINAL".len())
            .any(|window| window == b"FIRST_FINAL"));
        assert!(!presented
            .windows(b"SECOND_PARTIAL".len())
            .any(|window| window == b"SECOND_PARTIAL"));

        first_timer.deadline = Instant::now() - Duration::from_millis(1);
        let (expired, replacement) = transcript.handle_synchronized_output_timer(first_timer);
        assert!(
            !expired,
            "the first interval must not release its successor"
        );
        let replacement = replacement.expect("the stale job rearms the current interval");
        assert_ne!(first_timer_token, replacement.token);
    }

    #[test]
    fn split_window_stdin_timer_discard_does_not_latch_later_synchronized_admission() {
        let mut transcript = transcript(48, 4, 100);
        {
            let discarded_timer = transcript
                .append_bytes_with_effects(b"\x1b[?2026h\x1b[2J\x1b[HSPLIT_WINDOW_STDIN_PARTIAL")
                .synchronized_output_timer
                .expect("split-window stdin admission produces a timer job");
            assert!(discarded_timer.deadline > Instant::now());
        }

        let rearmed_timer = transcript
            .append_bytes_with_effects(b"\x1b[?2026l\x1b[?2026h\x1b[2J\x1b[HLATER_CHILD_PARTIAL")
            .synchronized_output_timer;
        assert!(
            rearmed_timer.is_some(),
            "discarding the split-window timer must not poison a later admission"
        );
    }

    #[test]
    fn append_bytes_recovers_after_unterminated_non_osc_dcs_ground_timer() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1bPtmux;not-osc\x07");
        let timer = result
            .ground_timer
            .expect("unterminated DCS should arm the parser ground timer");

        transcript.append_bytes(b"STILL-HIDDEN");
        let hidden_capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("capture is utf8");
        assert!(
            !hidden_capture.contains("STILL-HIDDEN"),
            "bytes before the DCS timeout should remain swallowed by the unterminated string"
        );

        assert!(
            transcript.force_ground_timer_expired_for_test(timer),
            "matching ground timer should expire the parser"
        );
        transcript.append_bytes(b"AFTER");

        let capture = String::from_utf8(
            transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .expect("capture is utf8");
        assert!(capture.contains("AFTER"));
        assert!(!capture.contains("not-osc"));
    }

    #[test]
    fn append_bytes_arms_single_ground_timer_while_parser_remains_blocked() {
        let mut transcript = transcript(40, 4, 10);
        let timer = transcript
            .append_bytes_with_effects(b"\x1bPtmux;not-osc\x07")
            .ground_timer
            .expect("unterminated DCS should arm the parser ground timer");

        assert!(
            transcript
                .append_bytes_with_effects(b"still blocked")
                .ground_timer
                .is_none(),
            "the same blocked parser state must not arm another timer on each read"
        );

        assert!(transcript.force_ground_timer_expired_for_test(timer));
        let next_timer = transcript
            .append_bytes_with_effects(b"\x1bPtmux;not-osc-again\x07")
            .ground_timer;
        assert!(
            next_timer.is_some(),
            "a new blocked sequence should arm a fresh timer after recovery"
        );
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
    fn osc4_queries_register_only_bounded_correlated_responses() {
        let mut transcript = transcript(40, 4, 10);
        let index = TerminalPaletteIndex::from(0);

        let first = transcript.append_bytes_with_effects(b"\x1b]4;0;?\x07");
        assert_eq!(first.passthroughs.len(), 1);
        assert_eq!(first.passthroughs[0].render_sequence(), b"\x1b]4;0;?\x1b\\");
        assert!(transcript.consume_palette_query_response(index));
        assert!(
            !transcript.consume_palette_query_response(index),
            "an unsolicited duplicate response must be rejected"
        );

        transcript.append_bytes_with_effects(b"\x1b]4;0;?;0;?\x1b\\");
        assert!(transcript.consume_palette_query_response(index));
        assert!(transcript.consume_palette_query_response(index));
        assert!(!transcript.consume_palette_query_response(index));

        for _ in 0..20 {
            transcript.append_bytes_with_effects(b"\x1b]4;0;?\x07");
        }
        for _ in 0..8 {
            assert!(transcript.consume_palette_query_response(index));
        }
        assert!(
            !transcript.consume_palette_query_response(index),
            "repeated unanswered queries stay capped"
        );
    }

    #[test]
    fn osc4_query_response_correlation_expires_and_reset_clears_it() {
        let mut transcript = transcript(40, 4, 10);
        let index = TerminalPaletteIndex::from(7);

        transcript.append_bytes_with_effects(b"\x1b]4;7;?\x1b\\");
        assert!(!transcript
            .consume_palette_query_response_at(index, Instant::now() + Duration::from_secs(9),));

        transcript.append_bytes_with_effects(b"\x1b]4;7;?\x07");
        transcript.reset_terminal_state();
        assert!(!transcript.consume_palette_query_response(index));
    }

    #[test]
    fn osc4_query_is_active_only_until_answered_or_past_the_tmux_window() {
        let mut transcript = transcript(40, 4, 10);
        let index = TerminalPaletteIndex::from(1);
        assert!(!transcript.has_active_palette_query());

        transcript.append_bytes_with_effects(b"\x1b]4;1;?\x07");
        let asked = Instant::now();
        assert!(transcript.has_active_palette_query());
        assert!(
            !transcript.has_active_palette_query_at(asked + Duration::from_millis(600)),
            "tmux 3.7b drops a request after 500 ms; it no longer slows partial keys"
        );
        assert!(transcript.consume_palette_query_response(index));
        assert!(
            !transcript.has_active_palette_query(),
            "an answered query no longer slows partial keys"
        );

        transcript.append_bytes_with_effects(b"\x1b]4;1;?\x07");
        transcript.reset_terminal_state();
        assert!(!transcript.has_active_palette_query());
    }

    #[test]
    fn osc4_invalid_indices_and_sets_never_open_a_response_slot() {
        let mut transcript = transcript(40, 4, 10);

        let result =
            transcript.append_bytes_with_effects(b"\x1b]4;256;?;0;rgb:0000/0000/0000;bad;?\x07");
        assert!(result.passthroughs.is_empty());
        assert!(!transcript.consume_palette_query_response(TerminalPaletteIndex::from(0)));
        assert!(!transcript.consume_palette_query_response(TerminalPaletteIndex::from(255)));
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
    fn append_bytes_reports_rmux_xtversion_identity() {
        let mut transcript = transcript(40, 4, 10);
        let result = transcript.append_bytes_with_effects(b"\x1b[>q");
        let version = env!("CARGO_PKG_VERSION");

        assert_eq!(
            result.replies,
            format!("\x1bP>|rmux {version}\x1b\\").into_bytes()
        );
        assert!(transcript.append_bytes_with_effects(b"").replies.is_empty());
    }

    #[test]
    fn append_bytes_negotiates_kitty_without_outer_mode_bits() {
        let mut transcript = transcript(40, 4, 10);
        let initial_mode = transcript.mode();
        let result = transcript.append_bytes_with_effects(b"\x1b[=8u\x1b[>1u\x1b[?u\x1b[<u\x1b[?u");
        assert_eq!(result.replies, b"\x1b[?1u\x1b[?8u");
        assert_eq!(transcript.keyboard_flags(), 8);
        assert_eq!(transcript.mode(), initial_mode);
        assert!(result.passthroughs.is_empty());
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
        let entered = transcript.append_bytes_with_effects(b"\x1b[?1049h");
        let unchanged = transcript.append_bytes_with_effects(b"alt\n");

        let capture = String::from_utf8(
            transcript
                .capture_saved(ScreenCaptureRange::default(), GridRenderOptions::default())
                .expect("alternate capture exists"),
        )
        .expect("utf8");
        assert!(entered.alternate_mode_changed);
        assert!(!unchanged.alternate_mode_changed);
        assert!(capture.contains("main"));
        assert!(!capture.contains("alt"));

        let exited = transcript.append_bytes_with_effects(b"\x1b[?1049l");
        assert!(exited.alternate_mode_changed);
        assert!(!transcript.is_alternate());
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
        assert_eq!(
            result.title_change,
            Some(("".to_owned(), "alpha".to_owned()))
        );

        let result = transcript.append_bytes_and_take_replies(b"\x1b]2;alpha\x07");
        assert!(!result.title_changed);
        assert_eq!(result.title_change, None);

        let result = transcript.append_bytes_and_take_replies(b"\x1b]2;beta\x07");
        assert!(result.title_changed);
        assert_eq!(
            result.title_change,
            Some(("alpha".to_owned(), "beta".to_owned()))
        );
    }
}

#[cfg(test)]
#[path = "pane_transcript/theme_tests.rs"]
mod theme_tests;

#[cfg(test)]
#[path = "pane_transcript/dcs_passthrough_tests.rs"]
mod dcs_passthrough_tests;
