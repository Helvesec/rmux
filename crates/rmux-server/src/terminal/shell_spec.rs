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
            ShellKind::PowerShell | ShellKind::WindowsPowerShell => {
                ShellCommandPlan::new(&self.program)
                    .arg("-NoProfile")
                    .arg("-Command")
                    .arg(format!(
                        "Set-Location -LiteralPath {}; {command}",
                        powershell_single_quoted(cwd)
                    ))
            }
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
            ShellKind::Batch => ShellCommandPlan::new(&cmd_wrapper_program())
                .arg("/D")
                .arg("/S")
                .arg("/C")
                .windows_verbatim_args(batch_cmd_tail(&self.program, command)),
            #[cfg(windows)]
            ShellKind::Other => ShellCommandPlan::new(&self.program).arg(command),
        }
    }

    fn interactive_plan(&self, cwd: &Path) -> ShellCommandPlan {
        #[cfg(unix)]
        let _ = cwd;
        #[cfg(windows)]
        let _ = cwd;

        match self.kind {
            #[cfg(unix)]
            ShellKind::Unix => {
                ShellCommandPlan::new(&self.program).arg0(login_shell_argv0(&self.program))
            }
            #[cfg(windows)]
            ShellKind::PowerShell => ShellCommandPlan::new(&self.program)
                .arg("-NoLogo")
                .arg("-NoExit"),
            #[cfg(windows)]
            ShellKind::WindowsPowerShell => ShellCommandPlan::new(&self.program)
                .arg("-NoLogo")
                .arg("-NoExit"),
            #[cfg(windows)]
            ShellKind::Cmd => ShellCommandPlan::new(&self.program).arg("/D").arg("/K"),
            #[cfg(windows)]
            ShellKind::Batch => ShellCommandPlan::new(&cmd_wrapper_program())
                .arg("/D")
                .arg("/K")
                .arg(self.program.as_os_str()),
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
    #[cfg(windows)]
    windows_verbatim_args: Option<OsString>,
}

impl ShellCommandPlan {
    fn new(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
            arg0: None,
            args: Vec::new(),
            #[cfg(windows)]
            windows_verbatim_args: None,
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

    #[cfg(windows)]
    fn windows_verbatim_args(mut self, args: impl Into<OsString>) -> Self {
        self.windows_verbatim_args = Some(args.into());
        self
    }

    fn into_child_command(self) -> ChildCommand {
        let mut command = ChildCommand::new(self.program);
        if let Some(arg0) = self.arg0 {
            command = command.arg0(arg0);
        }
        command = command.args(self.args);
        #[cfg(windows)]
        if let Some(args) = self.windows_verbatim_args {
            command = command.windows_verbatim_args(args);
        }
        command
    }

    fn into_std_command(self) -> StdCommand {
        let mut command = StdCommand::new(self.program);
        #[cfg(unix)]
        if let Some(arg0) = self.arg0 {
            use std::os::unix::process::CommandExt as _;
            command.arg0(arg0);
        }
        command.args(self.args);
        #[cfg(windows)]
        if let Some(args) = self.windows_verbatim_args {
            use std::os::windows::process::CommandExt as _;
            command.raw_arg(args);
        }
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
    WindowsPowerShell,
    #[cfg(windows)]
    Posix,
    #[cfg(windows)]
    Nu,
    #[cfg(windows)]
    Batch,
    #[cfg(windows)]
    Other,
}

#[cfg(unix)]
fn detect_shell_kind(_shell: &Path) -> ShellKind {
    ShellKind::Unix
}

#[cfg(windows)]
fn detect_shell_kind(shell: &Path) -> ShellKind {
    if is_windows_batch_script(shell) {
        return ShellKind::Batch;
    }

    match executable_name(shell)
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("cmd.exe" | "cmd") => ShellKind::Cmd,
        Some("powershell.exe" | "powershell") => ShellKind::WindowsPowerShell,
        Some("pwsh.exe" | "pwsh") => ShellKind::PowerShell,
        Some("bash.exe" | "bash" | "sh.exe" | "sh" | "zsh.exe" | "zsh") => ShellKind::Posix,
        Some("nu.exe" | "nu") => ShellKind::Nu,
        _ => ShellKind::Other,
    }
}

#[cfg(windows)]
fn is_windows_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "bat" | "cmd"))
        .unwrap_or(false)
}

#[cfg(windows)]
fn powershell_single_quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(windows)]
fn cmd_wrapper_program() -> PathBuf {
    std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}

#[cfg(windows)]
fn batch_cmd_tail(program: &Path, command: &str) -> OsString {
    let tail = format!(
        "{} {}",
        cmd_double_quoted(&program.to_string_lossy()),
        cmd_double_quoted(command)
    );
    format!("\"{tail}\"").into()
}

#[cfg(windows)]
fn cmd_double_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
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
            ShellKind::WindowsPowerShell
        );
        assert_eq!(
            detect_shell_kind(Path::new("pwsh.exe")),
            ShellKind::PowerShell
        );
        assert_eq!(detect_shell_kind(Path::new("bash.exe")), ShellKind::Posix);
        assert_eq!(detect_shell_kind(Path::new("nu.exe")), ShellKind::Nu);
        assert_eq!(
            detect_shell_kind(Path::new(r"C:\Users\RMUX User\shells\custom.cmd")),
            ShellKind::Batch
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_interactive_launches_directly_and_loads_profiles() {
        let spec = ShellSpec::new(Path::new(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        ));
        let plan = spec.interactive_plan(Path::new(r"C:\tmp"));

        assert_eq!(
            plan.program,
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        );
        assert_eq!(plan.args, os_args(["-NoLogo", "-NoExit"]));
        assert!(
            !plan.args.iter().any(|arg| arg == "-NoProfile"),
            "interactive Windows PowerShell must load the user's profile"
        );
    }

    #[cfg(windows)]
    #[test]
    fn pwsh_interactive_loads_profiles() {
        let spec = ShellSpec::new(Path::new("pwsh.exe"));
        let plan = spec.interactive_plan(Path::new(r"C:\tmp"));

        assert_eq!(plan.program, PathBuf::from("pwsh.exe"));
        assert_eq!(plan.args, os_args(["-NoLogo", "-NoExit"]));
        assert_eq!(
            plan.args.iter().any(|arg| arg == "-NoProfile"),
            false,
            "interactive PowerShell must not suppress profiles"
        );
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
        assert_eq!(interactive.args, os_args(["-NoLogo", "-NoExit"]));

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

    #[cfg(windows)]
    #[test]
    fn batch_default_shell_interactive_uses_cmd_keepalive_wrapper() {
        let spec = ShellSpec::new(Path::new(r"C:\Users\RMUX User\shells\custom shell.cmd"));
        let plan = spec.interactive_plan(Path::new(r"C:\tmp"));

        assert_eq!(
            plan.program
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .as_deref(),
            Some("cmd.exe")
        );
        assert_eq!(
            plan.args,
            os_args(["/D", "/K", r"C:\Users\RMUX User\shells\custom shell.cmd"])
        );
    }

    #[cfg(windows)]
    #[test]
    fn batch_default_shell_command_uses_cmd_c_wrapper() {
        let spec = ShellSpec::new(Path::new(r"C:\Users\RMUX User\shells\custom shell.bat"));
        let plan = spec.command_plan(Path::new(r"C:\tmp"), "echo RMUX_OK");

        assert_eq!(
            plan.program
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .as_deref(),
            Some("cmd.exe")
        );
        assert_eq!(plan.args, os_args(["/D", "/S", "/C"]));
        assert_eq!(
            plan.windows_verbatim_args.as_deref(),
            Some(
                OsString::from(
                    "\"\"C:\\Users\\RMUX User\\shells\\custom shell.bat\" \"echo RMUX_OK\"\""
                )
                .as_os_str()
            )
        );
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
