use rmux_proto::{CommandOutput, RmuxError};

use super::diagnostics::config_error_lines;

pub(crate) fn nonempty_stdout(stdout: Vec<u8>) -> Option<CommandOutput> {
    if stdout.is_empty() {
        None
    } else {
        Some(CommandOutput::from_stdout(stdout))
    }
}

pub(crate) fn append_error_output(stdout: &mut Vec<u8>, error: &RmuxError) {
    for line in config_error_lines(error) {
        stdout.extend_from_slice(line.as_bytes());
        stdout.push(b'\n');
    }
}
