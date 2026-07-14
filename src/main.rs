#![deny(missing_docs)]

//! RMUX application binary.
//!
//! The binary owns two entrypoints:
//! - the public CLI that speaks the detached `rmux-proto` request/response API
//!   through `rmux-client`, and
//! - the hidden internal daemon mode used by tmux-style start-server commands.
//!
//! Optimized package builds can alternatively enable `tiny-cli`, making this
//! public binary a small dispatcher for hot detached commands while complex
//! commands exec the private full `rmux` helper installed under libexec.

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod cli;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod cli_args;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod cli_response;
mod client_terminal;
mod command_alias_snapshot;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod os_string;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod process_locale;
mod runtime_command_expansion;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod server_runtime;
#[cfg(all(feature = "tiny-cli", any(not(debug_assertions), test)))]
mod tiny_main;
mod tmux_error_surface;
#[cfg(windows)]
mod windows_terminal;

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::env;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::ffi::OsString;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::io::{self, ErrorKind, Write};
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::path::PathBuf;

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use rmux_client::INTERNAL_DAEMON_FLAG;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use rmux_server::{ConfigFileSelection as ServerConfigFileSelection, DaemonConfig, ServerDaemon};

#[cfg(all(feature = "tiny-cli", not(debug_assertions)))]
fn main() {
    tiny_main::main();
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn main() {
    match process_locale::initialize_process_locale()
        .map_err(|error| cli::ExitFailure::new(1, error))
        .and_then(|()| try_main(env::args_os()))
    {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if !error.message().is_empty() {
                let _ = write_exit_message(
                    error.message(),
                    error.use_stderr(),
                    error.message_termination(),
                );
            }
            std::process::exit(error.exit_code());
        }
    }
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn write_exit_message(
    message: &str,
    stderr: bool,
    termination: cli::ExitMessageTermination,
) -> io::Result<()> {
    if stderr {
        write_exit_message_to(&mut io::stderr().lock(), message, termination)
    } else {
        write_exit_message_to(&mut io::stdout().lock(), message, termination)
    }
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn write_exit_message_to(
    output: &mut impl Write,
    message: &str,
    termination: cli::ExitMessageTermination,
) -> io::Result<()> {
    let result = match termination {
        cli::ExitMessageTermination::Line => writeln!(output, "{message}"),
        cli::ExitMessageTermination::Exact => output
            .write_all(message.as_bytes())
            .and_then(|()| output.flush()),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
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

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalDaemonArgs {
    socket_path: Option<PathBuf>,
    config_selection: ServerConfigFileSelection,
    config_quiet: bool,
    config_cwd: Option<PathBuf>,
    web_frontend: Option<String>,
    web_port: Option<u16>,
    startup_ready_fd: Option<i32>,
    startup_ready_event: Option<OsString>,
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
#[cfg(test)]
fn parse_internal_socket_path<I>(args: I) -> Result<Option<PathBuf>, String>
where
    I: Iterator<Item = OsString>,
{
    parse_internal_daemon_args(args).map(|args| args.socket_path)
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn parse_internal_daemon_args<I>(mut args: I) -> Result<InternalDaemonArgs, String>
where
    I: Iterator<Item = OsString>,
{
    let mut socket_path = None;
    let mut config_selection = ServerConfigFileSelection::Disabled;
    let mut config_quiet = false;
    let mut config_cwd = None;
    let mut web_frontend = None;
    let mut web_port = None;
    let mut startup_ready_fd = None;
    let mut startup_ready_event = None;

    if let Some(first) = args.next() {
        if os_string::os_str_bytes(first.as_os_str()).starts_with(b"--") {
            parse_internal_flag(
                first,
                &mut args,
                &mut config_selection,
                &mut config_quiet,
                &mut config_cwd,
                &mut web_frontend,
                &mut web_port,
                &mut startup_ready_fd,
                &mut startup_ready_event,
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
            &mut web_frontend,
            &mut web_port,
            &mut startup_ready_fd,
            &mut startup_ready_event,
        )?;
    }

    Ok(InternalDaemonArgs {
        socket_path,
        config_selection,
        config_quiet,
        config_cwd,
        web_frontend,
        web_port,
        startup_ready_fd,
        startup_ready_event,
    })
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
#[allow(clippy::too_many_arguments)]
fn parse_internal_flag<I>(
    argument: OsString,
    args: &mut I,
    config_selection: &mut ServerConfigFileSelection,
    config_quiet: &mut bool,
    config_cwd: &mut Option<PathBuf>,
    web_frontend: &mut Option<String>,
    web_port: &mut Option<u16>,
    startup_ready_fd: &mut Option<i32>,
    startup_ready_event: &mut Option<OsString>,
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
        Some("--web-port") => {
            let port = args
                .next()
                .ok_or_else(|| "--web-port requires a port".to_owned())?;
            let port = port
                .to_str()
                .ok_or_else(|| "invalid UTF-8 in --web-port".to_owned())?
                .parse::<u16>()
                .map_err(|_| "--web-port requires an integer port".to_owned())?;
            if port == 0 {
                return Err("--web-port must be between 1 and 65535".to_owned());
            }
            *web_port = Some(port);
        }
        Some("--frontend-url" | "--web-frontend") => {
            let frontend = args
                .next()
                .ok_or_else(|| "--frontend-url requires a URL".to_owned())?;
            let frontend = frontend
                .to_str()
                .ok_or_else(|| "invalid UTF-8 in --frontend-url".to_owned())?;
            *web_frontend = Some(frontend.to_owned());
        }
        Some("--startup-ready-fd") => {
            let fd = args
                .next()
                .ok_or_else(|| "--startup-ready-fd requires a file descriptor".to_owned())?;
            let fd = fd
                .to_str()
                .ok_or_else(|| "invalid UTF-8 in --startup-ready-fd".to_owned())?
                .parse::<i32>()
                .map_err(|_| "--startup-ready-fd requires an integer file descriptor".to_owned())?;
            if fd < 0 {
                return Err("--startup-ready-fd requires a non-negative file descriptor".to_owned());
            }
            *startup_ready_fd = Some(fd);
        }
        Some("--startup-ready-event") => {
            let event = args
                .next()
                .ok_or_else(|| "--startup-ready-event requires an event name".to_owned())?;
            *startup_ready_event = Some(event);
        }
        Some(other) => {
            return Err(format!("unexpected hidden daemon argument '{other}'"));
        }
        None => return Err("invalid UTF-8 in hidden daemon flag".to_owned()),
    }

    Ok(())
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn run_hidden_daemon(args: InternalDaemonArgs) -> io::Result<()> {
    reject_unsupported_web_args(&args)?;

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
    if let Some(port) = args.web_port {
        config = config.with_web_port(port);
    }
    if let Some(frontend) = args.web_frontend {
        config = config.with_web_frontend(frontend);
    }
    #[cfg(target_os = "linux")]
    if let Some(ready_fd) = args.startup_ready_fd {
        config = config.with_startup_ready_fd(ready_fd);
    }
    #[cfg(windows)]
    if let Some(ready_event) = args.startup_ready_event {
        config = config.with_startup_ready_event(ready_event);
    }
    rmux_os::memory::configure_daemon_allocator();
    let runtime = server_runtime::build_daemon_runtime()?;

    runtime.block_on(async move {
        let server = ServerDaemon::new(config).bind().await?;
        server.wait().await
    })
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn reject_unsupported_web_args(args: &InternalDaemonArgs) -> io::Result<()> {
    #[cfg(not(feature = "web"))]
    if args.web_port.is_some() || args.web_frontend.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rmux was built without web-share support",
        ));
    }

    #[cfg(feature = "web")]
    {
        let _ = args;
    }

    Ok(())
}

#[cfg(all(test, any(not(feature = "tiny-cli"), debug_assertions)))]
mod tests {
    use super::{
        parse_internal_daemon_args, parse_internal_socket_path, try_main, write_exit_message_to,
    };
    use crate::cli::ExitMessageTermination;
    use rmux_client::INTERNAL_DAEMON_FLAG;
    use rmux_server::ConfigFileSelection;
    use std::ffi::OsString;
    use std::path::PathBuf;

    const EXPECTED_BINARY_NAME: &str = "rmux";

    #[test]
    fn exit_message_writer_distinguishes_lines_from_exact_protocol_bytes() {
        let mut line = Vec::new();
        write_exit_message_to(&mut line, "diagnostic", ExitMessageTermination::Line)
            .expect("line-terminated exit message");
        assert_eq!(line, b"diagnostic\n");

        let mut exact = Vec::new();
        write_exit_message_to(
            &mut exact,
            "\u{1b}P1000pdiagnostic\n%exit\n\u{1b}\\",
            ExitMessageTermination::Exact,
        )
        .expect("exact exit message");
        assert_eq!(exact, b"\x1bP1000pdiagnostic\n%exit\n\x1b\\");
    }

    #[test]
    fn compiled_binary_name_is_rmux() {
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
    fn try_main_reports_absent_server_before_command_parse_failures() {
        #[cfg(unix)]
        let socket_args = [
            OsString::from("-S"),
            OsString::from(format!(
                "/tmp/rmux-main-missing-{}-parse.sock",
                std::process::id()
            )),
        ];
        #[cfg(windows)]
        let socket_args = [
            OsString::from("-L"),
            OsString::from(format!("main-missing-{}-parse", std::process::id())),
        ];

        let result = try_main([
            OsString::from("rmux"),
            socket_args[0].clone(),
            socket_args[1].clone(),
            OsString::from("new-session"),
            OsString::from("-s"),
        ]);

        let error = result.expect_err("missing new-session value should fail");
        assert_eq!(error.exit_code(), 1);
        assert!(
            error.message().contains("error connecting to"),
            "{}",
            error.message()
        );
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
