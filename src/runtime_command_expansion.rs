use std::error::Error;
use std::fmt;

use rmux_client::{ClientError, Connection};
use rmux_proto::{
    encode_internal_runtime_command_arguments, Response, RmuxError,
    CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION, INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH,
    RMUX_WIRE_VERSION,
};

#[derive(Debug)]
pub(crate) enum RuntimeCommandExpansionError {
    Client(ClientError),
    Server(RmuxError),
    Protocol(String),
}

impl RuntimeCommandExpansionError {
    pub(crate) fn previous_wire_version(&self) -> Option<u32> {
        let error = match self {
            Self::Client(ClientError::Protocol(error)) | Self::Server(error) => error,
            Self::Client(_) | Self::Protocol(_) => return None,
        };
        match error {
            RmuxError::UnsupportedWireVersion { got, .. }
                if (1..RMUX_WIRE_VERSION).contains(got) =>
            {
                Some(*got)
            }
            _ => None,
        }
    }
}

impl fmt::Display for RuntimeCommandExpansionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => error.fmt(formatter),
            Self::Server(error) => error.fmt(formatter),
            Self::Protocol(message) => formatter.write_str(message),
        }
    }
}

impl Error for RuntimeCommandExpansionError {}

/// Resolves server-owned command aliases without executing the command list.
///
/// The payload is serialized from the already-tokenized argv. This
/// avoids source-file expansion and preserves control bytes and literal shell
/// metacharacters exactly.
pub(crate) fn expand_runtime_command_segment(
    connection: &mut Connection,
    arguments: &[String],
) -> Result<Option<String>, RuntimeCommandExpansionError> {
    if !connection
        .supports_capability(CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION)
        .map_err(RuntimeCommandExpansionError::Client)?
    {
        return Ok(None);
    }
    let payload = encode_internal_runtime_command_arguments(arguments).map_err(|error| {
        RuntimeCommandExpansionError::Protocol(format!(
            "failed to encode runtime command arguments: {error}"
        ))
    })?;
    let response = connection
        .source_file(
            vec![INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH.to_owned()],
            false,
            true,
            true,
            false,
            None,
            Some(payload),
        )
        .map_err(RuntimeCommandExpansionError::Client)?;

    match response {
        Response::SourceFile(source) => {
            let stdout = source
                .command_output()
                .map_or(&[][..], |output| output.stdout());
            if source.exit_status().is_some_and(|status| status != 0) || !source.stderr().is_empty()
            {
                return Err(RuntimeCommandExpansionError::Protocol(
                    source_failure_message(stdout, source.stderr()),
                ));
            }
            decode_canonical_commands(stdout).map(Some)
        }
        Response::Error(error) => match error.error {
            RmuxError::Server(message) => Err(RuntimeCommandExpansionError::Protocol(message)),
            error => Err(RuntimeCommandExpansionError::Server(error)),
        },
        response => Err(RuntimeCommandExpansionError::Protocol(format!(
            "runtime command expansion returned {}",
            response.command_name()
        ))),
    }
}

fn decode_canonical_commands(output: &[u8]) -> Result<String, RuntimeCommandExpansionError> {
    std::str::from_utf8(output).map(str::to_owned).map_err(|_| {
        RuntimeCommandExpansionError::Protocol(
            "runtime command expansion returned invalid UTF-8".to_owned(),
        )
    })
}

fn source_failure_message(stdout: &[u8], stderr: &[u8]) -> String {
    let bytes = if stderr.is_empty() { stdout } else { stderr };
    String::from_utf8_lossy(bytes)
        .lines()
        .next_back()
        .filter(|line| !line.is_empty())
        .unwrap_or("runtime command expansion failed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{decode_canonical_commands, source_failure_message};

    #[test]
    fn canonical_decoder_preserves_lossless_payload_verbatim() {
        let canonical = "display-message \"a\\n\\r\\tb\"";
        assert_eq!(
            decode_canonical_commands(canonical.as_bytes()).expect("canonical command"),
            canonical
        );
    }

    #[test]
    fn source_failures_prefer_stderr_and_the_last_nonempty_line() {
        assert_eq!(source_failure_message(b"first\nsecond\n", b""), "second");
        assert_eq!(
            source_failure_message(b"ignored\n", b"stderr first\nstderr last\n"),
            "stderr last"
        );
    }
}
