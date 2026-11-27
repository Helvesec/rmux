use std::collections::HashMap;
#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs;
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rmux_core::OptionStore;
#[cfg(windows)]
use rmux_os::shell::is_usable_shell_candidate;
use rmux_proto::{OptionName, SessionName};
#[cfg(unix)]
use rustix::fs::{access, Access};
#[cfg(unix)]
use rustix::process::getuid;

#[cfg(windows)]
pub(super) const CLIENT_SHELL_ENV: &str = "RMUX_CLIENT_SHELL";

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
        .filter(|path| is_suitable_shell(path))
        .map(normalize_shell_path)
        .or_else(|| {
            environment
                .get("SHELL")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| is_suitable_shell(path))
        })
        .or_else(|| current_user_login_shell().filter(|path| is_suitable_shell(path)))
        .map(normalize_shell_path)
        .unwrap_or_else(default_shell_path)
}

#[cfg(unix)]
pub(crate) fn is_suitable_shell(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && access(path, Access::EXEC_OK).is_ok()
}

#[cfg(unix)]
pub(super) fn resolve_profile_shell(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
    _current_dir: &Path,
) -> PathBuf {
    resolve_shell_path(options, session_name, environment)
}

#[cfg(windows)]
pub(super) fn resolve_profile_shell(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
    current_dir: &Path,
) -> PathBuf {
    resolve_windows_shell_path(options, session_name, environment, Some(current_dir))
}

#[cfg(windows)]
pub(super) fn resolve_shell_path(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
) -> PathBuf {
    resolve_windows_shell_path(options, session_name, environment, None)
}

#[cfg(windows)]
fn resolve_windows_shell_path(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
    current_dir: Option<&Path>,
) -> PathBuf {
    let shell = explicit_default_shell(options, session_name)
        .map(PathBuf::from)
        .map(|path| resolve_program_path_with_base(&path, environment, current_dir))
        .filter(|path| path.is_file())
        .or_else(|| inherited_client_shell(environment))
        .unwrap_or_else(|| default_shell_path(environment));
    normalize_windows_shell_execution_path(shell)
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
pub(super) fn resolve_program_path_from(
    path: &Path,
    _environment: &HashMap<String, String>,
    _current_dir: &Path,
) -> PathBuf {
    path.to_path_buf()
}

#[cfg(windows)]
pub(super) fn normalize_shell_path(path: PathBuf) -> PathBuf {
    let environment = env::vars().collect::<HashMap<_, _>>();
    resolve_program_path(&path, &environment)
}

#[cfg(windows)]
fn normalize_windows_shell_execution_path(path: PathBuf) -> PathBuf {
    const FORWARD_SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    if !path
        .as_os_str()
        .encode_wide()
        .any(|unit| unit == FORWARD_SLASH)
    {
        return path;
    }

    let units = path
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == FORWARD_SLASH {
                BACKSLASH
            } else {
                unit
            }
        })
        .collect::<Vec<_>>();
    PathBuf::from(OsString::from_wide(&units))
}

#[cfg(windows)]
pub(super) fn resolve_program_path(path: &Path, environment: &HashMap<String, String>) -> PathBuf {
    resolve_program_path_with_base(path, environment, None)
}

#[cfg(windows)]
pub(super) fn resolve_program_path_from(
    path: &Path,
    environment: &HashMap<String, String>,
    current_dir: &Path,
) -> PathBuf {
    resolve_program_path_with_base(path, environment, Some(current_dir))
}

#[cfg(windows)]
fn resolve_program_path_with_base(
    path: &Path,
    environment: &HashMap<String, String>,
    current_dir: Option<&Path>,
) -> PathBuf {
    if path.is_absolute() || has_path_component(path) {
        let base = current_dir
            .map(Path::to_path_buf)
            .or_else(|| env::current_dir().ok());
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(base) = base {
            base.join(path)
        } else {
            path.to_path_buf()
        };
        let pathext = environment_or_process_os_value(environment, "PATHEXT");
        return resolve_program_candidate(&candidate, pathext.as_deref())
            .unwrap_or_else(|| path.to_path_buf());
    }

    find_program_on_path(path, environment, current_dir).unwrap_or_else(|| path.to_path_buf())
}

#[cfg(windows)]
fn has_path_component(path: &Path) -> bool {
    path.parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
}

#[cfg(windows)]
fn find_program_on_path(
    path: &Path,
    environment: &HashMap<String, String>,
    current_dir: Option<&Path>,
) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if matches!(name.as_str(), "cmd" | "cmd.exe") {
        return cmd_shell_path(environment);
    }
    if matches!(name.as_str(), "powershell" | "powershell.exe") {
        return windows_powershell_path(environment);
    }

    search_path(path, environment, current_dir)
}

#[cfg(windows)]
fn inherited_client_shell(environment: &HashMap<String, String>) -> Option<PathBuf> {
    let shell = environment_string_value(environment, CLIENT_SHELL_ENV)?;
    let shell = shell.trim();
    if shell.is_empty() {
        return None;
    }
    let requested = Path::new(shell);
    let resolved = resolve_program_path(requested, environment);
    if requested.components().count() == 1 && resolved == requested {
        return None;
    }
    (resolved.is_file() && is_usable_shell_candidate(&resolved)).then_some(resolved)
}

#[cfg(windows)]
fn search_path(
    path: &Path,
    environment: &HashMap<String, String>,
    current_dir: Option<&Path>,
) -> Option<PathBuf> {
    let path_value = environment_or_process_os_value(environment, "PATH")?;
    let pathext = environment_or_process_os_value(environment, "PATHEXT");
    search_path_in_from(
        path,
        path_value.as_os_str(),
        pathext.as_deref(),
        current_dir,
    )
}

#[cfg(windows)]
fn search_path_in(path: &Path, path_value: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf> {
    search_path_in_from(path, path_value, pathext, None)
}

#[cfg(windows)]
fn search_path_in_from(
    path: &Path,
    path_value: &OsStr,
    pathext: Option<&OsStr>,
    current_dir: Option<&Path>,
) -> Option<PathBuf> {
    for directory in env::split_paths(path_value) {
        let directory = if directory.is_absolute() {
            directory
        } else if let Some(current_dir) = current_dir {
            current_dir.join(directory)
        } else {
            directory
        };
        if let Some(candidate) = resolve_program_candidate(&directory.join(path), pathext) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(windows)]
fn resolve_program_candidate(path: &Path, pathext: Option<&OsStr>) -> Option<PathBuf> {
    executable_extensions(path, pathext)
        .into_iter()
        .map(|extension| append_extension(path, &extension))
        .find(|candidate| candidate.is_file() && is_usable_shell_candidate(candidate))
}

#[cfg(windows)]
fn executable_extensions(path: &Path, pathext: Option<&OsStr>) -> Vec<OsString> {
    if path.extension().is_some() {
        return vec![OsString::new()];
    }

    let mut extensions = vec![OsString::new()];
    extensions.extend(
        pathext
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .filter(|extension| !extension.is_empty())
                    .map(|extension| {
                        if extension.starts_with('.') {
                            OsString::from(extension)
                        } else {
                            OsString::from(format!(".{extension}"))
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|extensions| !extensions.is_empty())
            .unwrap_or_else(|| {
                [".COM", ".EXE", ".BAT", ".CMD"]
                    .into_iter()
                    .map(OsString::from)
                    .collect()
            }),
    );
    extensions
}

#[cfg(windows)]
fn append_extension(path: &Path, extension: &OsStr) -> PathBuf {
    let mut candidate = path.as_os_str().to_owned();
    candidate.push(extension);
    PathBuf::from(candidate)
}

#[cfg(unix)]
fn current_user_login_shell() -> Option<PathBuf> {
    rmux_os::identity::IdentityResolver::unix_login_shell_by_uid(getuid().as_raw())
        .ok()
        .flatten()
}

#[cfg(unix)]
fn default_shell_path() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn default_shell_path(environment: &HashMap<String, String>) -> PathBuf {
    cmd_shell_path(environment)
        .or_else(|| windows_powershell_path(environment))
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
pub(super) fn cmd_shell_path(environment: &HashMap<String, String>) -> Option<PathBuf> {
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

#[cfg(windows)]
fn environment_string_value<'a>(
    environment: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    environment.get(name).map(String::as_str).or_else(|| {
        environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    })
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;

    #[test]
    fn invalid_shell_environment_falls_back_to_a_suitable_login_shell() {
        let environment = HashMap::from([(
            "SHELL".to_owned(),
            "/definitely/missing/rmux-shell".to_owned(),
        )]);

        let resolved = resolve_shell_path(&OptionStore::new(), None, &environment);

        assert_ne!(resolved, PathBuf::from(&environment["SHELL"]));
        assert!(is_suitable_shell(&resolved), "{resolved:?}");
    }

    #[test]
    fn absent_shell_environment_uses_the_account_database_login_shell() {
        let expected = account_database_login_shell();
        assert!(is_suitable_shell(&expected), "{expected:?}");

        let resolved = resolve_shell_path(&OptionStore::new(), None, &HashMap::new());

        assert_eq!(resolved, normalize_shell_path(expected));
    }

    /// Reads the current account's login shell through the platform account
    /// database tool, independently of RMUX's own lookup. macOS accounts live
    /// in Directory Services rather than `/etc/passwd`.
    fn account_database_login_shell() -> PathBuf {
        let uid = getuid().as_raw().to_string();
        let record = [
            ("/usr/bin/getent", vec!["passwd", uid.as_str()]),
            ("/usr/bin/id", vec!["-P", uid.as_str()]),
        ]
        .into_iter()
        .filter(|(program, _)| Path::new(program).is_file())
        .find_map(|(program, arguments)| {
            let output = std::process::Command::new(program)
                .args(arguments)
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .expect("getent or id reports the current account record");
        let shell = record
            .rsplit(':')
            .next()
            .expect("account record has a shell field");
        PathBuf::from(shell)
    }

    #[test]
    fn invalid_stored_default_shell_falls_back_to_the_session_environment() {
        let mut options = OptionStore::new();
        options
            .set(
                rmux_proto::ScopeSelector::Global,
                OptionName::DefaultShell,
                "/definitely/missing/rmux-shell".to_owned(),
                rmux_proto::SetOptionMode::Replace,
            )
            .expect("core option storage accepts string values");
        let environment = HashMap::from([("SHELL".to_owned(), "/bin/sh".to_owned())]);

        assert_eq!(
            resolve_shell_path(&options, None, &environment),
            PathBuf::from("/bin/sh")
        );
    }
}

#[cfg(all(test, windows))]
#[path = "shell_resolver/windows_tests.rs"]
mod tests;
