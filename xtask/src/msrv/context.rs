//! Compiler configuration is unsupported; the repository task alias is inert.

use super::{invalid, Error};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

fn same_key(key: &OsStr, name: &str) -> bool {
    if cfg!(windows) {
        key.as_encoded_bytes().eq_ignore_ascii_case(name.as_bytes())
    } else {
        key == name
    }
}

fn variable(command: &Command, name: &str) -> Option<OsString> {
    match command.get_envs().find(|(key, _)| same_key(key, name)) {
        Some((_, value)) => value.map(OsStr::to_os_string),
        None => env::var_os(name),
    }
}

fn inspect_error(path: &Path, error: std::io::Error) -> Error {
    invalid(format!(
        "cannot inspect Cargo configuration {}: {error}",
        path.display()
    ))
}

fn directory(path: &Path) -> Result<(), Error> {
    if !fs::metadata(path)
        .map_err(|e| inspect_error(path, e))?
        .is_dir()
    {
        return Err(invalid(format!(
            "Cargo configuration directory is not a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn unconfigured(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(inspect_error(path, error)),
        Ok(_) => directory(path)?, // Detect dangling directory links and non-directories.
    }
    for name in ["config", "config.toml"] {
        let file = path.join(name);
        match fs::symlink_metadata(&file) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(inspect_error(&file, error)),
            Ok(metadata) if metadata.is_file() && task_alias_only(&file) => {}
            Ok(_) => return Err(invalid(format!(
                "unsupported Cargo configuration: {}; use an unconfigured cwd/ancestry and Cargo home",
                file.display()
            ))),
        }
    }
    Ok(())
}

fn task_alias_only(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(actual) = text.parse::<toml::Value>() else {
        return false;
    };
    let Some(table) = actual.as_table() else {
        return false;
    };
    let Some(alias) = table.get("alias").and_then(toml::Value::as_table) else {
        return false;
    };
    table.len() == 1
        && alias.len() == 1
        && alias.get("xtask").and_then(toml::Value::as_str) == Some("run --locked -p xtask --")
}

pub(super) fn check(command: &Command) -> Result<(), Error> {
    for name in [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_BOOTSTRAP",
    ] {
        // Reject an unsupported parent even if a command tried to hide it.
        if env::var_os(name).is_some_and(|v| !v.is_empty())
            || variable(command, name).is_some_and(|v| !v.is_empty())
        {
            return Err(invalid(format!(
                "{name} would invalidate exact compiler proof"
            )));
        }
    }
    let home = variable(command, "CARGO_HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid("msrv requires an explicit absolute CARGO_HOME directory"))?;
    let home = Path::new(&home);
    if !home.is_absolute() {
        return Err(invalid(
            "msrv requires an explicit absolute CARGO_HOME directory",
        ));
    }
    directory(home)?;
    unconfigured(home)?;
    let cwd = match command.get_current_dir() {
        Some(path) => path.to_owned(),
        None => env::current_dir()?,
    };
    // Windows Cargo retains junction spelling; Unix cwd resolves directory links.
    #[cfg(windows)]
    let cwd = std::path::absolute(cwd)?;
    #[cfg(windows)]
    directory(&cwd)?;
    #[cfg(not(windows))]
    let cwd = fs::canonicalize(cwd)?;
    for ancestor in cwd.ancestors() {
        unconfigured(&ancestor.join(".cargo"))?;
    }
    Ok(())
}
