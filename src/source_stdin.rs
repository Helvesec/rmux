//! Keep the caller's open file description alive for a detached source consumer.
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use rmux_client::{ClientError, Connection};
use rmux_proto::{Response, SourceFileInputV1Request, CAPABILITY_SOURCE_FILE_INPUT_RELAY_V1};

const READER_FLAG: &str = "--internal-source-stdin-reader";
const CREDENTIALS: &str = "RMUX_INTERNAL_SOURCE_STDIN_CREDENTIALS";

pub(crate) fn execute(
    connection: &mut Connection,
    socket: &Path,
    mut offer: SourceFileInputV1Request,
) -> Result<Response, ClientError> {
    let relay = offer.requests_input_pull()
        && connection.supports_capability(CAPABILITY_SOURCE_FILE_INPUT_RELAY_V1)?;
    if relay {
        offer
            .handshake
            .required_capabilities
            .push(CAPABILITY_SOURCE_FILE_INPUT_RELAY_V1.into());
    }
    let response = connection.source_file_with_stdin(offer, io::stdin())?;
    let Response::SourceFileRelayV1(credentials) = response else {
        return Ok(response);
    };
    if !relay {
        return Err(io::Error::other("unexpected source-file stdin relay").into());
    }
    let ticket = u128::from_be_bytes(credentials.ticket);
    let proof = u128::from_be_bytes(credentials.proof);
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(READER_FLAG)
        .arg(socket)
        .env(CREDENTIALS, format!("{ticket:032x}{proof:032x}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, false);
    let mut reader = command.spawn()?;
    // The invoking CLI may execute other commands before it exits.
    std::thread::Builder::new()
        .name("source-stdin-reaper".into())
        .spawn(move || reader.wait())?;
    connection.read_response_without_read_timeout()
}

pub(crate) fn reader_exit_code(args: &[std::ffi::OsString]) -> Option<i32> {
    if args.get(1)? != READER_FLAG {
        return None;
    }
    let result = (|| {
        let [_, _, socket] = args else { return None };
        let credentials = std::env::var(CREDENTIALS).ok()?;
        if credentials.len() != 64 || !credentials.is_ascii() {
            return None;
        }
        let ticket = u128::from_str_radix(&credentials[..32], 16).ok()?;
        let proof = u128::from_str_radix(&credentials[32..], 16).ok()?;
        rmux_client::connect(Path::new(socket))
            .ok()?
            .serve_source_file_stdin(ticket.to_be_bytes(), proof.to_be_bytes())
            .ok()
    })();
    Some(i32::from(result.is_none()))
}
