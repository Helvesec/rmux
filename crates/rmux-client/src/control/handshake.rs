//! Control-mode upgrade handshake: the request that turns an RPC connection
//! into a raw control stream, and the initial commands sent across it.

use std::io::Write;

use rmux_ipc::BlockingLocalStream;
use rmux_proto::{
    ClientTerminalContext, ControlMode, ControlModeRequest, Request, Response,
    MAX_INITIAL_CONTROL_COMMANDS,
};

use crate::{
    connection::{read_response_frame_exact, request_write_error, Connection, ControlTransition},
    ClientError,
};

impl Connection {
    /// Requests a control-mode upgrade and, on success, yields the raw local
    /// stream for tmux-compatible text control traffic.
    pub fn begin_control_mode(
        self,
        mode: ControlMode,
        client_terminal: ClientTerminalContext,
    ) -> Result<ControlTransition, ClientError> {
        self.begin_control_mode_with_initial_commands(mode, client_terminal, &[])
    }

    /// Requests a control-mode upgrade and writes command-line commands across
    /// the upgrade boundary so the server can frame them as tmux argv commands.
    pub fn begin_control_mode_with_initial_commands(
        self,
        mode: ControlMode,
        client_terminal: ClientTerminalContext,
        initial_commands: &[String],
    ) -> Result<ControlTransition, ClientError> {
        self.begin_control_transport(mode, client_terminal, initial_commands, false)
    }

    /// Negotiates versioned output records for the CLI driver. Older servers
    /// retain the legacy raw stream and its limited inferred exit status.
    pub fn begin_control_mode_with_status(
        self,
        mode: ControlMode,
        client_terminal: ClientTerminalContext,
        initial_commands: &[String],
    ) -> Result<ControlTransition, ClientError> {
        self.begin_control_transport(mode, client_terminal, initial_commands, true)
    }

    fn begin_control_transport(
        mut self,
        mode: ControlMode,
        client_terminal: ClientTerminalContext,
        initial_commands: &[String],
        negotiate_records: bool,
    ) -> Result<ControlTransition, ClientError> {
        if initial_commands.len() > MAX_INITIAL_CONTROL_COMMANDS {
            return Err(ClientError::Protocol(rmux_proto::RmuxError::Server(
                format!(
                    "too many initial control-mode commands: {} (maximum {MAX_INITIAL_CONTROL_COMMANDS})",
                    initial_commands.len()
                ),
            )));
        }
        let initial_command_count = u32::try_from(initial_commands.len()).map_err(|_| {
            ClientError::Protocol(rmux_proto::RmuxError::Server(
                "too many initial control-mode commands".to_owned(),
            ))
        })?;
        let records = negotiate_records
            && self.supports_capability(rmux_proto::capabilities::CAPABILITY_CONTROL_RECORDS_V1)?;
        if records {
            match self.roundtrip(&Request::Handshake(
                rmux_proto::HandshakeRequest::requiring([
                    rmux_proto::capabilities::CAPABILITY_CONTROL_RECORDS_V1,
                ]),
            ))? {
                Response::Handshake(_) => {}
                other => return Ok(ControlTransition::Rejected(other)),
            }
        }
        self.write_request(&Request::ControlMode(ControlModeRequest {
            mode,
            client_terminal,
            initial_command_count,
        }))?;
        write_initial_control_commands(self.stream_mut(), initial_commands)?;
        let response = read_response_frame_exact(self.stream_mut())?;

        match response {
            Response::ControlMode(response) => {
                let mut upgrade = self.into_control_upgrade(response)?;
                upgrade.records = records;
                Ok(ControlTransition::Upgraded(upgrade))
            }
            other => Ok(ControlTransition::Rejected(other)),
        }
    }
}

fn write_initial_control_commands(
    stream: &mut BlockingLocalStream,
    initial_commands: &[String],
) -> Result<(), ClientError> {
    for command in initial_commands {
        stream
            .write_all(command.as_bytes())
            .map_err(|error| request_write_error(stream, error))?;
        stream
            .write_all(b"\n")
            .map_err(|error| request_write_error(stream, error))?;
    }
    Ok(())
}
