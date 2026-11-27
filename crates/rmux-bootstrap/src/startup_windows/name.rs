use std::ffi::OsString;
use std::path::Path;

use rmux_ipc::MAX_NAMED_MUTEX_LEN;

use super::{StartupError, PIPE_PREFIX, STARTUP_MUTEX_PREFIX};

pub(super) fn validate_pipe_name(pipe_name: &Path) -> Result<(), StartupError> {
    let value = pipe_name.as_os_str();
    if value.is_empty() {
        return Err(StartupError::InvalidPipeName {
            reason: "pipe name was empty".into(),
            pipe_name: pipe_name.to_path_buf(),
        });
    }
    let display = value.to_string_lossy();
    if !display
        .get(..PIPE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PIPE_PREFIX))
    {
        return Err(StartupError::InvalidPipeName {
            reason: format!("pipe name must start with {PIPE_PREFIX:?}"),
            pipe_name: pipe_name.to_path_buf(),
        });
    }
    if pipe_name.file_name().is_none() {
        return Err(StartupError::InvalidPipeName {
            reason: "pipe name has no label component".into(),
            pipe_name: pipe_name.to_path_buf(),
        });
    }
    Ok(())
}

pub(super) fn startup_mutex_name(pipe_name: &Path) -> Result<OsString, StartupError> {
    let display = pipe_name.as_os_str().to_string_lossy();
    if !display
        .get(..PIPE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PIPE_PREFIX))
    {
        return Err(StartupError::InvalidPipeName {
            reason: format!("pipe name must start with {PIPE_PREFIX:?}"),
            pipe_name: pipe_name.to_path_buf(),
        });
    }

    // Win32 pipe names are case-insensitive but kernel mutex names are case-
    // sensitive in the default object namespace. Lowercase the entire derived
    // label so two callers using differently-cased pipe paths always derive
    // the same mutex name and actually serialize against each other. The SID
    // and integrity prefix remain unique once lowercased because no two
    // distinct identities collapse together under ASCII lowercasing.
    let label_lower = display[PIPE_PREFIX.len()..].to_ascii_lowercase();
    if label_lower.is_empty() {
        return Err(StartupError::InvalidPipeName {
            reason: "pipe name has no label component".into(),
            pipe_name: pipe_name.to_path_buf(),
        });
    }

    let candidate = format!("{STARTUP_MUTEX_PREFIX}{label_lower}");
    if candidate.len() > MAX_NAMED_MUTEX_LEN {
        return Err(StartupError::InvalidMutexName {
            reason: format!(
                "derived mutex name length {} exceeds {MAX_NAMED_MUTEX_LEN}",
                candidate.len()
            ),
            pipe_name: pipe_name.to_path_buf(),
        });
    }

    Ok(OsString::from(candidate))
}

#[cfg(test)]
pub(super) fn test_pipe(label: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(r"\\.\pipe\rmux-S-1-5-21-1000-il-medium-{label}"))
}
