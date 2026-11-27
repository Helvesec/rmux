use std::ffi::OsString;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use rmux_os::command::cmd_c_verbatim_tail;
use rmux_proto::AttachShellCommand;

pub(super) fn command_from_spec(spec: &AttachShellCommand) -> Command {
    command_for_shell(
        Path::new(spec.shell()),
        Path::new(spec.cwd()),
        spec.command(),
    )
}

pub(super) fn command_from_legacy(command: &str) -> Command {
    let cwd = std::env::current_dir().unwrap_or_else(|_| fallback_cwd());
    let shell = std::env::var_os("SHELL")
        .or_else(|| std::env::var_os("COMSPEC"))
        .unwrap_or_else(|| OsString::from("cmd.exe"));
    command_for_shell(Path::new(&shell), &cwd, command)
}

fn command_for_shell(shell: &Path, cwd: &Path, command: &str) -> Command {
    let mut child = Command::new(shell);
    child.current_dir(cwd);
    match detect_shell_kind(shell) {
        ShellKind::Cmd => {
            child.arg("/D").arg("/S").arg("/C");
            child.raw_arg(cmd_c_verbatim_tail(command));
        }
        ShellKind::PowerShell => {
            child.arg("-NoProfile").arg("-Command").arg(format!(
                "Set-Location -LiteralPath {}; {command}",
                powershell_single_quoted(cwd)
            ));
        }
        ShellKind::Posix => {
            child.arg("-lc").arg(command);
        }
        ShellKind::Nu => {
            child.arg("-c").arg(command);
        }
        ShellKind::Other => {
            child.arg(command);
        }
    }
    child
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellKind {
    Cmd,
    PowerShell,
    Posix,
    Nu,
    Other,
}

fn detect_shell_kind(shell: &Path) -> ShellKind {
    match executable_name(shell)
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("cmd.exe" | "cmd") => ShellKind::Cmd,
        Some("powershell.exe" | "powershell" | "pwsh.exe" | "pwsh") => ShellKind::PowerShell,
        Some("bash.exe" | "bash" | "sh.exe" | "sh" | "zsh.exe" | "zsh") => ShellKind::Posix,
        Some("nu.exe" | "nu") => ShellKind::Nu,
        _ => ShellKind::Other,
    }
}

fn executable_name(path: &Path) -> Option<String> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let trimmed = name.trim_start_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn powershell_single_quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn fallback_cwd() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn cmd_spec_uses_server_shell_and_preserves_payload() {
        let payload = r#""C:\Program Files\RMUX\helper.exe" "quoted arg" & echo done"#;
        let spec = AttachShellCommand::new(
            payload.to_owned(),
            "cmd.exe".to_owned(),
            r"C:\work".to_owned(),
        );

        let child = command_from_spec(&spec);

        assert_eq!(
            args(&child),
            vec![
                "/D".to_owned(),
                "/S".to_owned(),
                "/C".to_owned(),
                cmd_c_verbatim_tail(payload).to_string_lossy().into_owned(),
            ]
        );
        assert_eq!(child.get_current_dir(), Some(Path::new(r"C:\work")));
    }

    #[test]
    fn cmd_spec_does_not_inject_backslashes_around_quoted_arguments() {
        let spec = AttachShellCommand::new(
            r#"echo "RMUX CMD TAIL""#.to_owned(),
            "cmd.exe".to_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
        );

        let output = command_from_spec(&spec)
            .output()
            .expect("cmd attach action should run");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{output:?}");
        assert_eq!(stdout.trim(), r#""RMUX CMD TAIL""#);
    }

    #[test]
    fn powershell_spec_uses_literal_cwd_wrapper() {
        let spec = AttachShellCommand::new(
            "Write-Output RMUX_OK".to_owned(),
            "pwsh.exe".to_owned(),
            r"C:\Users\RMUXUser's Workspace\rmux".to_owned(),
        );

        let child = command_from_spec(&spec);

        assert_eq!(
            args(&child),
            vec![
                "-NoProfile",
                "-Command",
                "Set-Location -LiteralPath 'C:\\Users\\RMUXUser''s Workspace\\rmux'; Write-Output RMUX_OK",
            ]
        );
    }

    #[test]
    fn posix_spec_uses_lc_instead_of_cmd_c() {
        let spec = AttachShellCommand::new(
            "echo RMUX_OK".to_owned(),
            "bash.exe".to_owned(),
            r"C:\work".to_owned(),
        );

        let child = command_from_spec(&spec);

        assert_eq!(args(&child), vec!["-lc", "echo RMUX_OK"]);
    }

    #[test]
    fn nushell_spec_uses_c_instead_of_cmd_c() {
        let spec = AttachShellCommand::new(
            "echo RMUX_OK".to_owned(),
            "nu.exe".to_owned(),
            r"C:\work".to_owned(),
        );

        let child = command_from_spec(&spec);

        assert_eq!(args(&child), vec!["-c", "echo RMUX_OK"]);
    }

    #[test]
    fn custom_shell_gets_only_the_payload() {
        let spec = AttachShellCommand::new(
            "echo RMUX_OK".to_owned(),
            "custom-shell.exe".to_owned(),
            r"C:\work".to_owned(),
        );

        let child = command_from_spec(&spec);

        assert_eq!(args(&child), vec!["echo RMUX_OK"]);
    }

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(os_to_string).collect()
    }

    fn os_to_string(value: &OsStr) -> String {
        value.to_string_lossy().into_owned()
    }
}
