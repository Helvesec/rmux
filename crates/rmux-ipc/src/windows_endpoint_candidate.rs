//! Process-local candidates for pure Windows endpoint resolution.

#![cfg(windows)]

use std::collections::VecDeque;
use std::io;
use std::sync::{Mutex, OnceLock};

const MAX_CANDIDATES: usize = 256;
const NONCE_BYTES: usize = crate::windows_endpoint_record::NONCE_HEX_LEN / 2;

static CANDIDATES: OnceLock<Mutex<VecDeque<(String, String)>>> = OnceLock::new();

pub(crate) fn for_key(key: &str, rejected_nonce: Option<&str>) -> io::Result<String> {
    let candidates = CANDIDATES.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut candidates = candidates
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(index) = candidates
        .iter()
        .position(|(known_key, _)| known_key == key)
    {
        let (_, nonce) = &candidates[index];
        if rejected_nonce != Some(nonce.as_str()) {
            return Ok(nonce.clone());
        }
        candidates.remove(index);
    }

    let nonce = random_away_from(rejected_nonce)?;
    if candidates.len() == MAX_CANDIDATES {
        candidates.pop_front();
    }
    candidates.push_back((key.to_owned(), nonce.clone()));
    Ok(nonce)
}

fn random_away_from(rejected_nonce: Option<&str>) -> io::Result<String> {
    loop {
        let mut bytes = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|error| io::Error::other(format!("Windows endpoint RNG failed: {error}")))?;
        let nonce = encode_hex(&bytes);
        if rejected_nonce != Some(nonce.as_str()) {
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
