//! Build-only verification of a selected child program.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::process::Command;

/// Resolve and verify the program immediately before spawning or executing it.
pub fn resolve(program: &OsStr) -> io::Result<PathBuf> {
    let Some(mut verifier) = verifier()? else {
        return Ok(PathBuf::from(program));
    };
    let output = verifier
        .arg("check")
        .arg(program)
        .output()
        .map_err(|_| io::Error::other("candidate executable identity verifier unavailable"))?;
    if !output.status.success() {
        let kind = if output.status.code() == Some(89) {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::PermissionDenied
        };
        return Err(io::Error::new(
            kind,
            "candidate executable identity rejected",
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| io::Error::other("candidate executable identity path is not UTF-8"))?;
    let path = PathBuf::from(text.trim_end_matches(['\r', '\n']));
    if !path.is_absolute() {
        return Err(io::Error::other(
            "candidate executable identity path is not absolute",
        ));
    }
    Ok(path)
}

pub(super) fn verifier() -> io::Result<Option<Command>> {
    let Some(script) = option_env!("RMUX_IDENTITY_SCRIPT") else {
        return Ok(None);
    };
    let missing = || io::Error::other("candidate executable identity context missing");
    let python = option_env!("RMUX_IDENTITY_PYTHON").ok_or_else(missing)?;
    let source = option_env!("RMUX_IDENTITY_SOURCE").ok_or_else(missing)?;
    let head = option_env!("RMUX_IDENTITY_HEAD").ok_or_else(missing)?;
    let target = option_env!("RMUX_IDENTITY_TARGET").ok_or_else(missing)?;
    let mut command = Command::new(python);
    command
        .arg(script)
        .env("RMUX_IDENTITY_SOURCE", source)
        .env("RMUX_IDENTITY_HEAD", head)
        .env("RMUX_IDENTITY_TARGET", target)
        .env("CARGO_TARGET_DIR", target);
    Ok(Some(command))
}
#[cfg(all(test, unix))]
#[test]
fn retained_alias_selection_preserves_warm_producer_proof() {
    let status = Command::new(option_env!("RMUX_IDENTITY_PYTHON").unwrap_or("python3"))
        .args([
            "-m",
            "unittest",
            "scripts.test_candidate_build_identity.SelectedProgramStartup",
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .status()
        .expect("selected-program regression runner");
    assert!(status.success(), "selected-program regression failed");
}
