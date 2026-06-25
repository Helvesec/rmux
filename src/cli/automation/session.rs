use std::path::Path;
use std::process::Command;
use std::time::Duration;

use rmux_proto::{KillSessionRequest, Response, SessionName};

use crate::cli_args::WithSessionArgs;
use crate::cli_response::tmux_cli_error_message;

use super::super::ExitFailure;
use super::common::{connect_cli, duration_millis, sleep_poll_interval};

pub(crate) fn run_with_session(
    args: WithSessionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let ttl_millis = duration_millis(args.ttl);
    let mut connection = connect_cli(socket_path)?;
    let lease = create_lease(&mut connection, args.session_name.clone(), ttl_millis)?;
    let mut child = match Command::new(&args.command[0])
        .args(&args.command[1..])
        .env("RMUX_SESSION", args.session_name.as_str())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = release_lease(&mut connection, args.session_name.clone(), lease.token);
            return Err(ExitFailure::new(
                1,
                format!("with-session failed to spawn child: {error}"),
            ));
        }
    };

    let renew_interval = renew_interval(args.ttl);
    let mut time_to_renew = renew_interval;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if args.kill_on_owner_exit {
                    kill_owned_session(&mut connection, args.session_name.clone())?;
                } else {
                    release_lease(&mut connection, args.session_name, lease.token)?;
                }
                return Ok(status.code().unwrap_or(1));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = release_lease(&mut connection, args.session_name, lease.token);
                return Err(ExitFailure::new(
                    1,
                    format!("with-session failed while waiting for child: {error}"),
                ));
            }
        }

        if time_to_renew <= super::common::POLL_INTERVAL {
            if let Err(error) = renew_lease(
                &mut connection,
                args.session_name.clone(),
                lease.token,
                ttl_millis,
            ) {
                let _ = child.kill();
                let _ = release_lease(&mut connection, args.session_name, lease.token);
                return Err(error);
            }
            time_to_renew = renew_interval;
        } else {
            time_to_renew = time_to_renew.saturating_sub(super::common::POLL_INTERVAL);
        }
        sleep_poll_interval();
    }
}

struct Lease {
    token: u64,
}

fn create_lease(
    connection: &mut rmux_client::Connection,
    session_name: SessionName,
    ttl_millis: u64,
) -> Result<Lease, ExitFailure> {
    match connection
        .create_session_lease(session_name, ttl_millis)
        .map_err(ExitFailure::from_client)?
    {
        Response::CreateSessionLease(response) => Ok(Lease {
            token: response.token,
        }),
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message("with-session", &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for with-session",
                other.command_name()
            ),
        )),
    }
}

fn renew_lease(
    connection: &mut rmux_client::Connection,
    session_name: SessionName,
    token: u64,
    ttl_millis: u64,
) -> Result<(), ExitFailure> {
    match connection
        .renew_session_lease(session_name, token, ttl_millis)
        .map_err(ExitFailure::from_client)?
    {
        Response::RenewSessionLease(response) if response.renewed => Ok(()),
        Response::RenewSessionLease(_) => Err(ExitFailure::new(1, "with-session lease was lost")),
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message("with-session", &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for with-session",
                other.command_name()
            ),
        )),
    }
}

fn release_lease(
    connection: &mut rmux_client::Connection,
    session_name: SessionName,
    token: u64,
) -> Result<(), ExitFailure> {
    match connection
        .release_session_lease(session_name, token)
        .map_err(ExitFailure::from_client)?
    {
        Response::ReleaseSessionLease(response) if response.released => Ok(()),
        Response::ReleaseSessionLease(_) => Err(ExitFailure::new(
            1,
            "with-session lease was already released or lost",
        )),
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message("with-session", &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for with-session release",
                other.command_name()
            ),
        )),
    }
}

fn kill_owned_session(
    connection: &mut rmux_client::Connection,
    session_name: SessionName,
) -> Result<(), ExitFailure> {
    match connection
        .kill_session(KillSessionRequest {
            target: session_name,
            kill_all_except_target: false,
            clear_alerts: false,
        })
        .map_err(ExitFailure::from_client)?
    {
        Response::KillSession(_) => Ok(()),
        Response::Error(error)
            if matches!(error.error, rmux_proto::RmuxError::SessionNotFound(_)) =>
        {
            Ok(())
        }
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message("with-session", &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for with-session cleanup",
                other.command_name()
            ),
        )),
    }
}

fn renew_interval(ttl: Duration) -> Duration {
    let third = ttl / 3;
    third.clamp(Duration::from_millis(100), Duration::from_secs(1))
}
