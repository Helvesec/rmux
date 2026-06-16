use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmux_pty::ChildCommand;

#[cfg(windows)]
use super::executable_name;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct ShellSpec {
    program: PathBuf,
    kind: ShellKind,
}

impl ShellSpec {
    pub(super) fn new(shell: &Path) -> Self {
        Self {
            program: shell.to_path_buf(),
            kind: detect_shell_kind(shell),
        }
    }

    pub(super) fn command_child(&self, cwd: &Path, command: &str) -> ChildCommand {
        self.command_plan(cwd, command).into_child_command()
    }

    pub(super) fn command_std_child(&self, cwd: &Path, command: &str) -> StdCommand {
        self.command_plan(cwd, command).into_std_command()
    }

    pub(super) fn interactive_child(&self, cwd: &Path) -> ChildCommand {
        self.interactive_plan(cwd).into_child_command()
    }

    fn command_plan(&self, cwd: &Path, command: &str) -> ShellCommandPlan {
        #[cfg(unix)]
        let _ = cwd;

        match self.kind {
            #[cfg(unix)]
            ShellKind::Unix => ShellCommandPlan::new(&self.program).arg("-c").arg(command),
            #[cfg(windows)]
            ShellKind::PowerShell => ShellCommandPlan::new(&self.program)
                .arg("-NoProfile")
                .arg("-Command")
                .arg(format!(
                    "Set-Location -LiteralPath {}; {command}",
                    powershell_single_quoted(cwd)
                )),
            #[cfg(windows)]
            ShellKind::Cmd => ShellCommandPlan::new(&self.program)
                .arg("/D")
                .arg("/S")
                .arg("/C")
                .arg(command),
            #[cfg(windows)]
            ShellKind::Posix => ShellCommandPlan::new(&self.program).arg("-lc").arg(command),
            #[cfg(windows)]
            ShellKind::Nu => ShellCommandPlan::new(&self.program).arg("-c").arg(command),
            #[cfg(windows)]
            ShellKind::Other => ShellCommandPlan::new(&self.program).arg(command),
        }
    }

    fn interactive_plan(&self, cwd: &Path) -> ShellCommandPlan {
        #[cfg(unix)]
        let _ = cwd;

        match self.kind {
            #[cfg(unix)]
            ShellKind::Unix => {
                ShellCommandPlan::new(&self.program).arg0(login_shell_argv0(&self.program))
            }
            #[cfg(windows)]
            ShellKind::PowerShell => ShellCommandPlan::new(&self.program)
                .arg("-NoLogo")
                .arg("-NoProfile")
                .arg("-NoExit")
                .arg("-Command")
                .arg(format!(
                    "Set-Location -LiteralPath {}",
                    powershell_single_quoted(cwd)
                )),
            #[cfg(windows)]
            ShellKind::Cmd => ShellCommandPlan::new(&self.program).arg("/D").arg("/K"),
            #[cfg(windows)]
            ShellKind::Posix | ShellKind::Nu | ShellKind::Other => {
                ShellCommandPlan::new(&self.program)
            }
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ShellCommandPlan {
    program: PathBuf,
    arg0: Option<OsString>,
    args: Vec<OsString>,
}

impl ShellCommandPlan {
    fn new(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
            arg0: None,
            args: Vec::new(),
        }
    }

    fn arg0(mut self, arg0: impl Into<OsString>) -> Self {
        self.arg0 = Some(arg0.into());
        self
    }

    fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    fn into_child_command(self) -> ChildCommand {
        let mut command = ChildCommand::new(self.program);
        if let Some(arg0) = self.arg0 {
            command = command.arg0(arg0);
        }
        command.args(self.args)
    }

    fn into_std_command(self) -> StdCommand {
        let mut command = StdCommand::new(self.program);
        #[cfg(unix)]
        if let Some(arg0) = self.arg0 {
            use std::os::unix::process::CommandExt as _;
            command.arg0(arg0);
        }
        command.args(self.args);
        command
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellKind {
    #[cfg(unix)]
    Unix,
    #[cfg(windows)]
    Cmd,
    #[cfg(windows)]
    PowerShell,
    #[cfg(windows)]
    Posix,
    #[cfg(windows)]
    Nu,
    #[cfg(windows)]
    Other,
}

#[cfg(unix)]
fn detect_shell_kind(_shell: &Path) -> ShellKind {
    ShellKind::Unix
}

#[cfg(windows)]
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

#[cfg(windows)]
fn powershell_single_quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(unix)]
fn login_shell_argv0(shell: &Path) -> OsString {
    let name = shell
        .file_name()
        .unwrap_or(shell.as_os_str())
        .to_os_string();
    let mut login_name = OsString::from("-");
    login_name.push(name);
    login_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn detects_windows_shell_families_by_executable_name() {
        assert_eq!(
            detect_shell_kind(Path::new(r"C:\Windows\System32\cmd.exe")),
            ShellKind::Cmd
        );
        assert_eq!(
            detect_shell_kind(Path::new("powershell")),
            ShellKind::PowerShell
        );
        assert_eq!(
            detect_shell_kind(Path::new("pwsh.exe")),
            ShellKind::PowerShell
        );
        assert_eq!(detect_shell_kind(Path::new("bash.exe")), ShellKind::Posix);
        assert_eq!(detect_shell_kind(Path::new("nu.exe")), ShellKind::Nu);
    }

    #[cfg(windows)]
    #[test]
    fn cmd_interactive_uses_current_dir_instead_of_cd_wrapper() {
        let spec = ShellSpec::new(Path::new("cmd.exe"));
        let plan = spec.interactive_plan(Path::new(r"C:\Users\RMUXUser\Documents\rmux"));

        assert_eq!(plan.program, PathBuf::from("cmd.exe"));
        assert_eq!(plan.arg0, None);
        assert_eq!(plan.args, os_args(["/D", "/K"]));
    }

    #[cfg(windows)]
    #[test]
    fn cmd_command_preserves_command_text_without_wrapping_cwd() {
        let spec = ShellSpec::new(Path::new("cmd.exe"));
        let plan = spec.command_plan(Path::new(r"C:\tmp"), "echo RMUX_OK");

        assert_eq!(plan.args, os_args(["/D", "/S", "/C", "echo RMUX_OK"]));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_plans_quote_cwd_with_literal_path() {
        let spec = ShellSpec::new(Path::new("pwsh.exe"));
        let cwd = Path::new(r"C:\Users\RMUXUser's Workspace\rmux");

        let interactive = spec.interactive_plan(cwd);
        assert_eq!(
            interactive.args,
            os_args([
                "-NoLogo",
                "-NoProfile",
                "-NoExit",
                "-Command",
                "Set-Location -LiteralPath 'C:\\Users\\RMUXUser''s Workspace\\rmux'",
            ])
        );

        let one_shot = spec.command_plan(cwd, "Write-Output RMUX_OK");
        assert_eq!(
            one_shot.args,
            os_args([
                "-NoProfile",
                "-Command",
                "Set-Location -LiteralPath 'C:\\Users\\RMUXUser''s Workspace\\rmux'; Write-Output RMUX_OK",
            ])
        );
    }

    #[cfg(windows)]
    #[test]
    fn posix_shell_command_uses_lc_not_cmd_c() {
        let spec = ShellSpec::new(Path::new("bash.exe"));
        let plan = spec.command_plan(Path::new(r"C:\tmp"), "echo RMUX_OK");

        assert_eq!(plan.args, os_args(["-lc", "echo RMUX_OK"]));
    }

    #[cfg(windows)]
    #[test]
    fn nushell_command_uses_c_not_cmd_c() {
        let spec = ShellSpec::new(Path::new("nu.exe"));
        let plan = spec.command_plan(Path::new(r"C:\tmp"), "echo RMUX_OK");

        assert_eq!(plan.args, os_args(["-c", "echo RMUX_OK"]));
    }

    #[cfg(windows)]
    #[test]
    fn unknown_windows_shell_does_not_receive_cmd_c_flag() {
        let spec = ShellSpec::new(Path::new("custom-shell.exe"));
        let plan = spec.command_plan(Path::new(r"C:\tmp"), "echo RMUX_OK");

        assert_eq!(plan.args, os_args(["echo RMUX_OK"]));
    }

    #[cfg(unix)]
    #[test]
    fn unix_interactive_shell_uses_login_argv0() {
        let spec = ShellSpec::new(Path::new("/bin/bash"));
        let plan = spec.interactive_plan(Path::new("/tmp"));

        assert_eq!(plan.program, PathBuf::from("/bin/bash"));
        assert_eq!(plan.arg0, Some(OsString::from("-bash")));
        assert!(plan.args.is_empty());
    }

    #[cfg(windows)]
    fn os_args<const N: usize>(args: [&str; N]) -> Vec<OsString> {
        args.into_iter().map(OsString::from).collect()
    }
}
