//! Windows shell detection for a target pane: `cmd.exe` and WSL control-key routing.

use rmux_proto::{PaneTarget, ProcessCommand};

use super::pane_id_for_input_target;
use crate::pane_terminals::HandlerState;

pub(super) fn target_uses_windows_cmd_shell(state: &HandlerState, target: &PaneTarget) -> bool {
    let profile_shell_is_cmd = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(|profile| profile.shell().file_name())
        .and_then(|name| name.to_str())
        .is_some_and(is_windows_cmd_name);
    if profile_shell_is_cmd {
        return true;
    }

    state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(rmux_os::process::command_name)
        .as_deref()
        .is_some_and(is_windows_cmd_name)
}

pub(super) fn target_uses_windows_cmd_console_ctrl_d(
    state: &HandlerState,
    target: &PaneTarget,
) -> bool {
    if let Some(uses_cmd) = pane_id_for_input_target(state, target)
        .ok()
        .and_then(|pane_id| state.pane_start_process_command_for_id(pane_id))
        .and_then(process_command_windows_cmd_hint)
    {
        return uses_cmd;
    }

    if let Some(profile_shell_is_cmd) = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(|profile| {
            profile
                .shell()
                .file_name()
                .and_then(|name| name.to_str())
                .map(is_windows_cmd_name)
        })
    {
        return profile_shell_is_cmd;
    }

    state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(rmux_os::process::command_name)
        .as_deref()
        .is_some_and(is_windows_cmd_name)
}

fn process_command_windows_cmd_hint(command: &ProcessCommand) -> Option<bool> {
    match command {
        // Shell text is normally executed by the configured shell, but an
        // explicit nested shell overrides that profile for Ctrl-D semantics.
        ProcessCommand::Shell(command) => shell_text_windows_cmd_hint(command),
        ProcessCommand::Argv(argv) => Some(argv_invokes_windows_cmd(argv)),
        _ => None,
    }
}

fn shell_text_windows_cmd_hint(command: &str) -> Option<bool> {
    let head = windows_shell_command_head(command)?;
    let name = windows_command_name(head);
    if is_windows_cmd_name(name) {
        Some(true)
    } else if matches!(
        name.to_ascii_lowercase().as_str(),
        "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe" | "wsl" | "wsl.exe"
    ) {
        Some(false)
    } else {
        None
    }
}

fn windows_shell_command_head(command: &str) -> Option<&str> {
    let command = command.trim_start();
    let quote = command.as_bytes().first().copied()?;
    if matches!(quote, b'"' | b'\'') {
        let quoted = &command[1..];
        let end = quoted.as_bytes().iter().position(|byte| *byte == quote)?;
        return Some(&quoted[..end]);
    }
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    Some(&command[..end])
}

fn argv_invokes_windows_cmd(argv: &[String]) -> bool {
    let Some(head) = argv.first() else {
        return false;
    };
    let name = windows_command_name(head);
    is_windows_cmd_name(name)
        || std::path::Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
}

fn windows_command_name(command: &str) -> &str {
    let trimmed = command.trim_matches(['"', '\'']);
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(trimmed)
}

pub(super) fn target_uses_wsl_host_process(state: &HandlerState, target: &PaneTarget) -> bool {
    let lifecycle_command = pane_id_for_input_target(state, target)
        .ok()
        .and_then(|pane_id| state.pane_start_command_for_id(pane_id));
    let lifecycle_matches = lifecycle_command.is_some_and(command_invokes_wsl);
    let process_name = state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(rmux_os::process::command_name);
    let process_matches = process_name.as_deref().is_some_and(is_wsl_host_name);
    trace_windows_wsl_detection(process_name.as_deref(), lifecycle_matches, process_matches);
    if lifecycle_matches {
        return true;
    }

    process_matches
}

pub(super) fn target_routes_windows_ctrl_d_as_posix_eot(
    state: &HandlerState,
    target: &PaneTarget,
) -> bool {
    target_uses_wsl_host_process(state, target) || target_has_wsl_descendant_process(state, target)
}

fn target_has_wsl_descendant_process(state: &HandlerState, target: &PaneTarget) -> bool {
    let Some(pane_pid) = state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
    else {
        return false;
    };

    let descendant_matches = rmux_os::process::descendant_command_names(pane_pid)
        .iter()
        .any(|name| is_wsl_host_name(name));
    trace_windows_wsl_descendant_detection(pane_pid, descendant_matches);
    descendant_matches
}

fn trace_windows_wsl_descendant_detection(pane_pid: u32, descendant_matches: bool) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        pane_pid,
        descendant_matches,
        "detect Windows WSL descendant control-key routing"
    );
}

fn trace_windows_wsl_detection(
    process_name: Option<&str>,
    lifecycle_matches: bool,
    process_matches: bool,
) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        process_name,
        lifecycle_matches,
        process_matches,
        "detect Windows WSL control-key routing"
    );
}

fn command_invokes_wsl(command: &[String]) -> bool {
    command.iter().any(|part| {
        part.split_whitespace()
            .next()
            .is_some_and(is_wsl_command_head)
            || part.to_ascii_lowercase().contains("wsl.exe")
    })
}

fn is_wsl_command_head(head: &str) -> bool {
    let trimmed = head.trim_matches(['"', '\'']);
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_wsl_host_name)
        || is_wsl_host_name(trimmed)
}

fn is_wsl_host_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "wsl.exe"
        || lower == "wsl"
        || lower.ends_with("\\wsl.exe")
        || lower.ends_with("/wsl.exe")
}

fn is_windows_cmd_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("cmd.exe") || name.eq_ignore_ascii_case("cmd")
}

#[cfg(test)]
mod windows_tests {
    use rmux_proto::ProcessCommand;

    #[test]
    fn windows_ctrl_d_launch_plan_detection_preserves_shell_wrapper() {
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                "timeout.exe /T 10000".to_owned()
            )),
            None,
            "shell text must defer to the configured shell profile"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                "pwsh.exe -NoLogo -NoProfile".to_owned()
            )),
            Some(false),
            "an explicit PowerShell command must override a cmd profile"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoProfile"#.to_owned()
            )),
            Some(false),
            "a quoted PowerShell path must remain one command token"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                "cmd.exe /D /Q /K".to_owned()
            )),
            Some(true)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                r#""C:\Windows\System32\cmd.exe" /D /Q /K"#.to_owned()
            )),
            Some(true),
            "a quoted cmd path must retain physical Ctrl-D semantics"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                "cmd.exe".to_owned(),
                "/K".to_owned(),
            ])),
            Some(true)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                r"C:\tools\build.cmd".to_owned(),
            ])),
            Some(true)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                "pwsh.exe".to_owned(),
                "-NoProfile".to_owned(),
            ])),
            Some(false)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                "python.exe".to_owned(),
            ])),
            Some(false)
        );
    }
}
