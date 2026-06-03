//! Handshake transcript hashing.
//!
//! The transcript binds the *exact wire bytes* of both handshake messages so
//! that any tampering by a relay (e.g. stripping a capability, downgrading a
//! version, or substituting a public key) changes the derived keys and is
//! therefore detected.

use sha2::{Digest, Sha256};

/// Wire-stable v1 domain-separation tag for the transcript hash.
const DOMAIN: &[u8] = b"rmux web-share v1 transcript";

/// Computes the handshake transcript hash.
///
/// The hash is:
///
/// ```text
/// SHA256( DOMAIN
///         || le_u64(client_hello.len()) || client_hello
///         || le_u64(server_challenge.len()) || server_challenge )
/// ```
///
/// where `DOMAIN` is the wire-stable v1 domain tag and the lengths are encoded
/// as little-endian `u64`. Length-prefixing makes the concatenation
/// unambiguous, so *every* field inside either message — versions,
/// capabilities, nonces, public keys — is cryptographically bound.
///
/// CRITICAL: this hashes the *exact bytes passed in*. Callers must pass the
/// precise bytes that appeared on the wire and must never re-serialize, since
/// a re-serialization could differ from what the peer actually saw.
pub fn transcript_hash(client_hello: &[u8], server_challenge: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update((client_hello.len() as u64).to_le_bytes());
    hasher.update(client_hello);
    hasher.update((server_challenge.len() as u64).to_le_bytes());
    hasher.update(server_challenge);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_literal::hex;

    #[test]
    fn hash_is_deterministic() {
        let a = transcript_hash(b"hello", b"challenge");
        let b = transcript_hash(b"hello", b"challenge");
        assert_eq!(a, b);
    }

    #[test]
    fn length_prefix_prevents_ambiguity() {
        // ("ab", "c") vs ("a", "bc") must differ thanks to length prefixing.
        let a = transcript_hash(b"ab", b"c");
        let b = transcript_hash(b"a", b"bc");
        assert_ne!(a, b);
    }

    #[test]
    fn any_byte_change_changes_hash() {
        let base = transcript_hash(b"client", b"server");
        let flipped = transcript_hash(b"clienX", b"server");
        assert_ne!(base, flipped);
    }

    #[test]
    fn wire_stable_transcript_hash_vector() {
        let client_hello = br#"{"type":"hello","protocol_version":1,"capabilities":["e2ee-token-auth"],"token_id":"tok","client_nonce":"nonce","client_public":"pub","client_ml_kem_ek":"ek"}"#;
        let server_challenge = br#"{"type":"challenge","protocol_version":1,"capabilities":["e2ee-token-auth"],"server_nonce":"nonce","server_public":"pub","server_ml_kem_ct":"ct"}"#;

        assert_eq!(
            transcript_hash(client_hello, server_challenge),
            hex!("c269da65fcd3ded338735b48f95fc833b8bff39146417043ae6a0aff8c90212c")
        );
    }
}
