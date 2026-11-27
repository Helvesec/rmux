use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use rmux_os::shell::is_usable_shell_candidate;

pub(super) fn detected_pane_shell() -> String {
    find_command_on_path(Path::new("pwsh.exe"))
        .or_else(windows_powershell_path)
        .or_else(cmd_shell_path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cmd.exe".to_owned())
}

fn find_command_on_path(command: &Path) -> Option<PathBuf> {
    let path_value = std::env::var_os("PATH")?;
    let pathext = std::env::var_os("PATHEXT");
    find_command_on_path_in(command, path_value.as_os_str(), pathext.as_deref())
}

fn find_command_on_path_in(
    command: &Path,
    path_value: &OsStr,
    pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    let extensions = executable_extensions(command, pathext);
    for directory in std::env::split_paths(path_value) {
        for extension in &extensions {
            let candidate = directory.join(format!("{}{}", command.to_string_lossy(), extension));
            if candidate.is_file() && is_usable_shell_candidate(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn windows_powershell_path() -> Option<PathBuf> {
    std::env::var_os("SystemRoot").map(|root| {
        PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    })
}

fn cmd_shell_path() -> Option<PathBuf> {
    std::env::var_os("COMSPEC").map(PathBuf::from).or_else(|| {
        std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("cmd.exe"))
    })
}

fn executable_extensions(command: &Path, pathext: Option<&OsStr>) -> Vec<String> {
    if command.extension().is_some() {
        return vec![String::new()];
    }

    pathext
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| vec![".COM".to_owned(), ".EXE".to_owned(), ".BAT".to_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        name: &'static str,
        value: Option<OsString>,
    }

    impl EnvVarGuard {
        fn capture(name: &'static str) -> Self {
            Self {
                name,
                value: std::env::var_os(name),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.value.as_ref() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn detected_shell_reports_the_windows_default_pane_shell() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let _path = EnvVarGuard::capture("PATH");
        let _pathext = EnvVarGuard::capture("PATHEXT");
        let _comspec = EnvVarGuard::capture("COMSPEC");
        let root = unique_test_dir("shell");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp shell dir");
        let pwsh = root.join("pwsh.exe");
        std::fs::write(&pwsh, b"").expect("fake pwsh");
        std::env::set_var("PATH", &root);
        std::env::set_var("PATHEXT", ".EXE");
        std::env::set_var("COMSPEC", r"C:\Windows\System32\cmd.exe");

        assert_eq!(detected_pane_shell(), pwsh.to_string_lossy());

        std::fs::remove_dir_all(root).expect("remove temp shell dir");
    }

    #[test]
    fn windowsapps_shell_candidate_is_not_reported_as_usable() {
        assert!(!is_usable_shell_candidate(Path::new(
            r"C:\Users\RMUXUser\AppData\Local\Microsoft\WindowsApps\pwsh.exe"
        )));
        assert!(is_usable_shell_candidate(Path::new(
            r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7_x64__8wekyb3d8bbwe\pwsh.exe"
        )));
        assert!(is_usable_shell_candidate(Path::new(
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        )));
    }

    #[test]
    fn shell_path_lookup_uses_pathext_and_skips_windowsapps() {
        let root = unique_test_dir("pathext");
        let windows_apps = root.join("Microsoft").join("WindowsApps");
        let regular_bin = root.join("regular-bin");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&windows_apps).expect("windowsapps temp dir");
        std::fs::create_dir_all(&regular_bin).expect("regular temp dir");
        std::fs::write(windows_apps.join("demo.EXE"), b"").expect("fake windowsapps shell");
        std::fs::write(regular_bin.join("demo.EXE"), b"").expect("fake regular shell");
        let path = std::env::join_paths([windows_apps.as_os_str(), regular_bin.as_os_str()])
            .expect("joined PATH");

        let resolved = find_command_on_path_in(
            Path::new("demo"),
            path.as_os_str(),
            Some(OsStr::new(".EXE")),
        )
        .expect("regular shell should resolve");

        assert_eq!(resolved, regular_bin.join("demo.EXE"));

        std::fs::remove_dir_all(root).expect("remove temp shell dir");
    }

    #[test]
    fn cmd_shell_falls_back_to_systemroot_when_comspec_is_missing() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let _comspec = EnvVarGuard::capture("COMSPEC");
        let _systemroot = EnvVarGuard::capture("SystemRoot");
        std::env::remove_var("COMSPEC");
        std::env::set_var("SystemRoot", r"C:\Windows");

        assert_eq!(
            cmd_shell_path(),
            Some(PathBuf::from(r"C:\Windows\System32\cmd.exe"))
        );
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rmux-diagnose-{label}-{}", std::process::id()))
    }
}
