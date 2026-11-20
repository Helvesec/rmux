//! Internal hidden-daemon argument contract; callers retain entry guards and launch policy.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Internal config selection, owning decoded files or borrowing files for argv projection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConfigSelection<Files = Vec<PathBuf>> {
    /// Leave config loading disabled.
    #[default]
    Disabled,
    /// Ask the server to select its default files.
    Default,
    /// Explicit files in caller order.
    Files(Files),
}

/// Decoded operands after the caller has handled the initial internal marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HiddenDaemonArgs {
    pub socket_path: Option<PathBuf>,
    pub config_selection: ConfigSelection,
    pub config_quiet: bool,
    pub config_cwd: Option<PathBuf>,
    pub web_frontend: Option<String>,
    pub web_port: Option<u16>,
    pub startup_ready_fd: Option<i32>,
    pub startup_ready_event: Option<OsString>,
    #[cfg(unix)]
    pub startup_owner_fd: Option<i32>,
}

/// The existing entrypoints differ on non-Unicode tokens beginning with `--`.
#[derive(Clone, Copy)]
pub enum FlagSyntax {
    /// Full rmux recognizes the ASCII prefix even with a non-Unicode suffix.
    NativePrefix,
    /// Standalone rmux-daemon requires the entire flag token to be Unicode.
    UnicodePrefix,
}

impl FlagSyntax {
    fn is_flag(self, value: &OsStr) -> bool {
        match self {
            Self::NativePrefix => value.as_encoded_bytes().starts_with(b"--"),
            Self::UnicodePrefix => value.to_str().is_some_and(|value| value.starts_with("--")),
        }
    }
}

impl HiddenDaemonArgs {
    /// Decode the tail without changing marker, feature or native readiness policy.
    pub fn parse<I>(mut args: I, syntax: FlagSyntax) -> Result<Self, String>
    where
        I: Iterator<Item = OsString>,
    {
        let mut parsed = Self::default();
        if let Some(first) = args.next() {
            if syntax.is_flag(&first) {
                parsed.parse_flag(first, &mut args)?;
            } else {
                parsed.socket_path = Some(PathBuf::from(first));
            }
        }
        while let Some(argument) = args.next() {
            if !syntax.is_flag(&argument) {
                return Err("unexpected extra arguments for hidden daemon mode".to_owned());
            }
            parsed.parse_flag(argument, &mut args)?;
        }
        Ok(parsed)
    }

    fn parse_flag<I>(&mut self, argument: OsString, args: &mut I) -> Result<(), String>
    where
        I: Iterator<Item = OsString>,
    {
        match argument.to_str() {
            Some("--config-default") => {
                if !matches!(self.config_selection, ConfigSelection::Disabled) {
                    return Err("duplicate hidden daemon config selection".to_owned());
                }
                self.config_selection = ConfigSelection::Default;
            }
            Some("--config-file") => {
                let file = args
                    .next()
                    .ok_or_else(|| "--config-file requires a path".to_owned())?;
                match &mut self.config_selection {
                    ConfigSelection::Disabled => {
                        self.config_selection = ConfigSelection::Files(vec![PathBuf::from(file)]);
                    }
                    ConfigSelection::Files(files) => files.push(PathBuf::from(file)),
                    ConfigSelection::Default => {
                        return Err("--config-file conflicts with --config-default".to_owned());
                    }
                }
            }
            Some("--config-quiet") => self.config_quiet = true,
            Some("--config-cwd") => {
                let cwd = args
                    .next()
                    .ok_or_else(|| "--config-cwd requires a path".to_owned())?;
                self.config_cwd = Some(PathBuf::from(cwd));
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
                self.web_port = Some(port);
            }
            Some("--frontend-url" | "--web-frontend") => {
                let frontend = args
                    .next()
                    .ok_or_else(|| "--frontend-url requires a URL".to_owned())?;
                let frontend = frontend
                    .to_str()
                    .ok_or_else(|| "invalid UTF-8 in --frontend-url".to_owned())?;
                self.web_frontend = Some(frontend.to_owned());
            }
            Some("--startup-ready-fd") => {
                let fd = args
                    .next()
                    .ok_or_else(|| "--startup-ready-fd requires a file descriptor".to_owned())?;
                let fd = fd
                    .to_str()
                    .ok_or_else(|| "invalid UTF-8 in --startup-ready-fd".to_owned())?
                    .parse::<i32>()
                    .map_err(|_| {
                        "--startup-ready-fd requires an integer file descriptor".to_owned()
                    })?;
                if fd < 0 {
                    return Err(
                        "--startup-ready-fd requires a non-negative file descriptor".to_owned()
                    );
                }
                self.startup_ready_fd = Some(fd);
            }
            Some("--startup-ready-event") => {
                let event = args
                    .next()
                    .ok_or_else(|| "--startup-ready-event requires an event name".to_owned())?;
                self.startup_ready_event = Some(event);
            }
            #[cfg(unix)]
            Some("--startup-owner-fd") => {
                if self.startup_owner_fd.is_some() {
                    return Err("duplicate --startup-owner-fd".to_owned());
                }
                let fd = args
                    .next()
                    .ok_or_else(|| "--startup-owner-fd requires a file descriptor".to_owned())?;
                let fd = fd
                    .to_str()
                    .and_then(|value| value.parse::<i32>().ok())
                    .filter(|fd| *fd >= 3)
                    .ok_or_else(|| {
                        "--startup-owner-fd requires a file descriptor above stderr".to_owned()
                    })?;
                self.startup_owner_fd = Some(fd);
            }
            Some(other) => return Err(format!("unexpected hidden daemon argument '{other}'")),
            None => return Err("invalid UTF-8 in hidden daemon flag".to_owned()),
        }

        Ok(())
    }
}

/// Append only config/cwd/Web operands in the established producer order.
/// Marker, endpoint, readiness arguments and command execution stay with callers.
pub fn append_config_args(
    command: &mut Command,
    selection: ConfigSelection<&[PathBuf]>,
    quiet: bool,
    cwd: Option<&Path>,
    web_port: Option<u16>,
    web_frontend: Option<&str>,
) {
    match selection {
        ConfigSelection::Disabled => {}
        ConfigSelection::Default => {
            command.arg("--config-default");
        }
        ConfigSelection::Files(files) => {
            for file in files {
                command.arg("--config-file").arg(file);
            }
        }
    }
    if quiet {
        command.arg("--config-quiet");
    }
    if let Some(cwd) = cwd {
        command.arg("--config-cwd").arg(cwd);
    }
    if let Some(port) = web_port {
        command.arg("--web-port").arg(port.to_string());
    }
    if let Some(frontend) = web_frontend {
        command.arg("--frontend-url").arg(frontend);
    }
}
