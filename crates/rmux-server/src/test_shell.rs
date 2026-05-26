#[cfg(any(unix, windows))]
use std::path::Path;
#[cfg(unix)]
use std::sync::OnceLock;

/// Resolve a usable `bash` binary for tests at runtime.
///
/// Hardcoding `/bin/bash` breaks on systems where the canonical path is
/// elsewhere (NixOS puts bash in the user profile, FreeBSD in
/// `/usr/local/bin`, GitHub macOS runners in `/usr/local/bin/bash`, …).
/// Resolution order:
///   1. `RMUX_TEST_BASH` env var — explicit override for CI / dev.
///   2. `/bin/bash` if it exists — the historical default.
///   3. First `bash` found on `PATH` — what `/usr/bin/env bash` would
///      pick up in a shebang.
///   4. The literal string `"bash"` — last resort; relies on the spawn
///      path doing its own PATH lookup.
///
/// The resolution is cached after the first call so every test in a
/// run agrees on the same interpreter.
#[cfg(unix)]
pub(crate) fn test_bash_path() -> String {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            if let Ok(value) = std::env::var("RMUX_TEST_BASH") {
                if !value.is_empty() {
                    return value;
                }
            }
            if Path::new("/bin/bash").is_file() {
                return "/bin/bash".to_owned();
            }
            if let Some(path_var) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_var) {
                    let candidate = dir.join("bash");
                    if candidate.is_file() {
                        return candidate.to_string_lossy().into_owned();
                    }
                }
            }
            "bash".to_owned()
        })
        .clone()
}

#[cfg(any(unix, windows))]
pub(crate) fn command_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(unix)]
pub(crate) fn sh_quote(value: &str) -> String {
    command_quote(value)
}

#[cfg(unix)]
pub(crate) fn sh_quote_path(path: &Path) -> String {
    sh_quote(&path.display().to_string())
}

#[cfg(any(unix, windows))]
pub(crate) fn stdin_discard_command() -> String {
    platform_stdin_discard_command()
}

#[cfg(unix)]
fn platform_stdin_discard_command() -> String {
    "cat >/dev/null".to_owned()
}

#[cfg(windows)]
fn platform_stdin_discard_command() -> String {
    powershell_encoded_command(
        "$inputStream=[Console]::OpenStandardInput(); $inputStream.CopyTo([System.IO.Stream]::Null)",
    )
}

#[cfg(windows)]
pub(crate) fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
pub(crate) fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.display().to_string())
}

#[cfg(windows)]
pub(crate) fn powershell_encoded_command(script: &str) -> String {
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        base64_encode(&bytes)
    )
}

#[cfg(windows)]
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let value = ((first as u32) << 16) | ((second as u32) << 8) | third as u32;
        encoded.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(value & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}
