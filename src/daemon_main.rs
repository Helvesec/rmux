#![deny(missing_docs)]

//! Minimal hidden RMUX daemon binary.
//!
//! The public `rmux` CLI still owns tmux-compatible parsing and presentation.
//! This binary owns only the internal daemon re-exec contract so long-lived
//! server processes do not map the full CLI/help/completion surface.

mod server_runtime;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};

use rmux_bootstrap::hidden_daemon::{
    ConfigSelection as ConfigFileSelection, FlagSyntax, HiddenDaemonArgs as DaemonArgs,
};
use rmux_server::{DaemonConfig, ServerDaemon};

const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";

fn main() {
    #[cfg(unix)]
    if let Some(exit_code) =
        rmux_server::run_internal_fifo_reader_helper(std::env::args_os().skip(1))
    {
        std::process::exit(exit_code);
    }

    match try_main(env::args_os().skip(1)) {
        Ok(()) => {}
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            std::process::exit(1);
        }
    }
}

fn try_main<I>(args: I) -> io::Result<()>
where
    I: IntoIterator<Item = OsString>,
{
    let args = parse_daemon_args(args.into_iter())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    run_hidden_daemon(args)
}

fn parse_daemon_args<I>(mut args: I) -> Result<DaemonArgs, String>
where
    I: Iterator<Item = OsString>,
{
    if args.next().as_deref() != Some(std::ffi::OsStr::new(INTERNAL_DAEMON_FLAG)) {
        return Err("rmux-daemon is internal; launch it through `rmux`, not directly".to_owned());
    }

    DaemonArgs::parse(args, FlagSyntax::UnicodePrefix)
}

fn run_hidden_daemon(args: DaemonArgs) -> io::Result<()> {
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
        ConfigFileSelection::Disabled => config,
        ConfigFileSelection::Default => {
            config.with_default_config_load(args.config_quiet, args.config_cwd)
        }
        ConfigFileSelection::Files(files) => {
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

fn reject_unsupported_web_args(args: &DaemonArgs) -> io::Result<()> {
    #[cfg(not(feature = "web"))]
    if args.web_port.is_some() || args.web_frontend.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rmux-daemon was built without web-share support; launch web-share through `rmux`",
        ));
    }

    #[cfg(feature = "web")]
    {
        let _ = args;
    }

    Ok(())
}

#[cfg(test)]
mod hidden_daemon_cases;

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "web"))]
    use super::reject_unsupported_web_args;
    use super::{parse_daemon_args, ConfigFileSelection, INTERNAL_DAEMON_FLAG};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn hidden_daemon_contract() {
        crate::hidden_daemon_cases::check(
            |args| {
                parse_daemon_args(std::iter::once(OsString::from(INTERNAL_DAEMON_FLAG)).chain(args))
            },
            true,
        );
    }

    #[test]
    fn daemon_parser_requires_internal_flag() {
        let error = parse_daemon_args(std::iter::empty()).expect_err("missing flag rejects");

        assert!(error.contains("internal"));
    }

    #[test]
    fn daemon_parser_accepts_socket_and_config_flags() {
        let args = parse_daemon_args(
            [
                OsString::from(INTERNAL_DAEMON_FLAG),
                OsString::from("/tmp/rmux.sock"),
                OsString::from("--config-file"),
                OsString::from("rmux.conf"),
                OsString::from("--config-quiet"),
                OsString::from("--config-cwd"),
                OsString::from("/tmp"),
                OsString::from("--web-port"),
                OsString::from("4321"),
                OsString::from("--frontend-url"),
                OsString::from("http://127.0.0.1:4325"),
            ]
            .into_iter(),
        )
        .expect("valid daemon args");

        assert_eq!(args.socket_path, Some(PathBuf::from("/tmp/rmux.sock")));
        assert_eq!(
            args.config_selection,
            ConfigFileSelection::Files(vec![PathBuf::from("rmux.conf")])
        );
        assert!(args.config_quiet);
        assert_eq!(args.config_cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(args.web_port, Some(4321));
        assert_eq!(args.web_frontend.as_deref(), Some("http://127.0.0.1:4325"));
    }

    #[cfg(not(feature = "web"))]
    #[test]
    fn daemon_without_web_rejects_web_listener_flags() {
        let args = parse_daemon_args(
            [
                OsString::from(INTERNAL_DAEMON_FLAG),
                OsString::from("/tmp/rmux.sock"),
                OsString::from("--web-port"),
                OsString::from("4321"),
            ]
            .into_iter(),
        )
        .expect("parser keeps the internal contract shape");

        let error = reject_unsupported_web_args(&args).expect_err("web flags reject");

        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("without web-share support"));
    }
}
