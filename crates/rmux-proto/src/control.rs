//! tmux-compatible control-mode text protocol helpers.

use serde::{Deserialize, Serialize};

/// tmux-compatible control-mode transport flavor negotiated over the detached
/// bincode RPC channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMode {
    /// Plain `-C` control mode.
    Plain,
    /// `-CC` control-control mode with DCS wrapping.
    ControlControl,
}

impl ControlMode {
    /// Returns the tmux top-level `-C` count as parsed by Clap.
    #[must_use]
    pub const fn from_count(count: u8) -> Self {
        if count >= 2 {
            Self::ControlControl
        } else {
            Self::Plain
        }
    }

    /// Returns `true` when the client requested tmux control-control mode.
    #[must_use]
    pub const fn is_control_control(self) -> bool {
        matches!(self, Self::ControlControl)
    }
}

/// Low watermark for buffered control-mode output.
pub const CONTROL_BUFFER_LOW: usize = 512;
/// High watermark for buffered control-mode output.
pub const CONTROL_BUFFER_HIGH: usize = 8192;
/// Minimum control-mode write chunk tmux attempts before stopping.
pub const CONTROL_WRITE_MINIMUM: usize = 32;
/// Maximum age for queued control-mode pane output before disconnecting.
pub const CONTROL_MAXIMUM_AGE_MS: u64 = 300_000;
/// Startup prefix for control-control mode.
pub const CONTROL_CONTROL_START: &str = "\u{1b}P1000p";
/// Shutdown suffix for control-control mode.
pub const CONTROL_CONTROL_END: &str = "\u{1b}\\";
/// Private in-band marker used by Windows rmux clients to represent stdin EOF.
///
/// Windows named pipes do not provide a Unix-style write-half close while the
/// same client handle keeps reading server output. This marker is consumed by
/// the rmux server before command parsing and is never emitted as user output.
pub const CONTROL_STDIN_EOF_MARKER: &str = "\0rmux-control-eof";

/// Detached upgrade request that switches a connection into tmux-compatible
/// control mode while leaving the underlying RPC framing unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClientTerminalContext {
    /// Explicit terminal feature names contributed by top-level `-2` and `-T`.
    #[serde(default)]
    pub terminal_features: Vec<String>,
    /// Whether the invoking client should be treated as UTF-8 capable.
    #[serde(default)]
    pub utf8: bool,
}

/// Maximum number of command-line commands accepted across a control-mode
/// upgrade boundary.
pub const MAX_INITIAL_CONTROL_COMMANDS: usize = 1024;

/// Detached upgrade request that switches a connection into tmux-compatible
/// control mode while leaving the underlying RPC framing unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlModeRequest {
    /// The requested control-mode flavor.
    pub mode: ControlMode,
    /// Terminal/runtime hints captured from the invoking client.
    #[serde(default)]
    pub client_terminal: ClientTerminalContext,
    /// Number of command-line commands written immediately after the upgrade frame.
    ///
    /// This is explicit because local sockets and Windows named pipes are byte
    /// streams: write boundaries cannot identify argv commands reliably.
    #[serde(default)]
    pub initial_command_count: u32,
}

/// Detached upgrade response acknowledging entry into control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlModeResponse {
    /// The accepted control-mode flavor.
    pub mode: ControlMode,
}

/// Guard kind for `%begin`, `%end`, and `%error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlGuardKind {
    /// `%begin`
    Begin,
    /// `%end`
    End,
    /// `%error`
    Error,
}

impl ControlGuardKind {
    /// Returns the tmux control-guard keyword.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::End => "end",
            Self::Error => "error",
        }
    }
}

/// Formats a tmux-compatible guard line.
#[must_use]
pub fn format_guard_line(
    kind: ControlGuardKind,
    time_secs: i64,
    command_number: u64,
    flags: u8,
) -> String {
    format!(
        "%{} {} {} {}\n",
        kind.as_str(),
        time_secs,
        command_number,
        flags
    )
}

/// Formats a tmux-compatible `%output` line for pane bytes.
#[must_use]
pub fn format_output_line(pane_id: u32, bytes: &[u8]) -> String {
    format!("%output %{} {}\n", pane_id, octal_escape(bytes))
}

/// Formats a tmux-compatible `%extended-output` line for pane bytes.
#[must_use]
pub fn format_extended_output_line(pane_id: u32, age_ms: u64, bytes: &[u8]) -> String {
    format!(
        "%extended-output %{} {} : {}\n",
        pane_id,
        age_ms,
        octal_escape(bytes)
    )
}

/// Formats a tmux-compatible `%pause` line.
#[must_use]
pub fn format_pause_line(pane_id: u32) -> String {
    format!("%pause %{}\n", pane_id)
}

/// Formats a tmux-compatible `%continue` line.
#[must_use]
pub fn format_continue_line(pane_id: u32) -> String {
    format!("%continue %{}\n", pane_id)
}

/// Formats a tmux-compatible `%exit` line.
#[must_use]
pub fn format_exit_line(reason: Option<&str>) -> String {
    match reason {
        Some(reason) if !reason.is_empty() => format!("%exit {reason}\n"),
        _ => "%exit\n".to_owned(),
    }
}

/// Formats a tmux-compatible control-mode data payload.
///
/// ASCII control bytes and `\` are `\NNN` octal-escaped. Valid UTF-8
/// text is left intact so clients that expect tmux-style Unicode output do
/// not see every non-ASCII byte expanded into octal sequences. Invalid UTF-8
/// bytes are escaped one byte at a time.
#[must_use]
pub fn octal_escape(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                push_escaped_text(&mut output, valid);
                break;
            }
            Err(error) if error.valid_up_to() > 0 => {
                let valid_end = offset + error.valid_up_to();
                let valid = std::str::from_utf8(&bytes[offset..valid_end])
                    .expect("valid_up_to must describe valid UTF-8");
                push_escaped_text(&mut output, valid);
                offset = valid_end;
            }
            Err(error) => {
                let invalid_len = error.error_len().unwrap_or(1);
                for &byte in &bytes[offset..offset + invalid_len] {
                    push_octal_escape(&mut output, byte);
                }
                offset += invalid_len;
            }
        }
    }
    output
}

fn push_escaped_text(output: &mut String, text: &str) {
    for character in text.chars() {
        if character.is_ascii() {
            let byte = character as u8;
            if needs_octal_escape(byte) {
                push_octal_escape(output, byte);
            } else {
                output.push(character);
            }
        } else {
            output.push(character);
        }
    }
}

const fn needs_octal_escape(byte: u8) -> bool {
    byte < b' ' || byte == b'\\'
}

fn push_octal_escape(output: &mut String, byte: u8) {
    output.push('\\');
    output.push(char::from(b'0' + ((byte >> 6) & 0x7)));
    output.push(char::from(b'0' + ((byte >> 3) & 0x7)));
    output.push(char::from(b'0' + (byte & 0x7)));
}

#[cfg(test)]
mod tests {
    use super::{
        format_exit_line, format_extended_output_line, format_guard_line, format_output_line,
        octal_escape, ControlGuardKind, ControlMode,
    };

    #[test]
    fn count_two_selects_control_control_mode() {
        assert_eq!(ControlMode::from_count(0), ControlMode::Plain);
        assert_eq!(ControlMode::from_count(1), ControlMode::Plain);
        assert_eq!(ControlMode::from_count(2), ControlMode::ControlControl);
        assert_eq!(ControlMode::from_count(3), ControlMode::ControlControl);
    }

    #[test]
    fn octal_escape_matches_tmux_rules_for_control_bytes() {
        assert_eq!(octal_escape(b"abc"), "abc");
        assert_eq!(octal_escape(b"a\nb"), "a\\012b");
        assert_eq!(octal_escape(b"\\\0"), "\\134\\000");
        assert_eq!(octal_escape(b" "), " ");
        assert_eq!(octal_escape(b"~"), "~");
        // DEL is printable from tmux control-mode's perspective.
        assert_eq!(octal_escape(b"\x7f"), "\x7f");
        assert_eq!(octal_escape("é".as_bytes()), "é");
        assert_eq!(octal_escape("hello 👋".as_bytes()), "hello 👋");
        // Invalid UTF-8 still round-trips as octal bytes.
        assert_eq!(octal_escape(b"\x80"), "\\200");
        assert_eq!(octal_escape(b"\xff"), "\\377");
        // All printable ASCII passes through literally.
        for byte in b' '..b'\x7f' {
            if byte == b'\\' {
                continue;
            }
            let escaped = octal_escape(&[byte]);
            assert_eq!(
                escaped.len(),
                1,
                "byte {byte:#04x} should be literal, got {escaped:?}"
            );
        }
    }

    #[test]
    fn octal_escape_covers_every_ascii_control_byte() {
        for byte in 0_u8..b' ' {
            let escaped = octal_escape(&[byte]);
            assert_eq!(
                escaped,
                format!("\\{byte:03o}"),
                "control byte {byte:#04x} should be octal escaped"
            );
        }
        assert_eq!(octal_escape(b"\\"), "\\134");
    }

    #[test]
    fn guard_and_output_lines_are_newline_terminated() {
        assert_eq!(
            format_guard_line(ControlGuardKind::Begin, 10, 22, 1),
            "%begin 10 22 1\n"
        );
        assert_eq!(format_output_line(7, b"hi\n"), "%output %7 hi\\012\n");
        assert_eq!(
            format_extended_output_line(7, 15, b"hi"),
            "%extended-output %7 15 : hi\n"
        );
        assert_eq!(format_exit_line(None), "%exit\n");
        assert_eq!(
            format_exit_line(Some("too far behind")),
            "%exit too far behind\n"
        );
    }
}
