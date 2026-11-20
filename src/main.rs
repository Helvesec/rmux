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
mod cli_args;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod cli_response;
#[cfg(all(test, any(not(feature = "tiny-cli"), debug_assertions)))]
mod cli_test_inventory_tests;
mod cli_top_level;
mod client_terminal;
mod command_alias_snapshot;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod empty_server_lifecycle;
mod os_string;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod process_locale;
#[cfg(test)]
mod publication_dry_run_tests;
mod runtime_command_expansion;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
mod server_runtime;
#[cfg(all(test, unix))]
mod session_inventory_tests;
#[cfg(all(feature = "tiny-cli", any(not(debug_assertions), test)))]
mod tiny_main;
mod tmux_error_surface;
#[cfg(windows)]
mod windows_shell;
#[cfg(windows)]
mod windows_terminal;

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::env;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::ffi::OsString;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use std::io::{self, ErrorKind, Write};
#[cfg(all(test, any(not(feature = "tiny-cli"), debug_assertions)))]
use std::path::PathBuf;

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use rmux_bootstrap::hidden_daemon::{
    ConfigSelection as ServerConfigFileSelection, FlagSyntax,
    HiddenDaemonArgs as InternalDaemonArgs,
};
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use rmux_client::INTERNAL_DAEMON_FLAG;
#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
use rmux_server::{DaemonConfig, ServerDaemon};

#[cfg(all(feature = "tiny-cli", not(debug_assertions)))]
fn main() {
    tiny_main::main();
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn main() {
    #[cfg(unix)]
    if let Some(exit_code) =
        rmux_server::run_internal_fifo_reader_helper(std::env::args_os().skip(1))
    {
        std::process::exit(exit_code);
    }

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
#[cfg(test)]
fn parse_internal_socket_path<I>(args: I) -> Result<Option<PathBuf>, String>
where
    I: Iterator<Item = OsString>,
{
    parse_internal_daemon_args(args).map(|args| args.socket_path)
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn parse_internal_daemon_args<I>(args: I) -> Result<InternalDaemonArgs, String>
where
    I: Iterator<Item = OsString>,
{
    InternalDaemonArgs::parse(args, FlagSyntax::NativePrefix)
}

#[cfg(any(not(feature = "tiny-cli"), debug_assertions))]
fn run_hidden_daemon(args: InternalDaemonArgs) -> io::Result<()> {
    reject_unsupported_web_args(&args)?;
    #[cfg(unix)]
    let startup_owner = args
        .startup_owner_fd
        .map(|fd| {
            // SAFETY: hidden daemon argv transfers this inherited descriptor;
            // no runtime or other owner has been constructed yet.
            unsafe { rmux_os::daemon::take_startup_owner(fd) }
        })
        .transpose()?;

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
        #[cfg(unix)]
        let server = ServerDaemon::new(config)
            .bind_with_startup_owner(startup_owner)
            .await?;
        #[cfg(not(unix))]
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
mod hidden_daemon_cases;

#[cfg(all(test, any(not(feature = "tiny-cli"), debug_assertions)))]
mod tests {
    use super::{
        parse_internal_daemon_args, parse_internal_socket_path, try_main, write_exit_message_to,
    };
    use crate::cli::ExitMessageTermination;
    use rmux_bootstrap::hidden_daemon::ConfigSelection as ConfigFileSelection;
    use rmux_client::INTERNAL_DAEMON_FLAG;
    use std::ffi::OsString;
    use std::path::PathBuf;

    const EXPECTED_BINARY_NAME: &str = "rmux";

    #[test]
    fn hidden_daemon_contract() {
        crate::hidden_daemon_cases::check(
            |args| parse_internal_daemon_args(args.into_iter()),
            false,
        );
    }

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
