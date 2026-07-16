use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rmux_os::shell::is_usable_shell_candidate;

#[derive(Debug, Clone)]
pub(crate) struct WindowsShellEnvironment {
    path: Option<OsString>,
    system_root: Option<OsString>,
    comspec: Option<OsString>,
}

impl WindowsShellEnvironment {
    pub(crate) fn current() -> Self {
        Self {
            path: env::var_os("PATH"),
            system_root: env::var_os("SystemRoot"),
            comspec: env::var_os("COMSPEC"),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        path: Option<OsString>,
        system_root: Option<OsString>,
        comspec: Option<OsString>,
    ) -> Self {
        Self {
            path,
            system_root,
            comspec,
        }
    }
}

pub(crate) fn client_shell_for_parent_name(
    parent_name: &str,
    environment: &WindowsShellEnvironment,
) -> Option<String> {
    let lower = parent_name.to_ascii_lowercase();
    match lower.as_str() {
        "cmd.exe" | "cmd" => Some(cmd_shell_hint(environment)),
        "powershell.exe" | "powershell" | "pwsh.exe" | "pwsh" => {
            Some(powershell_shell_hint(environment))
        }
        "bash.exe" | "bash" | "sh.exe" | "sh" | "zsh.exe" | "zsh" | "nu.exe" | "nu" => {
            Some(parent_name.to_owned())
        }
        _ => None,
    }
}

fn powershell_shell_hint(environment: &WindowsShellEnvironment) -> String {
    if find_usable_pwsh_on_path(environment).is_some() {
        return "pwsh.exe".to_owned();
    }
    if windows_powershell_path(environment).is_some_and(|path| is_existing_candidate(&path)) {
        return "powershell.exe".to_owned();
    }
    cmd_shell_hint(environment)
}

fn find_usable_pwsh_on_path(environment: &WindowsShellEnvironment) -> Option<PathBuf> {
    let path_value = environment.path.as_deref()?;
    env::split_paths(path_value)
        .map(|directory| directory.join("pwsh.exe"))
        .find(|candidate| is_existing_candidate(candidate))
}

fn windows_powershell_path(environment: &WindowsShellEnvironment) -> Option<PathBuf> {
    environment.system_root.as_ref().map(|root| {
        PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    })
}

fn cmd_shell_hint(environment: &WindowsShellEnvironment) -> String {
    environment
        .comspec
        .as_ref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| is_existing_candidate(path))
        .or_else(|| {
            environment
                .system_root
                .as_ref()
                .map(|root| PathBuf::from(root).join("System32").join("cmd.exe"))
                .filter(|path| is_existing_candidate(path))
        })
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cmd.exe".to_owned())
}

fn is_existing_candidate(path: &Path) -> bool {
    path.is_file() && is_usable_shell_candidate(path)
}
