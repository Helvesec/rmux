use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::RequestHandler;
use crate::handler::scripting_support::QueueExecutionContext;
use crate::hook_runtime::with_hook_execution;
use rmux_core::command_parser::CommandParser;
use rmux_core::TargetFindContext;
use rmux_proto::{
    BreakPaneRequest, CommandOutput, DisplayMessageRequest, IfShellRequest, KillWindowRequest,
    LinkWindowRequest, NewSessionExtRequest, NewSessionRequest, NewWindowRequest, OptionName,
    OptionScopeSelector, PaneTarget, Request, RespawnPaneRequest, RespawnWindowRequest, Response,
    RotateWindowDirection, RotateWindowRequest, RunShellRequest, RunShellResponse, ScopeSelector,
    SelectPaneRequest, SessionName, SetEnvironmentRequest, SetOptionMode, SetOptionRequest,
    ShowBufferRequest, ShowOptionsRequest, SourceFileRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, SwapPaneDirection, SwapPaneRequest, Target, TerminalSize, WaitForMode,
    WaitForRequest, WaitForResponse, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn wait_for(channel: &str, mode: WaitForMode) -> Request {
    Request::WaitFor(WaitForRequest {
        channel: channel.to_owned(),
        mode,
    })
}

fn run_shell(command: &str, background: bool) -> Request {
    Request::RunShell(RunShellRequest {
        command: command.to_owned(),
        background,

        as_commands: false,
        show_stderr: false,
        delay_seconds: None,
        start_directory: None,
        target: None,
    })
}

fn source_file_request(paths: Vec<String>, cwd: Option<PathBuf>) -> Request {
    Request::SourceFile(SourceFileRequest {
        paths,
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: cwd,
        stdin: None,
    })
}

fn temp_root(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-source-file-{label}-{}-{unique}",
        std::process::id()
    ))
}

fn write_config(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("config parent directory");
    }
    fs::write(path, contents).expect("write config");
}

fn write_executable_script(path: &Path, contents: &str) {
    write_config(path, contents);
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).expect("script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("script permissions");
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn command_quote(command: &str) -> String {
    crate::test_shell::command_quote(command)
}

#[cfg(unix)]
fn shell_print_command(text: &str) -> String {
    format!("printf {}", command_quote(text))
}

#[cfg(windows)]
fn shell_print_command(text: &str) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[Console]::Out.Write({})",
        crate::test_shell::powershell_quote(text)
    ))
}

#[cfg(unix)]
fn shell_print_then_exit_command(text: &str, code: u8) -> String {
    format!("printf {}; exit {code}", command_quote(text))
}

#[cfg(windows)]
fn shell_print_then_exit_command(text: &str, code: u8) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[Console]::Out.Write({}); exit {code}",
        crate::test_shell::powershell_quote(text)
    ))
}

#[cfg(unix)]
fn shell_success_command() -> String {
    "true".to_owned()
}

#[cfg(windows)]
fn shell_success_command() -> String {
    crate::test_shell::powershell_encoded_command("exit 0")
}

#[cfg(unix)]
fn shell_env_or_default_command(name: &str, default: &str) -> String {
    format!("printf %s \"${{{name}-{default}}}\"")
}

#[cfg(windows)]
fn shell_env_or_default_command(name: &str, default: &str) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "$value=[Environment]::GetEnvironmentVariable({}); if ([string]::IsNullOrEmpty($value)) {{ $value={} }}; [Console]::Out.Write($value)",
        crate::test_shell::powershell_quote(name),
        crate::test_shell::powershell_quote(default)
    ))
}

#[path = "handler_scripting_tests/run_shell.rs"]
mod run_shell;

#[path = "handler_scripting_tests/source_file_core.rs"]
mod source_file_core;

#[path = "handler_scripting_tests/source_file_conditions.rs"]
mod source_file_conditions;

#[path = "handler_scripting_tests/if_shell.rs"]
mod if_shell;

#[path = "handler_scripting_tests/parsed_queue_core.rs"]
mod parsed_queue_core;

#[path = "handler_scripting_tests/parsed_queue_split.rs"]
mod parsed_queue_split;

#[path = "handler_scripting_tests/parsed_queue_targets.rs"]
mod parsed_queue_targets;

#[path = "handler_scripting_tests/parsed_queue_windows_mouse.rs"]
mod parsed_queue_windows_mouse;

#[path = "handler_scripting_tests/control_hooks_wait.rs"]
mod control_hooks_wait;

#[path = "handler_scripting_tests/command_alias.rs"]
mod command_alias;

#[path = "handler_scripting_tests/command_blocks.rs"]
mod command_blocks;
