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
use std::path::PathBuf;

use rmux_server::{ConfigFileSelection, DaemonConfig, ServerDaemon};

const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";

fn main() {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonArgs {
    socket_path: Option<PathBuf>,
    config_selection: ConfigFileSelection,
    config_quiet: bool,
    config_cwd: Option<PathBuf>,
    web_frontend: Option<String>,
    web_port: Option<u16>,
    startup_ready_fd: Option<i32>,
    startup_ready_event: Option<OsString>,
}

fn parse_daemon_args<I>(mut args: I) -> Result<DaemonArgs, String>
where
    I: Iterator<Item = OsString>,
{
    if args.next().as_deref() != Some(std::ffi::OsStr::new(INTERNAL_DAEMON_FLAG)) {
        return Err("rmux-daemon is internal; launch it through `rmux`, not directly".to_owned());
    }

    let mut socket_path = None;
    let mut config_selection = ConfigFileSelection::Disabled;
    let mut config_quiet = false;
    let mut config_cwd = None;
    let mut web_frontend = None;
    let mut web_port = None;
    let mut startup_ready_fd = None;
    let mut startup_ready_event = None;

    if let Some(first) = args.next() {
        if is_internal_flag_token(first.as_os_str()) {
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
        if !is_internal_flag_token(argument.as_os_str()) {
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

    Ok(DaemonArgs {
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

#[allow(clippy::too_many_arguments)]
fn parse_internal_flag<I>(
    argument: OsString,
    args: &mut I,
    config_selection: &mut ConfigFileSelection,
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
            if !matches!(config_selection, ConfigFileSelection::Disabled) {
                return Err("duplicate hidden daemon config selection".to_owned());
            }
            *config_selection = ConfigFileSelection::Default;
        }
        Some("--config-file") => {
            let file = args
                .next()
                .ok_or_else(|| "--config-file requires a path".to_owned())?;
            match config_selection {
                ConfigFileSelection::Disabled => {
                    *config_selection = ConfigFileSelection::Files(vec![PathBuf::from(file)]);
                }
                ConfigFileSelection::Files(files) => files.push(PathBuf::from(file)),
                ConfigFileSelection::Default => {
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
        Some(other) => return Err(format!("unexpected hidden daemon argument '{other}'")),
        None => return Err("invalid UTF-8 in hidden daemon flag".to_owned()),
    }

    Ok(())
}

fn run_hidden_daemon(args: DaemonArgs) -> io::Result<()> {
    reject_unsupported_web_args(&args)?;

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

fn is_internal_flag_token(value: &std::ffi::OsStr) -> bool {
    value.to_str().is_some_and(|value| value.starts_with("--"))
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "web"))]
    use super::reject_unsupported_web_args;
    use super::{parse_daemon_args, ConfigFileSelection, INTERNAL_DAEMON_FLAG};
    use std::ffi::OsString;
    use std::path::PathBuf;

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
