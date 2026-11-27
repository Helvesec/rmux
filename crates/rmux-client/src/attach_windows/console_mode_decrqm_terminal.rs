//! Test-only outer terminal for a child hosted in a real ConPTY.
//!
//! A passthrough ConPTY forwards the child's private-mode changes and its
//! `CSI ? Pd $ p` (DECRQM) queries to the hosting terminal instead of
//! answering them itself. The test parent is that hosting terminal, so it must
//! track the reporting modes the child sets and reply with DECRPM on the
//! ConPTY input, exactly as Windows Terminal would.
//!
//! That terminal state is not the child's: conhost keeps 1004 and 9001 set on
//! its hosting terminal for the input it reads from it, and follows every
//! forwarded `CSI ? Pd l` with `CSI ? Pd h` (Windows 11, C1836 at 8849bbd6d). Under
//! passthrough, DECRQM therefore cannot observe the child's own reset.

/// Private modes whose reporting state the VT restoration acceptance asserts:
/// focus reporting and win32-input-mode.
const TRACKED_MODES: [u16; 2] = [1004, 9001];
/// Longer CSI bodies are never DECSET/DECRST/DECRQM for a tracked mode.
const MAX_CSI_BODY: usize = 64;

#[derive(Default)]
enum ParserState {
    #[default]
    Ground,
    Escape,
    Csi(Vec<u8>),
}

/// Whose state the DECRPM for 1004 and 9001 reports, classified from the
/// answer to an explicit reset that the test writes before any product code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReportingOracle {
    /// The console answers from the child's own requests (no passthrough).
    ChildRequests,
    /// A passthrough ConPTY forwarded the query to its hosting terminal, where
    /// conhost keeps both modes set whatever the child requests.
    HostPinnedSet,
}

impl ReportingOracle {
    /// Classifies the `(win32, focus)` answer to the calibration reset.
    pub(super) fn after_explicit_reset(reporting: (bool, bool)) -> Option<Self> {
        match reporting {
            (false, false) => Some(Self::ChildRequests),
            (true, true) => Some(Self::HostPinnedSet),
            _ => None,
        }
    }

    /// The `(win32, focus)` answer expected while VT input is `vt_input`.
    pub(super) fn expected(self, vt_input: bool) -> (bool, bool) {
        match self {
            Self::ChildRequests => (vt_input, vt_input),
            Self::HostPinnedSet => (true, true),
        }
    }
}

#[derive(Default)]
pub(super) struct ReportingModeTerminal {
    enabled: [bool; TRACKED_MODES.len()],
    state: ParserState,
    answered: usize,
}

impl ReportingModeTerminal {
    /// Consumes child output, which may split sequences at any byte, and
    /// returns the DECRPM replies to write to the ConPTY input, in order.
    pub(super) fn feed(&mut self, output: &[u8]) -> Vec<u8> {
        let mut replies = Vec::new();
        for &byte in output {
            self.state = match (std::mem::take(&mut self.state), byte) {
                (_, 0x1b) => ParserState::Escape,
                (ParserState::Escape, b'[') => ParserState::Csi(Vec::new()),
                (ParserState::Csi(body), 0x40..=0x7e) => {
                    self.dispatch(&body, byte, &mut replies);
                    ParserState::Ground
                }
                (ParserState::Csi(mut body), 0x20..=0x3f) if body.len() < MAX_CSI_BODY => {
                    body.push(byte);
                    ParserState::Csi(body)
                }
                // C0 controls execute inside a CSI without ending it.
                (ParserState::Csi(body), 0x00..=0x1f) => ParserState::Csi(body),
                _ => ParserState::Ground,
            };
        }
        replies
    }

    /// DECRQM queries answered so far: zero when the console answers itself.
    pub(super) fn answered(&self) -> usize {
        self.answered
    }

    fn dispatch(&mut self, body: &[u8], final_byte: u8, replies: &mut Vec<u8>) {
        let Some(body) = body.strip_prefix(b"?") else {
            return;
        };
        match final_byte {
            b'h' | b'l' => {
                for mode in body.split(|byte| *byte == b';').filter_map(parse_mode) {
                    if let Some(slot) = tracked_slot(mode) {
                        self.enabled[slot] = final_byte == b'h';
                    }
                }
            }
            b'p' => {
                let Some(mode) = body.strip_suffix(b"$").and_then(parse_mode) else {
                    return;
                };
                // DECRPM: 1 = set, 2 = reset, 0 = not recognized.
                let value = match tracked_slot(mode) {
                    Some(slot) if self.enabled[slot] => 1,
                    Some(_) => 2,
                    None => 0,
                };
                replies.extend_from_slice(format!("\x1b[?{mode};{value}$y").as_bytes());
                self.answered += 1;
            }
            _ => {}
        }
    }
}

fn tracked_slot(mode: u16) -> Option<usize> {
    TRACKED_MODES.iter().position(|tracked| *tracked == mode)
}

fn parse_mode(digits: &[u8]) -> Option<u16> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{ReportingModeTerminal, ReportingOracle};

    fn replies(terminal: &mut ReportingModeTerminal, output: &str) -> String {
        String::from_utf8(terminal.feed(output.as_bytes())).expect("ASCII replies")
    }

    #[test]
    fn tracked_modes_start_reset_and_unknown_modes_are_not_recognized() {
        let mut terminal = ReportingModeTerminal::default();
        assert_eq!(
            replies(&mut terminal, "\x1b[?9001$p\x1b[?1004$p\x1b[?2004$p"),
            "\x1b[?9001;2$y\x1b[?1004;2$y\x1b[?2004;0$y"
        );
    }

    #[test]
    fn set_and_reset_follow_the_child_including_combined_parameters() {
        let mut terminal = ReportingModeTerminal::default();
        assert_eq!(replies(&mut terminal, "\x1b[?1004;9001h"), "");
        assert_eq!(
            replies(&mut terminal, "\x1b[?9001$p\x1b[?1004$p"),
            "\x1b[?9001;1$y\x1b[?1004;1$y"
        );
        assert_eq!(replies(&mut terminal, "\x1b[?9001l"), "");
        assert_eq!(
            replies(&mut terminal, "\x1b[?9001$p\x1b[?1004$p"),
            "\x1b[?9001;2$y\x1b[?1004;1$y"
        );
    }

    #[test]
    fn a_query_split_at_every_byte_is_answered_once() {
        let mut terminal = ReportingModeTerminal::default();
        let mut collected = Vec::new();
        for byte in b"\x1b[?9001h\x1b[?9001$p" {
            collected.extend(terminal.feed(&[*byte]));
        }
        assert_eq!(collected, b"\x1b[?9001;1$y");
    }

    #[test]
    fn ansi_modes_and_malformed_queries_are_ignored() {
        let mut terminal = ReportingModeTerminal::default();
        assert_eq!(
            replies(
                &mut terminal,
                "\x1b[1004h\x1b[9001$p\x1b[?9001;1004$p\x1b[?$p"
            ),
            ""
        );
        assert_eq!(replies(&mut terminal, "\x1b[?1004$p"), "\x1b[?1004;2$y");
    }

    #[test]
    fn windows11_native_conpty_stream_receives_the_replies_it_waited_for() {
        // Output captured on Windows 11 (inbox passthrough ConPTY) by
        // C1835 at d9b519fa0: conhost's own startup requests, then the child's
        // calibration enable and its DECRQM, which no one answered.
        let mut terminal = ReportingModeTerminal::default();
        let native = "\x1b[1t\x1b[?1004h\x1b[?9001h\r\nrunning 1 test\r\ntest attach::windows::\
            console_mode_ownership_tests::vt_raw_terminal_restores_both_console_modes ... \
            \x1b[?1004h\x1b[?9001h\x1b[?9001$p\x1b[?1004$p";
        assert_eq!(
            replies(&mut terminal, native),
            "\x1b[?9001;1$y\x1b[?1004;1$y"
        );
    }

    #[test]
    fn windows11_passthrough_repins_the_reset_and_classifies_host_pinned() {
        // C1836 at 8849bbd6d (Windows 11, passthrough): the calibration reset as
        // forwarded, each child `l` followed by conhost's own `h`.
        let mut terminal = ReportingModeTerminal::default();
        let enable = "\x1b[?1004h\x1b[?9001h\x1b[?9001$p\x1b[?1004$p";
        assert_eq!(
            replies(&mut terminal, enable),
            "\x1b[?9001;1$y\x1b[?1004;1$y"
        );
        let reset = "\x1b[?9001l\x1b[?9001h\x1b[?1004l\x1b[?1004h\x1b[?9001$p\x1b[?1004$p";
        assert_eq!(
            replies(&mut terminal, reset),
            "\x1b[?9001;1$y\x1b[?1004;1$y"
        );
        assert_eq!(terminal.answered(), 4);
        let oracle = ReportingOracle::after_explicit_reset((true, true));
        assert_eq!(oracle, Some(ReportingOracle::HostPinnedSet));
        assert_eq!(ReportingOracle::HostPinnedSet.expected(false), (true, true));
    }

    #[test]
    fn a_console_that_answers_follows_vt_input_and_partial_resets_are_rejected() {
        let oracle = ReportingOracle::after_explicit_reset((false, false));
        assert_eq!(oracle, Some(ReportingOracle::ChildRequests));
        assert_eq!(ReportingOracle::ChildRequests.expected(true), (true, true));
        assert_eq!(
            ReportingOracle::ChildRequests.expected(false),
            (false, false)
        );
        assert_eq!(ReportingOracle::after_explicit_reset((true, false)), None);
        assert_eq!(ReportingOracle::after_explicit_reset((false, true)), None);
    }
}
