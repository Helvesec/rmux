#![deny(missing_docs)]

//! RMUX application binary.
//!
//! The binary owns two entrypoints:
//! - the public CLI that speaks the detached `rmux-proto` request/response API
//!   through `rmux-client`, and
//! - the hidden internal daemon mode used by tmux-style start-server commands.
//!
//! Keeping the hidden daemon re-exec path here is why the root package depends
//! directly on `rmux-server` and Tokio without introducing an extra crate.

mod cli;
mod cli_args;
mod cli_response;
mod os_string;
mod process_locale;

use std::env;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;

use rmux_client::INTERNAL_DAEMON_FLAG;
use rmux_server::{ConfigFileSelection as ServerConfigFileSelection, DaemonConfig, ServerDaemon};
use tokio::runtime::Builder;

fn main() {
    match process_locale::initialize_process_locale()
        .map_err(|error| cli::ExitFailure::new(1, error))
        .and_then(|()| try_main(env::args_os()))
    {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if !error.message().is_empty() {
                let _ = write_exit_message(error.message(), error.use_stderr());
            }
            std::process::exit(error.exit_code());
        }
    }
}

fn write_exit_message(message: &str, stderr: bool) -> io::Result<()> {
    if stderr {
        match writeln!(io::stderr().lock(), "{message}") {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        match writeln!(io::stdout().lock(), "{message}") {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn try_main<I>(args: I) -> Result<i32, cli::ExitFailure>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();

    match args.get(1) {
        Some(argument) if argument == INTERNAL_DAEMON_FLAG => {
            let internal = parse_internal_daemon_args(args.into_iter().skip(2))
                .map_err(|error| cli::ExitFailure::new(1, error))?;
            run_hidden_daemon(internal)
                .map_err(|error| error.to_string())
                .map(|()| 0)
                .map_err(|error| cli::ExitFailure::new(1, error))
        }
        _ => cli::run(args),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalDaemonArgs {
    socket_path: Option<PathBuf>,
    config_selection: ServerConfigFileSelection,
    config_quiet: bool,
    config_cwd: Option<PathBuf>,
}

#[cfg(test)]
fn parse_internal_socket_path<I>(args: I) -> Result<Option<PathBuf>, String>
where
    I: Iterator<Item = OsString>,
{
    parse_internal_daemon_args(args).map(|args| args.socket_path)
}

fn parse_internal_daemon_args<I>(mut args: I) -> Result<InternalDaemonArgs, String>
where
    I: Iterator<Item = OsString>,
{
    let mut socket_path = None;
    let mut config_selection = ServerConfigFileSelection::Disabled;
    let mut config_quiet = false;
    let mut config_cwd = None;

    if let Some(first) = args.next() {
        if os_string::os_str_bytes(first.as_os_str()).starts_with(b"--") {
            parse_internal_flag(
                first,
                &mut args,
                &mut config_selection,
                &mut config_quiet,
                &mut config_cwd,
            )?;
        } else {
            socket_path = Some(PathBuf::from(first));
        }
    }

    while let Some(argument) = args.next() {
        if !os_string::os_str_bytes(argument.as_os_str()).starts_with(b"--") {
            return Err("unexpected extra arguments for hidden daemon mode".to_owned());
        }
        parse_internal_flag(
            argument,
            &mut args,
            &mut config_selection,
            &mut config_quiet,
            &mut config_cwd,
        )?;
    }

    Ok(InternalDaemonArgs {
        socket_path,
        config_selection,
        config_quiet,
        config_cwd,
    })
}

fn parse_internal_flag<I>(
    argument: OsString,
    args: &mut I,
    config_selection: &mut ServerConfigFileSelection,
    config_quiet: &mut bool,
    config_cwd: &mut Option<PathBuf>,
) -> Result<(), String>
where
    I: Iterator<Item = OsString>,
{
    match argument.to_str() {
        Some("--config-default") => {
            if !matches!(config_selection, ServerConfigFileSelection::Disabled) {
                return Err("duplicate hidden daemon config selection".to_owned());
            }
            *config_selection = ServerConfigFileSelection::Default;
        }
        Some("--config-file") => {
            let file = args
                .next()
                .ok_or_else(|| "--config-file requires a path".to_owned())?;
            match config_selection {
                ServerConfigFileSelection::Disabled => {
                    *config_selection = ServerConfigFileSelection::Files(vec![PathBuf::from(file)]);
                }
                ServerConfigFileSelection::Files(files) => files.push(PathBuf::from(file)),
                ServerConfigFileSelection::Default => {
                    return Err("--config-file conflicts with --config-default".to_owned());
                }
            }
        }
        Some("--config-quiet") => *config_quiet = true,
        Some("--config-cwd") => {
            let cwd = args
                .next()
                .ok_or_else(|| "--config-cwd requires a path".to_owned())?;
            *config_cwd = Some(PathBuf::from(cwd));
        }
        Some(other) => {
            return Err(format!("unexpected hidden daemon argument '{other}'"));
        }
        None => return Err("invalid UTF-8 in hidden daemon flag".to_owned()),
    }

    Ok(())
}

fn run_hidden_daemon(args: InternalDaemonArgs) -> io::Result<()> {
    let mut config = match args.socket_path {
        Some(socket_path) => DaemonConfig::new(socket_path),
        None => DaemonConfig::with_default_socket_path()?,
    };
    config = match args.config_selection {
        ServerConfigFileSelection::Disabled => config,
        ServerConfigFileSelection::Default => {
            config.with_default_config_load(args.config_quiet, args.config_cwd)
        }
        ServerConfigFileSelection::Files(files) => {
            config.with_config_files(files, args.config_quiet, args.config_cwd)
        }
    };
    #[cfg(unix)]
    let runtime = Builder::new_current_thread().enable_all().build()?;
    #[cfg(windows)]
    let runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let server = ServerDaemon::new(config).bind().await?;
        server.wait().await
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_internal_daemon_args, parse_internal_socket_path, try_main};
    use rmux_client::INTERNAL_DAEMON_FLAG;
    use rmux_server::ConfigFileSelection;
    use std::ffi::OsString;
    use std::path::PathBuf;

    const EXPECTED_BINARY_NAME: &str = "rmux";

    #[test]
    fn binary_contract_is_rmux() {
        let compiled_binary_name = option_env!("CARGO_BIN_NAME").unwrap_or(env!("CARGO_PKG_NAME"));
        assert_eq!(compiled_binary_name, EXPECTED_BINARY_NAME);
    }

    #[test]
    fn hidden_daemon_parser_accepts_an_optional_socket_path() {
        let socket_path =
            parse_internal_socket_path([OsString::from("/tmp/rmux-hidden.sock")].into_iter())
                .expect("hidden socket path");

        assert_eq!(socket_path, Some(PathBuf::from("/tmp/rmux-hidden.sock")));
    }

    #[test]
    fn hidden_daemon_parser_rejects_unexpected_arguments() {
        let error = parse_internal_socket_path(
            [
                OsString::from("/tmp/rmux-hidden.sock"),
                OsString::from("/tmp/extra.sock"),
            ]
            .into_iter(),
        )
        .expect_err("unexpected hidden daemon argument should fail");

        assert!(error.contains("unexpected extra arguments"));
    }

    #[test]
    fn hidden_daemon_parser_defaults_to_the_spec_socket_when_unset() {
        let socket_path =
            parse_internal_socket_path(std::iter::empty()).expect("default socket path selection");

        assert_eq!(socket_path, None);
    }

    #[test]
    fn hidden_daemon_parser_accepts_config_forwarding_flags() {
        let args = parse_internal_daemon_args(
            [
                OsString::from("/tmp/rmux-hidden.sock"),
                OsString::from("--config-file"),
                OsString::from("one.conf"),
                OsString::from("--config-file"),
                OsString::from("two.conf"),
                OsString::from("--config-quiet"),
                OsString::from("--config-cwd"),
                OsString::from("/tmp/cwd"),
            ]
            .into_iter(),
        )
        .expect("hidden config args");

        assert_eq!(
            args.socket_path,
            Some(PathBuf::from("/tmp/rmux-hidden.sock"))
        );
        assert!(args.config_quiet);
        assert_eq!(args.config_cwd, Some(PathBuf::from("/tmp/cwd")));
        assert_eq!(
            args.config_selection,
            ConfigFileSelection::Files(vec![PathBuf::from("one.conf"), PathBuf::from("two.conf")])
        );
    }

    #[test]
    fn try_main_reports_clap_failures_for_invalid_public_invocations() {
        let result = try_main([
            OsString::from("rmux"),
            OsString::from("detach-client"),
            OsString::from("unexpected"),
        ]);

        let error = result.expect_err("unexpected detach arguments should fail");
        assert_eq!(error.exit_code(), 1);
        assert!(error.message().contains("unexpected"));
    }

    #[test]
    fn try_main_rejects_hidden_daemon_extra_arguments() {
        let error = try_main([
            OsString::from("rmux"),
            OsString::from(INTERNAL_DAEMON_FLAG),
            OsString::from("/tmp/rmux-hidden.sock"),
            OsString::from("/tmp/extra.sock"),
        ])
        .expect_err("unexpected hidden daemon arguments should fail");

        assert!(error.message().contains("unexpected extra arguments"));
    }
}
