use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn command() -> Command {
    let selected = std::env::var_os("RMUX_PYTHON");
    // Preserve Unix's existing override-or-python3 contract.
    #[cfg(not(windows))]
    return Command::new(selected.unwrap_or_else(|| "python3".into()));

    #[cfg(windows)]
    {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let directories: Vec<_> = std::env::split_paths(&path).collect();
        let python = select_windows(selected.as_deref(), &directories, probe)
            .unwrap_or_else(|error| panic!("queue inventory requires Python 3: {error}"));
        Command::new(python)
    }
}

fn select_windows(
    selected: Option<&OsStr>,
    directories: &[PathBuf],
    mut probe: impl FnMut(&Path, &[&str]) -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    // An explicit choice is authoritative: never hide a broken override.
    if let Some(selected) = selected {
        return probe(Path::new(selected), &[])
            .map_err(|error| format!("RMUX_PYTHON {selected:?}: {error}"));
    }

    let mut failures = Vec::new();
    for (name, arguments) in [
        ("python3.exe", &[][..]),
        ("python.exe", &[][..]),
        ("py.exe", &["-3"][..]),
    ] {
        for directory in directories {
            let candidate = directory.join(name);
            // App-execution aliases can fail with Access Denied in SSH, or
            // open the Store on the desktop. Do not exclude real packaged
            // installations under Program Files/WindowsApps.
            if is_app_alias(&candidate) {
                continue;
            }
            match probe(&candidate, arguments) {
                Ok(python) => return Ok(python),
                Err(error) => failures.push(format!("{}: {error}", candidate.display())),
            }
        }
    }
    Err(format!(
        "set RMUX_PYTHON to a working interpreter; candidates: {}",
        failures.join("; ")
    ))
}

fn is_app_alias(path: &Path) -> bool {
    let mut microsoft = false;
    for component in path.components() {
        let name = component.as_os_str();
        if microsoft && name.eq_ignore_ascii_case("WindowsApps") {
            return true;
        }
        microsoft = name.eq_ignore_ascii_case("Microsoft");
    }
    false
}

fn probe(candidate: &Path, arguments: &[&str]) -> Result<PathBuf, String> {
    let output = Command::new(candidate)
        .args(arguments)
        .args([
            "-c",
            "import json, sys; print(json.dumps(sys.executable)); raise SystemExit(0 if sys.version_info.major == 3 else 1)",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let python: PathBuf = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    if !python.is_absolute() {
        return Err(format!("interpreter returned a relative path: {python:?}"));
    }
    // Pin sys.executable, including when discovery used the py launcher.
    Ok(python)
}

#[cfg(test)]
#[path = "python_tests.rs"]
mod tests;
