//! Resolution of the `claude` program (and Git Bash on Windows).

#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use super::ExitFailure;
#[cfg(windows)]
use super::CLAUDE_GIT_BASH_PATH_ENV;

#[cfg(windows)]
pub(super) enum WindowsClaudeProgram {
    Executable(PathBuf),
    PowerShellScript(PathBuf),
}

#[cfg(windows)]
pub(super) fn claude_process_command() -> Result<ProcessCommand, ExitFailure> {
    match resolve_windows_claude_program()? {
        WindowsClaudeProgram::Executable(path) => Ok(ProcessCommand::new(path)),
        WindowsClaudeProgram::PowerShellScript(path) => {
            let mut command = ProcessCommand::new("powershell.exe");
            command
                .arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(path);
            Ok(command)
        }
    }
}

#[cfg(not(windows))]
pub(super) fn claude_process_command() -> Result<ProcessCommand, ExitFailure> {
    Ok(ProcessCommand::new("claude"))
}

#[cfg(windows)]
fn resolve_windows_claude_program() -> Result<WindowsClaudeProgram, ExitFailure> {
    resolve_windows_program("claude").ok_or_else(|| {
        ExitFailure::new(
            1,
            "rmux claude: failed to find claude on PATH; install Claude Code or set PATH",
        )
    })
}

#[cfg(windows)]
pub(super) fn windows_git_bash_path() -> Result<PathBuf, ExitFailure> {
    find_windows_git_bash_path().ok_or_else(|| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude on Windows requires Git Bash for teammate panes; install Git for Windows or set {CLAUDE_GIT_BASH_PATH_ENV}"
            ),
        )
    })
}

#[cfg(windows)]
fn find_windows_git_bash_path() -> Option<PathBuf> {
    env::var_os(CLAUDE_GIT_BASH_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            windows_known_git_bash_paths()
                .into_iter()
                .find(|path| path.is_file())
        })
        .or_else(|| match resolve_windows_program("bash") {
            Some(WindowsClaudeProgram::Executable(path)) if is_git_bash_path(&path) => Some(path),
            _ => None,
        })
}

#[cfg(windows)]
fn windows_known_git_bash_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files\Git\usr\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Git\usr\bin\bash.exe"),
    ]
}

#[cfg(windows)]
fn is_git_bash_path(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("bash.exe"))
        && path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("Git")
        })
}

#[cfg(windows)]
pub(super) fn resolve_windows_program(name: &str) -> Option<WindowsClaudeProgram> {
    let name_path = Path::new(name);
    let has_path_separator = name.contains('\\') || name.contains('/');
    let mut candidates = Vec::new();
    if has_path_separator || name_path.is_absolute() {
        candidates.extend(windows_program_candidates(name_path));
    } else {
        let path = env::var_os("PATH").unwrap_or_default();
        for dir in env::split_paths(&path) {
            candidates.extend(windows_program_candidates(&dir.join(name)));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .map(windows_program_kind)
}

#[cfg(windows)]
fn windows_program_candidates(path: &Path) -> Vec<PathBuf> {
    if path.extension().is_some() {
        return vec![path.to_path_buf()];
    }
    windows_pathexts()
        .into_iter()
        .map(|extension| path.with_extension(extension.trim_start_matches('.')))
        .collect()
}

#[cfg(windows)]
fn windows_pathexts() -> Vec<String> {
    env::var_os("PATHEXT")
        .map(|value| {
            env::split_paths(&value)
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| {
            [".COM", ".EXE", ".BAT", ".CMD", ".PS1"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
}

#[cfg(windows)]
fn windows_program_kind(path: PathBuf) -> WindowsClaudeProgram {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("ps1") => WindowsClaudeProgram::PowerShellScript(path),
        _ => WindowsClaudeProgram::Executable(path),
    }
}
