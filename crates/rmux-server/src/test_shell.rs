#[cfg(any(unix, windows))]
use std::path::Path;

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
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        encode_powershell_script(script)
    )
}

#[cfg(windows)]
fn encode_powershell_script(script: &str) -> String {
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64_encode(&bytes)
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

/// The real-child final-sink harness.
///
/// Split out of this module because it carries a slot protocol, two child
/// scripts and their diagnostics, none of which the shell-quoting helpers
/// above need to know about.
#[cfg(any(unix, windows))]
pub(crate) mod final_sink;
