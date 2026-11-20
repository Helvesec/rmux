#[cfg(unix)]
use std::io::{self, Read, Write};

use super::ControlModeStatus;

const EXIT_RECORD_PREFIX: &[u8] = b"%exit";

/// tmux 3.7b's client-side record when the server disappears without closing
/// the control session (`client_exit_message` for `CLIENT_EXIT_LOST_SERVER`).
pub(super) const LOST_SERVER_EXIT_RECORD: &[u8] = b"%exit server exited unexpectedly\n";

/// How the server side of a control-mode stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlOutputEnd {
    /// The transport closed right after a complete `%exit` record.
    ExitDelivered(ControlModeStatus),
    /// The transport closed without a terminal `%exit` record; the client has
    /// already written [`LOST_SERVER_EXIT_RECORD`] to its own output.
    ServerLost,
}

#[derive(Debug, Default)]
pub(super) struct ControlOutputProgress {
    line: ControlLine,
    terminal_exit_delivered: bool,
    commands: CommandOutcomes,
}

#[derive(Debug)]
enum ControlLine {
    Prefix(usize),
    ExitReason,
    Other,
}

impl Default for ControlLine {
    fn default() -> Self {
        Self::Prefix(0)
    }
}

impl ControlOutputProgress {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.observe_byte(*byte);
            self.commands.observe_byte(*byte);
        }
    }

    /// The end the client reports once the transport has closed: the
    /// `%exit`-terminated session's status, or the lost server.
    pub(super) fn closed_end(&self) -> ControlOutputEnd {
        if self.terminal_exit_delivered {
            ControlOutputEnd::ExitDelivered(self.commands.status())
        } else {
            ControlOutputEnd::ServerLost
        }
    }

    /// Bytes the client writes once the transport has closed, so every control
    /// session ends with exactly one complete `%exit` record on its own line.
    pub(super) fn lost_server_suffix(&self) -> Option<Vec<u8>> {
        if self.terminal_exit_delivered {
            return None;
        }
        let mut suffix = Vec::with_capacity(LOST_SERVER_EXIT_RECORD.len() + 1);
        if !matches!(self.line, ControlLine::Prefix(0)) {
            suffix.push(b'\n');
        }
        suffix.extend_from_slice(LOST_SERVER_EXIT_RECORD);
        Some(suffix)
    }

    fn observe_byte(&mut self, byte: u8) {
        if matches!(self.line, ControlLine::Prefix(0)) && self.terminal_exit_delivered {
            self.terminal_exit_delivered = false;
        }

        self.line = match std::mem::take(&mut self.line) {
            ControlLine::Prefix(matched) if matched < EXIT_RECORD_PREFIX.len() => {
                if byte == EXIT_RECORD_PREFIX[matched] {
                    ControlLine::Prefix(matched + 1)
                } else if byte == b'\n' {
                    self.complete_line(false)
                } else {
                    ControlLine::Other
                }
            }
            ControlLine::Prefix(_) if byte == b'\n' => self.complete_line(true),
            ControlLine::Prefix(_) if byte == b' ' => ControlLine::ExitReason,
            ControlLine::Prefix(_) => ControlLine::Other,
            ControlLine::ExitReason if byte == b'\n' => self.complete_line(true),
            ControlLine::ExitReason => ControlLine::ExitReason,
            ControlLine::Other if byte == b'\n' => self.complete_line(false),
            ControlLine::Other => ControlLine::Other,
        };
    }

    fn complete_line(&mut self, is_exit: bool) -> ControlLine {
        self.terminal_exit_delivered = is_exit;
        ControlLine::Prefix(0)
    }
}

/// Longest line prefix kept to classify a record; guard lines are far shorter.
const RECORD_PREFIX_CAPACITY: usize = 64;
const BEGIN_GUARD_PREFIX: &[u8] = b"%begin ";
const END_GUARD_PREFIX: &[u8] = b"%end ";
const ERROR_GUARD_PREFIX: &[u8] = b"%error ";
const SESSION_CHANGED_PREFIX: &[u8] = b"%session-changed ";
const PARSE_ERROR_PREFIX: &[u8] = b"parse error: ";

/// Command outcomes the client can prove from its own control stream.
///
/// tmux 3.7b's server sets the client's exit status to 1 when a command of
/// that client fails to execute (`cmdq_error`), but not on a control-input
/// parse error. That status reaches the process only when the client exits
/// with `MSG_EXIT`, not after a detach or a server shutdown. The `%exit`
/// record is identical in all three cases, so the client claims the failure
/// only where no detach or shutdown can intervene: it never joined a session
/// (no `%session-changed`), so the server ended it once its command-line
/// commands had run. The `%begin`/`%error` block of a failed command ends with
/// its own guard tuple, which keeps command output that merely looks like a
/// guard from closing the block.
#[derive(Debug, Default)]
struct CommandOutcomes {
    record: Vec<u8>,
    record_overflowed: bool,
    open_block: Option<CommandBlock>,
    session_joined: bool,
    command_failed: bool,
}

#[derive(Debug)]
struct CommandBlock {
    guard_tuple: Vec<u8>,
    from_control_input: bool,
    body_lines: usize,
    first_line_is_parse_error: bool,
}

impl CommandBlock {
    fn new(guard_tuple: &[u8]) -> Self {
        Self {
            guard_tuple: guard_tuple.to_vec(),
            from_control_input: guard_tuple.rsplit(|byte| *byte == b' ').next() == Some(b"1"),
            body_lines: 0,
            first_line_is_parse_error: false,
        }
    }

    /// A control-input line that failed to parse: tmux reports it in its own
    /// block as one `parse error: ` line without failing the client.
    fn is_control_input_parse_error(&self) -> bool {
        self.from_control_input && self.body_lines == 1 && self.first_line_is_parse_error
    }
}

impl CommandOutcomes {
    fn status(&self) -> ControlModeStatus {
        if self.command_failed && !self.session_joined {
            ControlModeStatus::CommandFailed
        } else {
            ControlModeStatus::Success
        }
    }

    fn observe_byte(&mut self, byte: u8) {
        if byte == b'\n' {
            self.complete_record();
        } else if self.record.len() < RECORD_PREFIX_CAPACITY {
            self.record.push(byte);
        } else {
            self.record_overflowed = true;
        }
    }

    fn complete_record(&mut self) {
        let record = std::mem::take(&mut self.record);
        let whole = !std::mem::take(&mut self.record_overflowed);
        if let Some(block) = self.open_block.as_mut() {
            let closing = |prefix: &[u8]| {
                whole && record.strip_prefix(prefix) == Some(block.guard_tuple.as_slice())
            };
            if closing(END_GUARD_PREFIX) {
                self.open_block = None;
            } else if closing(ERROR_GUARD_PREFIX) {
                self.command_failed |= !block.is_control_input_parse_error();
                self.open_block = None;
            } else {
                if block.body_lines == 0 {
                    block.first_line_is_parse_error = record.starts_with(PARSE_ERROR_PREFIX);
                }
                block.body_lines += 1;
            }
            return;
        }
        if !whole {
            return;
        }
        if let Some(guard_tuple) = record.strip_prefix(BEGIN_GUARD_PREFIX) {
            self.open_block = Some(CommandBlock::new(guard_tuple));
        } else if record.starts_with(SESSION_CHANGED_PREFIX) {
            self.session_joined = true;
        }
    }
}

#[cfg(unix)]
pub(super) fn copy_control_output<R>(
    mut stream: R,
    output: &mut impl Write,
) -> io::Result<ControlOutputEnd>
where
    R: Read,
{
    let mut buffer = [0_u8; 8192];
    let mut progress = ControlOutputProgress::default();

    loop {
        let bytes_read = match stream.read(&mut buffer) {
            Ok(0) => return finish_closed_control_output(&progress, output),
            Ok(bytes_read) => bytes_read,
            Err(error) if peer_closed_control_stream(&error) => {
                return finish_closed_control_output(&progress, output);
            }
            Err(error) => return Err(error),
        };
        output.write_all(&buffer[..bytes_read])?;
        output.flush()?;
        progress.observe(&buffer[..bytes_read]);
    }
}

#[cfg(unix)]
fn finish_closed_control_output(
    progress: &ControlOutputProgress,
    output: &mut impl Write,
) -> io::Result<ControlOutputEnd> {
    if let Some(suffix) = progress.lost_server_suffix() {
        output.write_all(&suffix)?;
        output.flush()?;
    }
    Ok(progress.closed_end())
}

#[cfg(unix)]
fn peer_closed_control_stream(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected | io::ErrorKind::UnexpectedEof
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::{copy_control_output, ControlModeStatus, ControlOutputEnd};
    use std::io::{self, Read};

    struct BytesThenReset {
        bytes: Option<&'static [u8]>,
    }

    impl Read for BytesThenReset {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.bytes.take() else {
                return Err(io::Error::from(io::ErrorKind::ConnectionReset));
            };
            buffer[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    fn copy_all(stream: impl Read) -> (ControlOutputEnd, Vec<u8>) {
        let mut output = Vec::new();
        let end = copy_control_output(stream, &mut output).expect("closed transport");
        (end, output)
    }

    #[test]
    fn complete_exit_record_makes_peer_reset_a_clean_control_end() {
        let (end, output) = copy_all(BytesThenReset {
            bytes: Some(b"%begin 1 1 0\n%end 1 1 0\n%exit\n"),
        });

        assert_eq!(
            end,
            ControlOutputEnd::ExitDelivered(ControlModeStatus::Success)
        );
        assert_eq!(output, b"%begin 1 1 0\n%end 1 1 0\n%exit\n");
    }

    #[test]
    fn complete_exit_record_with_reason_makes_eof_a_clean_control_end() {
        let (end, output) = copy_all(&b"%output %0 x\n%exit detached\n"[..]);

        assert_eq!(
            end,
            ControlOutputEnd::ExitDelivered(ControlModeStatus::Success)
        );
        assert_eq!(output, b"%output %0 x\n%exit detached\n");
    }

    fn status_of(transcript: &[u8]) -> ControlModeStatus {
        match copy_all(transcript).0 {
            ControlOutputEnd::ExitDelivered(status) => status,
            ControlOutputEnd::ServerLost => panic!("transcript must end with %exit"),
        }
    }

    #[test]
    fn failed_command_of_a_client_without_a_session_is_a_failed_session() {
        // tmux 3.7b: `-C attach -t nope` and `-C has-session -t nope` exit 1.
        assert_eq!(
            status_of(b"%begin 1 1 0\ncan't find session: nope\n%error 1 1 0\n%exit\n"),
            ControlModeStatus::CommandFailed
        );
        assert_eq!(
            status_of(
                b"%begin 1 1 0\na\n%end 1 1 0\n%begin 1 2 0\nno sessions\n%error 1 2 0\n%exit\n"
            ),
            ControlModeStatus::CommandFailed
        );
    }

    #[test]
    fn successful_commands_of_a_client_without_a_session_succeed() {
        assert_eq!(
            status_of(b"%begin 1 1 0\n%end 1 1 0\n%sessions-changed\n%exit\n"),
            ControlModeStatus::Success
        );
    }

    #[test]
    fn failed_command_after_joining_a_session_keeps_the_clean_exit() {
        // A detach or a server shutdown would end this session with the same
        // `%exit` record, and tmux then exits 0 despite the earlier failure.
        assert_eq!(
            status_of(
                b"%begin 1 1 0\n%end 1 1 0\n%session-changed $0 s\n\
                  %begin 1 2 1\ncan't find session: nope\n%error 1 2 1\n%exit\n"
            ),
            ControlModeStatus::Success
        );
    }

    #[test]
    fn control_input_parse_error_does_not_fail_the_session() {
        assert_eq!(
            status_of(b"%begin 1 2 1\nparse error: unknown command: bogus\n%error 1 2 1\n%exit\n"),
            ControlModeStatus::Success
        );
        // Output that only starts like a parse error stays a command failure.
        assert_eq!(
            status_of(
                b"%begin 1 2 1\nparse error: z\ncan't find session: nope\n%error 1 2 1\n%exit\n"
            ),
            ControlModeStatus::CommandFailed
        );
    }

    #[test]
    fn command_line_parse_error_fails_the_session() {
        // tmux rejects an unparsable command-line command before connecting.
        assert_eq!(
            status_of(b"%begin 1 1 0\nparse error: unknown command: bogus\n%error 1 1 0\n%exit\n"),
            ControlModeStatus::CommandFailed
        );
    }

    #[test]
    fn command_output_that_looks_like_a_guard_does_not_close_the_block() {
        assert_eq!(
            status_of(b"%begin 1 1 0\n%error 1 9 0\n%session-changed $0 s\n%end 1 1 0\n%exit\n"),
            ControlModeStatus::Success
        );
        assert_eq!(
            status_of(b"%begin 1 1 0\n%end 1 9 0\nno sessions\n%error 1 1 0\n%exit\n"),
            ControlModeStatus::CommandFailed
        );
    }

    #[test]
    fn records_split_across_reads_are_classified_whole() {
        struct OneByteAtATime(&'static [u8]);
        impl Read for OneByteAtATime {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let Some((first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                buffer[0] = *first;
                self.0 = rest;
                Ok(1)
            }
        }

        let (end, _) = copy_all(OneByteAtATime(
            b"%begin 1 1 0\ncan't find session: nope\n%error 1 1 0\n%exit\n",
        ));
        assert_eq!(
            end,
            ControlOutputEnd::ExitDelivered(ControlModeStatus::CommandFailed)
        );
    }

    #[test]
    fn failed_command_without_exit_record_still_reports_the_lost_server() {
        let (end, _) = copy_all(&b"%begin 1 1 0\nno sessions\n%error 1 1 0\n"[..]);

        assert_eq!(end, ControlOutputEnd::ServerLost);
    }

    #[test]
    fn eof_without_exit_record_reports_the_lost_server() {
        let (end, output) = copy_all(&b"%begin 1 1 0\n%end 1 1 0\n%output %0 $ \n"[..]);

        assert_eq!(end, ControlOutputEnd::ServerLost);
        assert_eq!(
            output,
            b"%begin 1 1 0\n%end 1 1 0\n%output %0 $ \n%exit server exited unexpectedly\n"
        );
    }

    #[test]
    fn eof_before_any_output_reports_the_lost_server() {
        let (end, output) = copy_all(&b""[..]);

        assert_eq!(end, ControlOutputEnd::ServerLost);
        assert_eq!(output, b"%exit server exited unexpectedly\n");
    }

    #[test]
    fn eof_inside_a_record_terminates_it_before_the_lost_server_record() {
        let (end, output) = copy_all(&b"%output %0 \\033[01;32muser@h"[..]);

        assert_eq!(end, ControlOutputEnd::ServerLost);
        assert_eq!(
            output,
            b"%output %0 \\033[01;32muser@h\n%exit server exited unexpectedly\n"
        );
    }

    #[test]
    fn exit_record_followed_by_more_output_is_not_terminal() {
        let (end, output) = copy_all(&b"%exit\n%output %0 x\n"[..]);

        assert_eq!(end, ControlOutputEnd::ServerLost);
        assert_eq!(
            output,
            b"%exit\n%output %0 x\n%exit server exited unexpectedly\n"
        );
    }

    #[test]
    fn partial_exit_record_does_not_hide_peer_reset() {
        let (end, output) = copy_all(BytesThenReset {
            bytes: Some(b"%begin 1 1 0\n%end 1 1 0\n%exi"),
        });

        assert_eq!(end, ControlOutputEnd::ServerLost);
        assert_eq!(
            output,
            b"%begin 1 1 0\n%end 1 1 0\n%exi\n%exit server exited unexpectedly\n"
        );
    }

    #[test]
    fn unrelated_transport_errors_stay_errors() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::InvalidData))
            }
        }

        let error = copy_control_output(Failing, &mut Vec::new()).expect_err("i/o failure");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
