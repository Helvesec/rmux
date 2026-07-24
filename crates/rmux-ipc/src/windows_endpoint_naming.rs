//! Windows managed-endpoint hashing, paths, and nonces.

#![cfg(windows)]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::endpoint::PIPE_PREFIX;
use crate::windows_endpoint_record::NONCE_HEX_LEN;

const MANAGED_PIPE_MARKER: &str = "-g-";

pub(crate) fn endpoint_key(label: &OsStr, sid: &str, integrity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"rmux windows endpoint label v1\0");
    hash.update((sid.len() as u64).to_le_bytes());
    hash.update(sid.as_bytes());
    hash.update((integrity.len() as u64).to_le_bytes());
    hash.update(integrity.as_bytes());
    let label_units = label.encode_wide().collect::<Vec<_>>();
    hash.update((label_units.len() as u64).to_le_bytes());
    for unit in label_units {
        hash.update(unit.to_le_bytes());
    }
    encode_hex(hash.finalize().as_slice())
}

pub(crate) fn pipe_path(sid: &str, integrity: &str, key: &str, nonce: &str) -> PathBuf {
    PathBuf::from(format!(
        "{PIPE_PREFIX}rmux-{sid}-il-{integrity}{MANAGED_PIPE_MARKER}{key}-{nonce}"
    ))
}

pub(crate) fn random_nonce() -> io::Result<String> {
    let mut nonce = [0_u8; NONCE_HEX_LEN / 2];
    getrandom::fill(&mut nonce)
        .map_err(|error| io::Error::other(format!("Windows endpoint RNG failed: {error}")))?;
    Ok(encode_hex(&nonce))
}

pub(crate) fn random_nonce_away_from(rejected: Option<&str>) -> io::Result<String> {
    loop {
        let nonce = random_nonce()?;
        if rejected != Some(nonce.as_str()) {
            return Ok(nonce);
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
