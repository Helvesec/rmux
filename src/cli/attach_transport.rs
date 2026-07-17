use std::path::Path;

#[cfg(unix)]
use rmux_client::attach_terminal_with_initial_bytes;
#[cfg(unix)]
use rmux_client::attach_terminal_with_initial_bytes_and_resize_geometry;
#[cfg(windows)]
use rmux_client::attach_terminal_with_initial_bytes_and_windows_console_key;
#[cfg(unix)]
use rmux_client::AttachError;
use rmux_client::{connect, AttachSessionUpgrade, AttachTransition, ClientError, Connection};
use rmux_proto::request::{AttachSessionExt2Request, AttachSessionExt3Request, ListClientsRequest};
#[cfg(windows)]
use rmux_proto::CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY;
use rmux_proto::{
    ErrorResponse, Response, CAPABILITY_ATTACH_RENDER, CAPABILITY_ATTACH_RESIZE_GEOMETRY,
};

use crate::client_terminal::ATTACH_TERMINAL_REQUIRED_MESSAGE;

use super::{expect_command_success, unexpected_response, ExitFailure};

pub(super) struct QueuedAttachSession {
    upgrade: AttachSessionUpgrade,
    capabilities: AttachClientCapabilities,
}

pub(super) enum QueuedAttachSessionResult {
    Detached(Box<QueuedAttachSession>),
    Completed(i32),
}

struct AttachClientCapabilities {
    resize_geometry: bool,
    #[cfg(windows)]
    windows_console_key: bool,
}

impl QueuedAttachSession {
    pub(super) fn run(self) -> Result<i32, ExitFailure> {
        run_attach_upgrade(self.upgrade, self.capabilities)
    }
}

pub(super) fn begin_queued_attach(
    connection: Connection,
    request: AttachSessionExt2Request,
) -> Result<QueuedAttachSessionResult, ExitFailure> {
    require_attach_terminal()?;
    let (transition, capabilities) = begin_attach(connection, request)?;
    match transition {
        AttachTransition::Upgraded(upgrade) => Ok(QueuedAttachSessionResult::Detached(Box::new(
            QueuedAttachSession {
                upgrade,
                capabilities,
            },
        ))),
        AttachTransition::Rejected(response) => {
            expect_command_success(response, "attach-session")?;
            Ok(QueuedAttachSessionResult::Completed(0))
        }
    }
}

pub(super) fn queued_attach_session_is_active(socket_path: &Path) -> Result<bool, ExitFailure> {
    let mut connection = match connect(socket_path) {
        Ok(connection) => connection,
        Err(error) => {
            let failure = ExitFailure::from_client_connect(socket_path, error);
            return if failure.is_server_absent() {
                Ok(false)
            } else {
                Err(failure)
            };
        }
    };
    let response = connection
        .list_clients(ListClientsRequest {
            format: Some("#{client_pid}\n".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_session: None,
        })
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::ListClients(response) => {
            let requester_pid = std::process::id().to_string();
            Ok(response
                .command_output()
                .stdout()
                .split(|byte| *byte == b'\n')
                .any(|line| line == requester_pid.as_bytes()))
        }
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        response => Err(unexpected_response("list-clients", &response)),
    }
}

pub(super) fn attach_with_connection(
    connection: Connection,
    request: AttachSessionExt2Request,
) -> Result<i32, ExitFailure> {
    require_attach_terminal()?;
    let (transition, capabilities) = begin_attach(connection, request)?;
    match transition {
        AttachTransition::Upgraded(upgrade) => run_attach_upgrade(upgrade, capabilities),
        AttachTransition::Rejected(response) => {
            expect_command_success(response, "attach-session")?;
            Ok(0)
        }
    }
}

pub(super) fn require_attach_terminal() -> Result<(), ExitFailure> {
    crate::client_terminal::require_attach_terminal()
        .map_err(|message| ExitFailure::new(1, message))
}

fn begin_attach(
    mut connection: Connection,
    request: AttachSessionExt2Request,
) -> Result<(AttachTransition, AttachClientCapabilities), ExitFailure> {
    let resize_geometry = connection
        .supports_capability(CAPABILITY_ATTACH_RESIZE_GEOMETRY)
        .map_err(ExitFailure::from_client)?;
    let render = connection
        .supports_capability(CAPABILITY_ATTACH_RENDER)
        .map_err(ExitFailure::from_client)?;
    #[cfg(windows)]
    let windows_console_key = connection
        .supports_capability(CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY)
        .map_err(ExitFailure::from_client)?;
    let mut advertised = Vec::new();
    if render {
        advertised.push(CAPABILITY_ATTACH_RENDER.to_owned());
    }
    #[cfg(windows)]
    if windows_console_key {
        advertised.push(CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY.to_owned());
    }
    let transition = if !advertised.is_empty() {
        connection
            .begin_attach_with_capabilities(AttachSessionExt3Request::from_ext2(
                request, advertised,
            ))
            .map_err(ExitFailure::from_client)?
    } else {
        connection
            .begin_attach_with_target_spec(request)
            .map_err(ExitFailure::from_client)?
    };
    Ok((
        transition,
        AttachClientCapabilities {
            resize_geometry,
            #[cfg(windows)]
            windows_console_key,
        },
    ))
}

fn run_attach_upgrade(
    upgrade: AttachSessionUpgrade,
    capabilities: AttachClientCapabilities,
) -> Result<i32, ExitFailure> {
    let AttachClientCapabilities {
        resize_geometry,
        #[cfg(windows)]
        windows_console_key,
    } = capabilities;
    let (stream, initial_bytes) = upgrade.into_parts();
    #[cfg(unix)]
    {
        if resize_geometry {
            attach_terminal_with_initial_bytes_and_resize_geometry(stream, initial_bytes)
                .map_err(attach_terminal_exit_failure)?;
        } else {
            attach_terminal_with_initial_bytes(stream, initial_bytes)
                .map_err(attach_terminal_exit_failure)?;
        }
    }
    #[cfg(windows)]
    {
        let _ = resize_geometry;
        attach_terminal_with_initial_bytes_and_windows_console_key(
            stream,
            initial_bytes,
            windows_console_key,
        )
        .map_err(attach_terminal_exit_failure)?;
    }
    Ok(0)
}

fn attach_terminal_exit_failure(error: ClientError) -> ExitFailure {
    if attach_terminal_failed_because_stdio_is_not_terminal(&error) {
        ExitFailure::new(1, ATTACH_TERMINAL_REQUIRED_MESSAGE)
    } else {
        ExitFailure::from_client(error)
    }
}

#[cfg(unix)]
fn attach_terminal_failed_because_stdio_is_not_terminal(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Attach(AttachError::Termios(errno))
            if matches!(errno.raw_os_error(), libc::ENOTTY | libc::ENODEV)
    )
}

#[cfg(windows)]
fn attach_terminal_failed_because_stdio_is_not_terminal(_error: &ClientError) -> bool {
    false
}
