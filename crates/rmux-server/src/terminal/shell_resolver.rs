use std::collections::HashMap;
#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};

use rmux_core::OptionStore;
use rmux_proto::{OptionName, SessionName};
#[cfg(unix)]
use rustix::process::getuid;

#[cfg(unix)]
pub(super) fn resolve_shell_path(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
) -> PathBuf {
    session_name
        .and_then(|session_name| options.resolve(Some(session_name), OptionName::DefaultShell))
        .or_else(|| options.resolve(None, OptionName::DefaultShell))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(normalize_shell_path)
        .or_else(|| {
            environment
                .get("SHELL")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(current_user_login_shell)
        .map(normalize_shell_path)
        .unwrap_or_else(default_shell_path)
}

#[cfg(windows)]
pub(super) fn resolve_shell_path(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
) -> PathBuf {
    explicit_default_shell(options, session_name)
        .map(PathBuf::from)
        .map(|path| resolve_program_path(&path, environment))
        .unwrap_or_else(|| default_shell_path(environment))
}

#[cfg(windows)]
fn explicit_default_shell<'a>(
    options: &'a OptionStore,
    session_name: Option<&SessionName>,
) -> Option<&'a str> {
    session_name
        .and_then(|session_name| options.session_value(session_name, OptionName::DefaultShell))
        .or_else(|| options.global_value(OptionName::DefaultShell))
        .filter(|value| !value.is_empty())
}

#[cfg(unix)]
pub(super) fn normalize_shell_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(unix)]
pub(super) fn resolve_program_path(path: &Path, _environment: &HashMap<String, String>) -> PathBuf {
    path.to_path_buf()
}

#[cfg(windows)]
pub(super) fn normalize_shell_path(path: PathBuf) -> PathBuf {
    let environment = env::vars().collect::<HashMap<_, _>>();
    resolve_program_path(&path, &environment)
}

#[cfg(windows)]
pub(super) fn resolve_program_path(path: &Path, environment: &HashMap<String, String>) -> PathBuf {
    if path.components().count() > 1 {
        return path.to_path_buf();
    }

    find_program_on_path(path, environment).unwrap_or_else(|| path.to_path_buf())
}

#[cfg(windows)]
fn find_program_on_path(path: &Path, environment: &HashMap<String, String>) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if matches!(name.as_str(), "cmd" | "cmd.exe") {
        return cmd_shell_path(environment);
    }
    if matches!(name.as_str(), "powershell" | "powershell.exe") {
        return windows_powershell_path(environment);
    }

    search_path(path, environment)
}

#[cfg(windows)]
fn search_path(path: &Path, environment: &HashMap<String, String>) -> Option<PathBuf> {
    let path_value = environment_or_process_os_value(environment, "PATH")?;
    let pathext = environment_or_process_os_value(environment, "PATHEXT");
    search_path_in(path, path_value.as_os_str(), pathext.as_deref())
}

#[cfg(windows)]
fn search_path_in(path: &Path, path_value: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf> {
    let extensions = executable_extensions(path, pathext);
    for directory in env::split_paths(path_value) {
        for extension in &extensions {
            let candidate = directory.join(format!("{}{}", path.to_string_lossy(), extension));
            if candidate.is_file() && is_usable_shell_candidate(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn is_usable_shell_candidate(path: &Path) -> bool {
    // `%LOCALAPPDATA%\Microsoft\WindowsApps` entries are app-execution aliases;
    // launching them as ConPTY shells is unreliable. Packaged applications may
    // also live under `C:\Program Files\WindowsApps\...`; those are real package
    // executables and should remain eligible.
    !is_windowsapps_alias_candidate(path)
}

#[cfg(windows)]
fn is_windowsapps_alias_candidate(path: &Path) -> bool {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    components.windows(2).any(|window| {
        window[0].eq_ignore_ascii_case("Microsoft") && window[1].eq_ignore_ascii_case("WindowsApps")
    })
}

#[cfg(windows)]
fn executable_extensions(path: &Path, pathext: Option<&OsStr>) -> Vec<String> {
    if path.extension().is_some() {
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

#[cfg(unix)]
fn current_user_login_shell() -> Option<PathBuf> {
    let uid = getuid().as_raw();
    fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| passwd_shell_for_uid(line, uid))
}

#[cfg(unix)]
fn passwd_shell_for_uid(line: &str, uid: u32) -> Option<PathBuf> {
    let mut fields = line.split(':');
    let _name = fields.next()?;
    let _password = fields.next()?;
    let parsed_uid = fields.next()?.parse::<u32>().ok()?;
    let _gid = fields.next()?;
    let _gecos = fields.next()?;
    let _home = fields.next()?;
    let shell = fields.next()?;
    (parsed_uid == uid && !shell.is_empty()).then(|| PathBuf::from(shell))
}

#[cfg(unix)]
fn default_shell_path() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn default_shell_path(environment: &HashMap<String, String>) -> PathBuf {
    search_path(Path::new("pwsh.exe"), environment)
        .or_else(|| windows_powershell_path(environment))
        .or_else(|| cmd_shell_path(environment))
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}

#[cfg(windows)]
fn windows_powershell_path(environment: &HashMap<String, String>) -> Option<PathBuf> {
    environment_or_process_os_value(environment, "SystemRoot").map(|root| {
        PathBuf::from(root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    })
}

#[cfg(windows)]
fn cmd_shell_path(environment: &HashMap<String, String>) -> Option<PathBuf> {
    environment_or_process_os_value(environment, "COMSPEC")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            environment_or_process_os_value(environment, "SystemRoot")
                .map(|root| PathBuf::from(root).join("System32").join("cmd.exe"))
        })
}

#[cfg(windows)]
fn environment_or_process_os_value(
    environment: &HashMap<String, String>,
    name: &str,
) -> Option<OsString> {
    environment_os_value(environment, name).or_else(|| env::var_os(name))
}

#[cfg(windows)]
fn environment_os_value(environment: &HashMap<String, String>, name: &str) -> Option<OsString> {
    environment.get(name).map(OsString::from).or_else(|| {
        environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| OsString::from(value))
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;

    #[test]
    fn search_path_skips_windowsapps_alias_candidates() {
        let root = unique_test_dir("windowsapps-alias");
        let windows_apps = root.join("Microsoft").join("WindowsApps");
        let regular_bin = root.join("regular-bin");
        fs::create_dir_all(&windows_apps).expect("windowsapps test directory");
        fs::create_dir_all(&regular_bin).expect("regular test directory");
        fs::write(windows_apps.join("pwsh.exe"), b"").expect("windowsapps pwsh fixture");
        fs::write(regular_bin.join("pwsh.exe"), b"").expect("regular pwsh fixture");
        let path = env::join_paths([windows_apps.as_os_str(), regular_bin.as_os_str()])
            .expect("joined PATH");

        let resolved = search_path_in(
            Path::new("pwsh.exe"),
            path.as_os_str(),
            Some(OsStr::new(".EXE")),
        )
        .expect("regular pwsh should resolve");

        assert_eq!(resolved, regular_bin.join("pwsh.exe"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_path_rejects_only_windowsapps_alias_candidates() {
        let root = unique_test_dir("only-windowsapps");
        let windows_apps = root.join("Microsoft").join("WindowsApps");
        fs::create_dir_all(&windows_apps).expect("windowsapps test directory");
        fs::write(windows_apps.join("pwsh.exe"), b"").expect("windowsapps pwsh fixture");
        let path = env::join_paths([windows_apps.as_os_str()]).expect("joined PATH");

        let resolved = search_path_in(
            Path::new("pwsh.exe"),
            path.as_os_str(),
            Some(OsStr::new(".EXE")),
        );

        assert_eq!(resolved, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_path_allows_packaged_windowsapps_executables() {
        let root = unique_test_dir("packaged-windowsapps");
        let packaged = root
            .join("Program Files")
            .join("WindowsApps")
            .join("Microsoft.PowerShell_7.6.1.0_x64__8wekyb3d8bbwe");
        fs::create_dir_all(&packaged).expect("packaged windowsapps test directory");
        fs::write(packaged.join("pwsh.exe"), b"").expect("packaged pwsh fixture");
        let path = env::join_paths([packaged.as_os_str()]).expect("joined PATH");

        let resolved = search_path_in(
            Path::new("pwsh.exe"),
            path.as_os_str(),
            Some(OsStr::new(".EXE")),
        )
        .expect("packaged pwsh should resolve");

        assert_eq!(resolved, packaged.join("pwsh.exe"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_program_path_uses_effective_environment_path_case_insensitively() {
        let root = unique_test_dir("effective-path");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin test directory");
        fs::write(bin.join("rmux-probe.exe"), b"").expect("probe fixture");
        let path = env::join_paths([bin.as_os_str()]).expect("joined PATH");
        let environment = HashMap::from([
            ("Path".to_owned(), path.to_string_lossy().into_owned()),
            ("PATHEXT".to_owned(), ".EXE".to_owned()),
        ]);

        let resolved = resolve_program_path(Path::new("rmux-probe"), &environment);

        assert_eq!(resolved, bin.join("rmux-probe.EXE"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_default_shell_uses_effective_environment_path() {
        let root = unique_test_dir("default-shell-path");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin test directory");
        fs::write(bin.join("custom-shell.exe"), b"").expect("custom shell fixture");
        let path = env::join_paths([bin.as_os_str()]).expect("joined PATH");
        let environment = HashMap::from([
            ("PATH".to_owned(), path.to_string_lossy().into_owned()),
            ("PATHEXT".to_owned(), ".EXE".to_owned()),
        ]);
        let mut options = OptionStore::new();
        options
            .set(
                rmux_proto::ScopeSelector::Global,
                OptionName::DefaultShell,
                "custom-shell".to_owned(),
                rmux_proto::SetOptionMode::Replace,
            )
            .expect("default-shell set succeeds");

        let resolved = resolve_shell_path(&options, None, &environment);

        assert_eq!(resolved, bin.join("custom-shell.EXE"));
        let _ = fs::remove_dir_all(root);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "rmux-shell-resolver-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }
}
