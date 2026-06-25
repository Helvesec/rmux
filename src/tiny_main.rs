//! Tiny public CLI dispatcher for release package builds.
#![cfg_attr(test, allow(dead_code))]

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(not(windows))]
use rmux_client::attach_terminal_with_initial_bytes;
#[cfg(unix)]
use rmux_client::attach_terminal_with_initial_bytes_and_resize_geometry;
#[cfg(windows)]
use rmux_client::attach_terminal_with_initial_bytes_and_windows_console_key;
use rmux_client::{
    connect, ensure_server_running_with_config, resolve_socket_path,
    resolve_tmux_compatible_socket_path, AttachTransition, AutoStartConfig, ClientError,
    Connection, StartServerError,
};
#[cfg(not(windows))]
use rmux_client::{connect_or_absent, ConnectResult};
use rmux_core::formats::{DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_WINDOW_FORMAT};
use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, DisplayMessageRequest, KillSessionRequest,
    NewSessionExtRequest,
};
#[cfg(windows)]
use rmux_proto::CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY;
use rmux_proto::{
    CapturePaneTargetActionRequest, JoinPaneRequest, ListSessionsRequest, Request,
    ResizePaneTargetActionRequest, Response, RmuxError, SetOptionMode, SourceFileResponse,
    SplitDirection, SplitWindowTargetActionRequest, CAPABILITY_ATTACH_RENDER,
    CAPABILITY_ATTACH_RESIZE_GEOMETRY,
};

mod helper;
mod output;
mod parse;
mod trace;

use crate::tmux_error_surface::{source_file_error_uses_stdout, tmux_cli_error_message};
#[cfg(not(windows))]
use helper::daemon_helper_path;
use helper::exec_full_helper;
use output::{client_error, write_response_output_or_error, write_stdout};
use parse::{
    parse_attach_session, parse_capture_pane, parse_display_message, parse_has_session,
    parse_join_pane, parse_kill_pane, parse_kill_session, parse_list_panes, parse_list_windows,
    parse_new_session, parse_new_window, parse_rename_window, parse_resize_pane,
    parse_select_window, parse_send_keys, parse_set_option, parse_show_options, parse_source_file,
    parse_split_window, TinyDisplayMessage, TinyHasSession, TinyJoinPane, TinyKillPane,
    TinyKillSession, TinyListPanes, TinyListWindows, TinyNewWindow, TinyRenameWindow,
    TinySelectWindow, TinySendKeys, TinySetOption, TinyShowOptions, TinySourceFile,
};
use trace::{trace_direct, trace_fallback};

#[cfg(unix)]
const KILL_SERVER_SOCKET_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const KILL_SERVER_SOCKET_CLEANUP_MIN_POLL: Duration = Duration::from_millis(1);
#[cfg(unix)]
const KILL_SERVER_SOCKET_CLEANUP_MAX_POLL: Duration = Duration::from_millis(10);

pub(crate) fn main() {
    let args: Vec<OsString> = env::args_os().collect();
    let result = match TinyInvocation::parse(&args) {
        TinyInvocation::Version => {
            let version = if invoked_as_tmux(&args) {
                "tmux 3.4".to_owned()
            } else {
                format!("rmux {}", env!("CARGO_PKG_VERSION"))
            };
            write_stdout(format!("{version}\n").as_bytes())
                .map(|()| 0)
                .map_err(|error| error.to_string())
        }
        TinyInvocation::Direct(command) => {
            trace_direct(command.name());
            command.run(&args)
        }
        TinyInvocation::Fallback => {
            trace_fallback("unsupported invocation");
            exec_full_helper(&args)
        }
    };

    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if !error.is_empty() {
                let _ = writeln!(io::stderr().lock(), "{error}");
            }
            std::process::exit(1);
        }
    }
}

enum TinyInvocation {
    Version,
    Direct(Box<TinyCommand>),
    Fallback,
}

enum TinyCommand {
    StartServer {
        socket_path: PathBuf,
    },
    ListSessions {
        socket_path: PathBuf,
    },
    HasSession {
        socket_path: PathBuf,
        request: TinyHasSession,
    },
    ListWindows {
        socket_path: PathBuf,
        request: TinyListWindows,
    },
    ListPanes {
        socket_path: PathBuf,
        request: TinyListPanes,
    },
    KillServer {
        socket_path: PathBuf,
    },
    CapturePane {
        socket_path: PathBuf,
        request: CapturePaneTargetActionRequest,
    },
    AttachSession {
        socket_path: PathBuf,
        request: AttachSessionExt2Request,
    },
    SplitWindow {
        socket_path: PathBuf,
        request: SplitWindowTargetActionRequest,
    },
    NewWindow {
        socket_path: PathBuf,
        request: TinyNewWindow,
    },
    NewSession {
        socket_path: PathBuf,
        request: NewSessionExtRequest,
    },
    KillSession {
        socket_path: PathBuf,
        request: TinyKillSession,
    },
    ShowOptions {
        socket_path: PathBuf,
        request: TinyShowOptions,
        command_name: &'static str,
    },
    RenameWindow {
        socket_path: PathBuf,
        request: TinyRenameWindow,
    },
    SelectWindow {
        socket_path: PathBuf,
        request: TinySelectWindow,
    },
    KillPane {
        socket_path: PathBuf,
        request: TinyKillPane,
    },
    JoinPane {
        socket_path: PathBuf,
        request: TinyJoinPane,
    },
    SetOption {
        socket_path: PathBuf,
        request: TinySetOption,
        command_name: &'static str,
    },
    ResizePane {
        socket_path: PathBuf,
        request: ResizePaneTargetActionRequest,
    },
    DisplayMessage {
        socket_path: PathBuf,
        request: TinyDisplayMessage,
    },
    SendKeys {
        socket_path: PathBuf,
        request: TinySendKeys,
    },
    SourceFile {
        socket_path: PathBuf,
        request: TinySourceFile,
    },
}

impl TinyInvocation {
    fn parse(args: &[OsString]) -> Self {
        if env_var_nonempty("RMUX_DISABLE_TINY_CLI") {
            return Self::Fallback;
        }

        let target_actions_disabled = env_var_nonempty("RMUX_DISABLE_CLI_TARGET_ACTIONS");
        let tmux_compatible = invoked_as_tmux(args);
        let mut socket_name: Option<OsString> = None;
        let mut socket_path: Option<PathBuf> = None;
        let mut index = 1;
        while index < args.len() {
            let Some(arg) = args[index].to_str() else {
                return Self::Fallback;
            };
            match arg {
                "-V" if index + 1 == args.len() => return Self::Version,
                "-L" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Self::Fallback;
                    };
                    socket_name = Some(value.clone());
                }
                "-S" => {
                    index += 1;
                    let Some(value) = args.get(index) else {
                        return Self::Fallback;
                    };
                    socket_path = Some(PathBuf::from(value));
                }
                "start-server" if index + 1 == args.len() => {
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::StartServer { socket_path }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "list-sessions" | "ls" if index + 1 == args.len() => {
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::ListSessions { socket_path }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "has-session" | "has" => {
                    let Some(request) = parse_has_session(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::HasSession {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "list-windows" | "lsw" => {
                    let Some(request) = parse_list_windows(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::ListWindows {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "list-panes" | "lsp" => {
                    let Some(request) = parse_list_panes(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::ListPanes {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "kill-server" if index + 1 == args.len() => {
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::KillServer { socket_path }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "capture-pane" | "capturep" => {
                    if target_actions_disabled {
                        return Self::Fallback;
                    }
                    let Some(request) = parse_capture_pane(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::CapturePane {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "attach-session" | "attach" | "a" => {
                    let Some(request) = parse_attach_session(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::AttachSession {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "split-window" | "splitw" | "split" => {
                    if target_actions_disabled {
                        return Self::Fallback;
                    }
                    let Some(request) = parse_split_window(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::SplitWindow {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "new-window" | "neww" => {
                    let Some(request) = parse_new_window(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::NewWindow {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "new-session" | "new" => {
                    let Some(request) = parse_new_session(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::NewSession {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "kill-session" => {
                    let Some(request) = parse_kill_session(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::KillSession {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "show-options" => {
                    let Some(request) = parse_show_options(&args[index + 1..], false) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::ShowOptions {
                            socket_path,
                            request,
                            command_name: "show-options",
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "show-window-options" => {
                    let Some(request) = parse_show_options(&args[index + 1..], true) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::ShowOptions {
                            socket_path,
                            request,
                            command_name: "show-window-options",
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "rename-window" | "renamew" => {
                    let Some(request) = parse_rename_window(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::RenameWindow {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "select-window" | "selectw" => {
                    let Some(request) = parse_select_window(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::SelectWindow {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "kill-pane" | "killp" => {
                    let Some(request) = parse_kill_pane(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::KillPane {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "join-pane" | "joinp" => {
                    let Some(request) = parse_join_pane(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::JoinPane {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "set-option" | "set" => {
                    let Some(request) = parse_set_option(&args[index + 1..], false) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::SetOption {
                            socket_path,
                            request,
                            command_name: "set-option",
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "set-window-option" | "setw" => {
                    let Some(request) = parse_set_option(&args[index + 1..], true) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::SetOption {
                            socket_path,
                            request,
                            command_name: "set-window-option",
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "resize-pane" | "resizep" => {
                    if target_actions_disabled {
                        return Self::Fallback;
                    }
                    let Some(request) = parse_resize_pane(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::ResizePane {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "display-message" | "display" => {
                    let Some(request) = parse_display_message(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::DisplayMessage {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "send-keys" | "send" => {
                    let Some(request) = parse_send_keys(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::SendKeys {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                "source-file" | "source" => {
                    let Some(request) = parse_source_file(&args[index + 1..]) else {
                        return Self::Fallback;
                    };
                    return tiny_socket(
                        tmux_compatible,
                        socket_name.as_deref(),
                        socket_path.as_deref(),
                    )
                    .map(|socket_path| {
                        Self::Direct(Box::new(TinyCommand::SourceFile {
                            socket_path,
                            request,
                        }))
                    })
                    .unwrap_or(Self::Fallback);
                }
                _ => return Self::Fallback,
            }
            index += 1;
        }

        Self::Fallback
    }
}

impl TinyCommand {
    const fn name(&self) -> &'static str {
        match self {
            Self::StartServer { .. } => "start-server",
            Self::ListSessions { .. } => "list-sessions",
            Self::HasSession { .. } => "has-session",
            Self::ListWindows { .. } => "list-windows",
            Self::ListPanes { .. } => "list-panes",
            Self::KillServer { .. } => "kill-server",
            Self::CapturePane { .. } => "capture-pane",
            Self::AttachSession { .. } => "attach-session",
            Self::SplitWindow { .. } => "split-window",
            Self::NewWindow { .. } => "new-window",
            Self::NewSession { .. } => "new-session",
            Self::KillSession { .. } => "kill-session",
            Self::ShowOptions { command_name, .. } => command_name,
            Self::RenameWindow { .. } => "rename-window",
            Self::SelectWindow { .. } => "select-window",
            Self::KillPane { .. } => "kill-pane",
            Self::JoinPane { .. } => "join-pane",
            Self::SetOption { command_name, .. } => command_name,
            Self::ResizePane { .. } => "resize-pane",
            Self::DisplayMessage { .. } => "display-message",
            Self::SendKeys { .. } => "send-keys",
            Self::SourceFile { .. } => "source-file",
        }
    }

    fn run(self, original_args: &[OsString]) -> Result<i32, String> {
        match self {
            Self::StartServer { socket_path } => run_start_server(&socket_path),
            Self::ListSessions { socket_path } => run_list_sessions(&socket_path),
            Self::HasSession {
                socket_path,
                request,
            } => run_has_session(original_args, &socket_path, request),
            Self::ListWindows {
                socket_path,
                request,
            } => run_list_windows(original_args, &socket_path, request),
            Self::ListPanes {
                socket_path,
                request,
            } => run_list_panes(original_args, &socket_path, request),
            Self::KillServer { socket_path } => run_kill_server(&socket_path),
            Self::CapturePane {
                socket_path,
                request,
            } => run_target_action(
                original_args,
                &socket_path,
                "capture-pane",
                RetryPolicy::DecodeOrEof,
                |connection| connection.capture_pane_target_action(request),
            ),
            Self::AttachSession {
                socket_path,
                request,
            } => run_attach_session(original_args, &socket_path, request),
            Self::SplitWindow {
                socket_path,
                request,
            } => run_target_action(
                original_args,
                &socket_path,
                "split-window",
                RetryPolicy::DecodeOnly,
                |connection| connection.split_window_target_action(request),
            ),
            Self::NewWindow {
                socket_path,
                request,
            } => run_new_window(&socket_path, request),
            Self::NewSession {
                socket_path,
                request,
            } => run_new_session(original_args, &socket_path, request),
            Self::KillSession {
                socket_path,
                request,
            } => run_kill_session(original_args, &socket_path, request),
            Self::ShowOptions {
                socket_path,
                request,
                command_name,
            } => run_show_options(&socket_path, request, command_name),
            Self::RenameWindow {
                socket_path,
                request,
            } => run_rename_window(&socket_path, request),
            Self::SelectWindow {
                socket_path,
                request,
            } => run_select_window(&socket_path, request),
            Self::KillPane {
                socket_path,
                request,
            } => run_kill_pane(&socket_path, request),
            Self::JoinPane {
                socket_path,
                request,
            } => run_join_pane(&socket_path, request),
            Self::SetOption {
                socket_path,
                request,
                command_name,
            } => run_set_option(&socket_path, request, command_name),
            Self::ResizePane {
                socket_path,
                request,
            } => run_target_action(
                original_args,
                &socket_path,
                "resize-pane",
                RetryPolicy::DecodeOnly,
                |connection| connection.resize_pane_target_action(request),
            ),
            Self::DisplayMessage {
                socket_path,
                request,
            } => run_display_message(&socket_path, request),
            Self::SendKeys {
                socket_path,
                request,
            } => run_send_keys(&socket_path, request),
            Self::SourceFile {
                socket_path,
                request,
            } => run_source_file(&socket_path, request),
        }
    }
}

fn tiny_socket(
    tmux_compatible: bool,
    socket_name: Option<&OsStr>,
    socket_path: Option<&Path>,
) -> Option<PathBuf> {
    if tmux_compatible {
        resolve_tmux_compatible_socket_path(socket_name, socket_path).ok()
    } else {
        resolve_socket_path(socket_name, socket_path).ok()
    }
}

fn invoked_as_tmux(args: &[OsString]) -> bool {
    invoked_as_tmux_from(
        args,
        env::var_os("RMUX_INTERNAL_INVOKED_AS_TMUX").as_deref(),
    )
}

fn invoked_as_tmux_from(args: &[OsString], internal_override: Option<&OsStr>) -> bool {
    if internal_override.is_some_and(|value| !value.is_empty()) {
        return true;
    }
    args.first()
        .and_then(|arg| Path::new(arg).file_stem())
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem.eq_ignore_ascii_case("tmux"))
}

fn env_var_nonempty(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn run_start_server(socket_path: &Path) -> Result<i32, String> {
    let mut connection = Connection::start_server(socket_path, false, default_auto_start_config()?)
        .map_err(|error| match error {
            StartServerError::Client(error) => client_error(socket_path, error),
            StartServerError::AutoStart(error) => error.to_string(),
        })?;
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    response
        .command_output()
        .ok_or_else(|| "protocol error: unexpected response for start-server".to_owned())?;
    Ok(0)
}

fn run_list_sessions(socket_path: &Path) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "list-sessions")
}

fn run_has_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyHasSession,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .has_session(request.target)
        .map_err(|error| error.to_string())?;
    match response {
        Response::HasSession(response) if response.exists => Ok(0),
        Response::HasSession(_) => exec_full_helper(original_args),
        response => write_response_output_or_error(response, "has-session"),
    }
}

fn run_list_windows(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyListWindows,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .list_windows(request.target, None)
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "list-windows")
}

fn run_list_panes(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyListPanes,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    match request {
        TinyListPanes::AllSessions => run_list_panes_all_sessions(&mut connection),
        TinyListPanes::Target {
            target,
            target_window_index,
        } => {
            let target_window_index = match target_window_index {
                Some(index) => Some(index),
                None => match resolve_active_window_index(&mut connection, &target) {
                    Ok(index) => Some(index),
                    Err(_) => return exec_full_helper(original_args),
                },
            };
            let response = connection
                .list_panes_in_window(
                    target,
                    target_window_index,
                    Some(DEFAULT_LIST_PANES_WINDOW_FORMAT.to_owned()),
                )
                .map_err(|error| error.to_string())?;
            if response_needs_session_resolution_retry(&response) {
                return exec_full_helper(original_args);
            }
            write_response_output_or_error(response, "list-panes")
        }
    }
}

fn run_list_panes_all_sessions(connection: &mut Connection) -> Result<i32, String> {
    let sessions = list_session_names(connection)?;
    let mut stdout = Vec::new();
    for session_name in sessions {
        let response = connection
            .list_panes_in_window(
                session_name,
                None,
                Some(DEFAULT_LIST_PANES_ALL_FORMAT.to_owned()),
            )
            .map_err(|error| error.to_string())?;
        stdout.extend_from_slice(&response_stdout(response, "list-panes")?);
    }
    write_stdout(&stdout)
        .map(|()| 0)
        .map_err(|error| format!("failed to write list-panes command output: {error}"))
}

fn list_session_names(connection: &mut Connection) -> Result<Vec<rmux_proto::SessionName>, String> {
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    let stdout = response_stdout(response, "list-sessions")?;
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            rmux_proto::SessionName::new(line)
                .map_err(|error| format!("invalid session name from list-sessions: {error}"))
        })
        .collect()
}

fn response_stdout(response: Response, command: &str) -> Result<Vec<u8>, String> {
    match response {
        Response::Error(error) => Err(tmux_cli_error_message(command, &error.error)),
        response => response
            .command_output()
            .map(|output| output.stdout().to_vec())
            .ok_or_else(|| format!("{command} returned no command output")),
    }
}

fn resolve_active_window_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
) -> Result<u32, String> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some("#{window_index}:#{window_active}".to_owned()),
        )
        .map_err(|error| error.to_string())?;
    let output = response
        .command_output()
        .ok_or_else(|| "protocol error: list-windows did not return command output".to_owned())?;
    let stdout = String::from_utf8_lossy(output.stdout());
    let active_line = stdout
        .lines()
        .find(|line| line.rsplit(':').next() == Some("1"))
        .ok_or_else(|| "list-panes could not resolve the active window".to_owned())?;
    active_line
        .split(':')
        .next()
        .ok_or_else(|| "active window output is malformed".to_owned())?
        .parse::<u32>()
        .map_err(|error| format!("invalid active window index from server: {error}"))
}

#[cfg(windows)]
fn run_kill_server(socket_path: &Path) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    match connection.kill_server_after_write() {
        Ok(()) => Ok(0),
        Err(error) if kill_server_connection_closed(&error) => Ok(0),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(not(windows))]
fn run_kill_server(socket_path: &Path) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    match connection.kill_server() {
        Ok(response) => {
            let code = write_response_output_or_error(response, "kill-server")?;
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(code)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) if unsupported_wire_version(&error) => {
            run_legacy_wire_v1_kill_server(socket_path)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn run_display_message(socket_path: &Path, request: TinyDisplayMessage) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let target = request.target;
    let message = request.message;
    let has_explicit_target = target.is_some();
    let response = connection
        .display_message(target, true, message.clone())
        .map_err(|error| error.to_string())?;
    if has_explicit_target && display_message_missing_target_uses_empty_context(&response) {
        let response = connection
            .roundtrip(&Request::DisplayMessage(DisplayMessageRequest {
                target: None,
                print: true,
                message,
                empty_target_context: true,
            }))
            .map_err(|error| error.to_string())?;
        return write_response_output_or_error(response, "display-message");
    }
    write_response_output_or_error(response, "display-message")
}

fn display_message_missing_target_uses_empty_context(response: &Response) -> bool {
    matches!(
        response,
        Response::Error(error)
            if matches!(
                &error.error,
                RmuxError::InvalidTarget { .. } | RmuxError::SessionNotFound(_)
            )
    )
}

fn run_send_keys(socket_path: &Path, request: TinySendKeys) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .send_keys(request.target, request.keys)
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "send-keys")
}

fn run_new_window(socket_path: &Path, request: TinyNewWindow) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .new_window_at_with_environment(
            request.target,
            None,
            request.name,
            request.detached,
            None,
            request.start_directory,
            request.command,
            false,
        )
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "new-window")
}

fn run_kill_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyKillSession,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .kill_session(KillSessionRequest {
            target: request.target,
            kill_all_except_target: false,
            clear_alerts: false,
        })
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "kill-session")
}

fn run_show_options(
    socket_path: &Path,
    request: TinyShowOptions,
    command_name: &'static str,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .show_options(request.scope, None, false, false, false)
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, command_name)
}

fn run_rename_window(socket_path: &Path, request: TinyRenameWindow) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .rename_window(request.target, request.name.replace('\\', r"\\"))
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "rename-window")
}

fn run_select_window(socket_path: &Path, request: TinySelectWindow) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .select_window(request.target)
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "select-window")
}

fn run_kill_pane(socket_path: &Path, request: TinyKillPane) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .kill_pane(request.target)
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "kill-pane")
}

fn run_join_pane(socket_path: &Path, request: TinyJoinPane) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .join_pane(JoinPaneRequest {
            source: request.source,
            target: request.target,
            direction: SplitDirection::Vertical,
            detached: request.detached,
            before: false,
            full_size: false,
            size: None,
        })
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "join-pane")
}

fn run_set_option(
    socket_path: &Path,
    request: TinySetOption,
    command_name: &'static str,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .set_option_by_name(
            request.scope,
            request.option,
            Some(request.value),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, command_name)
}

fn run_source_file(socket_path: &Path, request: TinySourceFile) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = connection
        .source_file(request.paths, false, false, false, false, None, None)
        .map_err(|error| error.to_string())?;
    write_source_file_response(response)
}

fn run_new_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: NewSessionExtRequest,
) -> Result<i32, String> {
    let mut connection = connect_with_validated_startup(socket_path)?;

    let response = connection.new_session_extended(request);
    if response_needs_full_helper(&response, RetryPolicy::DecodeOnly) {
        return exec_full_helper(original_args);
    }
    let response = response.map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "new-session")
}

fn run_attach_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: AttachSessionExt2Request,
) -> Result<i32, String> {
    let mut connection = match connect_with_validated_startup(socket_path) {
        Ok(connection) => connection,
        Err(_) => return exec_full_helper(original_args),
    };
    if !server_has_sessions(&mut connection)? {
        let _ = connection.kill_server();
        wait_for_killed_server_socket_cleanup(socket_path);
        return Err("no sessions".to_owned());
    }

    let attach_resize_geometry = connection
        .supports_capability(CAPABILITY_ATTACH_RESIZE_GEOMETRY)
        .map_err(|error| error.to_string())?;
    let attach_render = connection
        .supports_capability(CAPABILITY_ATTACH_RENDER)
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    let attach_windows_console_key = connection
        .supports_capability(CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY)
        .map_err(|error| error.to_string())?;
    let transition = if attach_render {
        connection.begin_attach_with_capabilities(AttachSessionExt3Request::from_ext2(
            request,
            vec![CAPABILITY_ATTACH_RENDER.to_owned()],
        ))
    } else {
        connection.begin_attach_with_target_spec(request)
    }
    .map_err(|error| error.to_string())?;

    match transition {
        AttachTransition::Upgraded(upgrade) => {
            let (stream, initial_bytes) = upgrade.into_parts();
            #[cfg(unix)]
            {
                if attach_resize_geometry {
                    attach_terminal_with_initial_bytes_and_resize_geometry(stream, initial_bytes)
                        .map_err(|error| error.to_string())?;
                } else {
                    attach_terminal_with_initial_bytes(stream, initial_bytes)
                        .map_err(|error| error.to_string())?;
                }
            }
            #[cfg(windows)]
            {
                let _ = attach_resize_geometry;
                attach_terminal_with_initial_bytes_and_windows_console_key(
                    stream,
                    initial_bytes,
                    attach_windows_console_key,
                )
                .map_err(|error| error.to_string())?;
            }
            #[cfg(all(not(unix), not(windows)))]
            {
                let _ = attach_resize_geometry;
                attach_terminal_with_initial_bytes(stream, initial_bytes)
                    .map_err(|error| error.to_string())?;
            }
            Ok(0)
        }
        AttachTransition::Rejected(response) if response_is_decode_error(&response) => {
            exec_full_helper(original_args)
        }
        AttachTransition::Rejected(response) => {
            write_response_output_or_error(response, "attach-session")
        }
    }
}

#[derive(Clone, Copy)]
enum RetryPolicy {
    DecodeOnly,
    DecodeOrEof,
}

fn run_target_action(
    original_args: &[OsString],
    socket_path: &Path,
    command: &str,
    retry_policy: RetryPolicy,
    send: impl FnOnce(&mut rmux_client::Connection) -> Result<Response, ClientError>,
) -> Result<i32, String> {
    let mut connection = connect(socket_path).map_err(|error| client_error(socket_path, error))?;
    let response = send(&mut connection);
    if response_needs_full_helper(&response, retry_policy) {
        return exec_full_helper(original_args);
    }
    let response = response.map_err(|error| error.to_string())?;
    write_response_output_or_error(response, command)
}

fn connect_with_validated_startup(socket_path: &Path) -> Result<Connection, String> {
    ensure_server_running_with_config(socket_path, default_auto_start_config()?)
        .map_err(|error| error.to_string())
}

fn default_auto_start_config() -> Result<AutoStartConfig, String> {
    let config = AutoStartConfig::default_files(true, env::current_dir().ok());

    #[cfg(windows)]
    {
        Ok(config)
    }

    #[cfg(not(windows))]
    {
        let helper = daemon_helper_path()?;
        Ok(config.with_binary_override(helper))
    }
}

fn server_has_sessions(connection: &mut Connection) -> Result<bool, String> {
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(|error| error.to_string())?;
    match response {
        Response::ListSessions(response) => Ok(!response.command_output().stdout().is_empty()),
        Response::Error(error) => {
            write_response_output_or_error(Response::Error(error), "list-sessions").map(|_| false)
        }
        other => write_response_output_or_error(other, "list-sessions").map(|_| false),
    }
}

fn write_source_file_response(response: Response) -> Result<i32, String> {
    match response {
        Response::SourceFile(response) => write_source_file_success_response(response),
        Response::Error(error) if source_file_error_uses_stdout(&error.error) => {
            let message = tmux_cli_error_message("source-file", &error.error);
            write_stdout(format!("{message}\n").as_bytes()).map_err(|error| error.to_string())?;
            Ok(1)
        }
        other => write_response_output_or_error(other, "source-file"),
    }
}

fn write_source_file_success_response(response: SourceFileResponse) -> Result<i32, String> {
    if let Some(output) = response.command_output() {
        write_stdout(output.stdout()).map_err(|error| error.to_string())?;
    }
    Ok(response.exit_status().unwrap_or(0))
}

#[cfg(not(windows))]
fn run_legacy_wire_v1_kill_server(socket_path: &Path) -> Result<i32, String> {
    let mut connection =
        match connect_or_absent(socket_path).map_err(|error| client_error(socket_path, error))? {
            ConnectResult::Connected(connection) => connection,
            ConnectResult::Absent => {
                wait_for_killed_server_socket_cleanup(socket_path);
                return Ok(0);
            }
        };

    match connection.kill_server_legacy_wire_v1() {
        Ok(()) => {
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) if kill_server_connection_closed(&error) => {
            wait_for_killed_server_socket_cleanup(socket_path);
            Ok(0)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn kill_server_connection_closed(error: &ClientError) -> bool {
    matches!(error, ClientError::UnexpectedEof)
        || matches!(
            error,
            ClientError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                )
        )
}

#[cfg(not(windows))]
fn unsupported_wire_version(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Protocol(RmuxError::UnsupportedWireVersion { .. })
    )
}

fn response_needs_full_helper(
    response: &Result<Response, ClientError>,
    retry_policy: RetryPolicy,
) -> bool {
    let decode_error = matches!(
        response,
        Ok(Response::Error(error)) if matches!(error.error, RmuxError::Decode(_))
    );
    let eof_retry = matches!(retry_policy, RetryPolicy::DecodeOrEof)
        && matches!(response, Err(ClientError::UnexpectedEof));
    decode_error || eof_retry
}

fn response_needs_session_resolution_retry(response: &Response) -> bool {
    matches!(
        response,
        Response::Error(error)
            if matches!(
                error.error,
                RmuxError::SessionNotFound(_) | RmuxError::InvalidTarget { .. }
            )
    )
}

fn response_is_decode_error(response: &Response) -> bool {
    matches!(response, Response::Error(error) if matches!(error.error, RmuxError::Decode(_)))
}

#[cfg(unix)]
fn wait_for_killed_server_socket_cleanup(socket_path: &Path) {
    let deadline = Instant::now() + KILL_SERVER_SOCKET_CLEANUP_TIMEOUT;
    let mut next_poll = KILL_SERVER_SOCKET_CLEANUP_MIN_POLL;
    while socket_path.exists() && Instant::now() < deadline {
        std::thread::sleep(next_poll);
        next_poll = (next_poll + next_poll).min(KILL_SERVER_SOCKET_CLEANUP_MAX_POLL);
    }
}

#[cfg(not(unix))]
fn wait_for_killed_server_socket_cleanup(_socket_path: &Path) {}

#[cfg(test)]
mod tests;
