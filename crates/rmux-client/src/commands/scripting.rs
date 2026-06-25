use rmux_proto::{
    IfShellRequest, Request, Response, RunShellDelaySeconds, RunShellRequest, Target, WaitForMode,
    WaitForRequest,
};
use std::path::PathBuf;

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `run-shell` request over the detached RPC channel without a
    /// response read timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn run_shell(
        &mut self,
        command: String,
        background: bool,
        as_commands: bool,
        show_stderr: bool,
        delay_seconds: Option<f64>,
        start_directory: Option<PathBuf>,
        target: Option<rmux_proto::PaneTarget>,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::RunShell(Box::new(RunShellRequest {
            command,
            background,
            as_commands,
            show_stderr,
            delay_seconds: delay_seconds.map(RunShellDelaySeconds),
            start_directory,
            target,
            source_depth: None,
        })))
    }

    /// Sends an `if-shell` request over the detached RPC channel without a
    /// response read timeout.
    pub fn if_shell(
        &mut self,
        condition: String,
        format_mode: bool,
        then_command: String,
        else_command: Option<String>,
        target: Option<Target>,
        background: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::IfShell(Box::new(IfShellRequest {
            condition,
            format_mode,
            then_command,
            else_command,
            target,
            caller_cwd: current_working_directory(),
            background,
        })))
    }

    /// Sends a `wait-for` request over the detached RPC channel without a
    /// response read timeout.
    pub fn wait_for(
        &mut self,
        channel: String,
        mode: WaitForMode,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::WaitFor(WaitForRequest { channel, mode }))
    }
}

fn current_working_directory() -> Option<PathBuf> {
    std::env::current_dir().ok()
}
