//! Key schedule: derives directional AEAD keys and nonce prefixes from the
//! pre-shared secret, the ephemeral DH shared secret, and the handshake
//! transcript using HKDF-SHA256.

use alloc::vec::Vec;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Error;
use crate::record::{RecordOpener, RecordSealer};
use crate::transcript::transcript_hash;

/// Wire-stable v1 HKDF info label for the client-to-server key.
const LABEL_KEY_C2S: &[u8] = b"rmux web-share v1 key c2s";
/// Wire-stable v1 HKDF info label for the server-to-client key.
const LABEL_KEY_S2C: &[u8] = b"rmux web-share v1 key s2c";
/// Wire-stable v1 HKDF info label for the client-to-server nonce prefix.
const LABEL_NONCE_C2S: &[u8] = b"rmux web-share v1 nonce c2s";
/// Wire-stable v1 HKDF info label for the server-to-client nonce prefix.
const LABEL_NONCE_S2C: &[u8] = b"rmux web-share v1 nonce s2c";

/// Derived per-direction secrets for one session.
///
/// The raw key material is zeroized on drop. Once the ciphers have been built
/// via [`SessionKeys::into_client`] or [`SessionKeys::into_server`], the raw
/// arrays inside are no longer needed and are wiped.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    /// Client-to-server AEAD key.
    c2s_key: [u8; 32],
    /// Server-to-client AEAD key.
    s2c_key: [u8; 32],
    /// Client-to-server nonce prefix.
    c2s_nonce_prefix: [u8; 4],
    /// Server-to-client nonce prefix.
    s2c_nonce_prefix: [u8; 4],
}

/// Derives [`SessionKeys`] for a session.
///
/// Inputs:
/// - `psk`: the high-entropy pre-shared secret (see the crate-level security
///   invariant — this MUST NOT be a low-entropy PIN or password).
/// - `dh_shared_secret`: the externally-computed ephemeral X25519 DH shared secret.
/// - `ml_kem_shared_secret`: the ML-KEM-768 shared secret. The v1 handshake is
///   hybrid by construction; the channel stays secure as long as *either* the DH
///   or the ML-KEM secret holds.
/// - `client_hello` / `server_challenge`: the exact wire bytes of the two
///   handshake messages, bound via the transcript hash. They carry the ML-KEM
///   encapsulation key and ciphertext, so the transcript binds them automatically
///   (giving ciphertext binding for the hybrid).
///
/// Construction:
/// 1. The all-zero DH secret is rejected in constant time
///    ([`Error::WeakSharedSecret`]).
/// 2. `transcript = transcript_hash(client_hello, server_challenge)`.
/// 3. `ikm = dh_shared_secret (32) || ml_kem_shared_secret (32) || psk`.
/// 4. `hk = HKDF-SHA256(salt = transcript, ikm)`.
/// 5. Four labels are expanded for the two keys and two nonce prefixes.
pub fn derive(
    psk: &[u8],
    dh_shared_secret: &[u8; 32],
    ml_kem_shared_secret: &[u8; 32],
    client_hello: &[u8],
    server_challenge: &[u8],
) -> Result<SessionKeys, Error> {
    // 1. Constant-time rejection of an all-zero DH shared secret (RFC 7748).
    let zero = [0u8; 32];
    if bool::from(dh_shared_secret.ct_eq(&zero)) {
        return Err(Error::WeakSharedSecret);
    }

    // 2. Bind the full handshake transcript.
    let transcript = transcript_hash(client_hello, server_challenge);

    // 3. ikm = dh (32) || ml_kem (32) || psk. Each shared secret is a fixed
    //    32-byte prefix, so the concatenation is unambiguous without length tags.
    let mut ikm = Vec::with_capacity(32 + 32 + psk.len());
    ikm.extend_from_slice(dh_shared_secret);
    ikm.extend_from_slice(ml_kem_shared_secret);
    ikm.extend_from_slice(psk);

    // 4. salt = transcript binds the transcript into the PRK.
    let hk = Hkdf::<Sha256>::new(Some(&transcript), &ikm);

    // 5. Expand the four outputs.
    let mut keys = SessionKeys {
        c2s_key: [0u8; 32],
        s2c_key: [0u8; 32],
        c2s_nonce_prefix: [0u8; 4],
        s2c_nonce_prefix: [0u8; 4],
    };
    hk.expand(LABEL_KEY_C2S, &mut keys.c2s_key)
        .map_err(|_| Error::KeyDerivation)?;
    hk.expand(LABEL_KEY_S2C, &mut keys.s2c_key)
        .map_err(|_| Error::KeyDerivation)?;
    hk.expand(LABEL_NONCE_C2S, &mut keys.c2s_nonce_prefix)
        .map_err(|_| Error::KeyDerivation)?;
    hk.expand(LABEL_NONCE_S2C, &mut keys.s2c_nonce_prefix)
        .map_err(|_| Error::KeyDerivation)?;

    // Wipe the IKM (it contained the DH secret and PSK).
    ikm.zeroize();

    Ok(keys)
}

impl SessionKeys {
    /// Consumes the keys, returning the client-side `(sealer, opener)`.
    ///
    /// A client seals on the client-to-server direction and opens on the
    /// server-to-client direction.
    pub fn into_client(mut self) -> (RecordSealer, RecordOpener) {
        let sealer_cipher = ChaCha20Poly1305::new((&self.c2s_key).into());
        let opener_cipher = ChaCha20Poly1305::new((&self.s2c_key).into());
        let sealer = RecordSealer::new(sealer_cipher, self.c2s_nonce_prefix);
        let opener = RecordOpener::new(opener_cipher, self.s2c_nonce_prefix);
        // The ciphers now hold the keys; wipe the raw arrays.
        self.c2s_key.zeroize();
        self.s2c_key.zeroize();
        (sealer, opener)
    }

    /// Consumes the keys, returning the server-side `(sealer, opener)`.
    ///
    /// A server seals on the server-to-client direction and opens on the
    /// client-to-server direction.
    pub fn into_server(mut self) -> (RecordSealer, RecordOpener) {
        let sealer_cipher = ChaCha20Poly1305::new((&self.s2c_key).into());
        let opener_cipher = ChaCha20Poly1305::new((&self.c2s_key).into());
        let sealer = RecordSealer::new(sealer_cipher, self.s2c_nonce_prefix);
        let opener = RecordOpener::new(opener_cipher, self.c2s_nonce_prefix);
        // The ciphers now hold the keys; wipe the raw arrays.
        self.c2s_key.zeroize();
        self.s2c_key.zeroize();
        (sealer, opener)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn all_zero_dh_is_rejected() {
        // `SessionKeys` deliberately does not implement `Debug` (it holds
        // secret key material), so match instead of `unwrap_err`.
        match derive(b"psk", &[0u8; 32], &[1u8; 32], b"hello", b"challenge") {
            Err(e) => assert_eq!(e, Error::WeakSharedSecret),
            Ok(_) => panic!("all-zero DH must be rejected"),
        }
    }

    #[test]
    fn distinct_directions_have_distinct_keys() {
        let keys = derive(
            b"a-good-psk",
            &[7u8; 32],
            &[9u8; 32],
            b"hello",
            b"challenge",
        )
        .unwrap();
        assert_ne!(keys.c2s_key, keys.s2c_key);
        assert_ne!(keys.c2s_nonce_prefix, keys.s2c_nonce_prefix);
    }

    #[test]
    fn ml_kem_secret_changes_the_derived_keys() {
        // Same DH + psk, different ML-KEM secrets must yield different keys.
        let a = derive(
            b"a-good-psk",
            &[7u8; 32],
            &[9u8; 32],
            b"hello",
            b"challenge",
        )
        .unwrap();
        let b = derive(
            b"a-good-psk",
            &[7u8; 32],
            &[10u8; 32],
            b"hello",
            b"challenge",
        )
        .unwrap();
        assert_ne!(
            a.c2s_key, b.c2s_key,
            "the ML-KEM secret must contribute to the schedule"
        );
        assert_ne!(a.s2c_key, b.s2c_key);
    }

    #[test]
    fn key_schedule_matches_independent_hkdf_spec() {
        let psk = b"canonical-token-psk";
        let dh = [0x31u8; 32];
        let ml_kem = [0xa7u8; 32];
        let client_hello = br#"{"type":"hello","capabilities":["e2ee-token-auth"]}"#;
        let server_challenge = br#"{"type":"challenge","capabilities":["e2ee-token-auth"]}"#;

        let keys = derive(psk, &dh, &ml_kem, client_hello, server_challenge).unwrap();

        let transcript = independent_transcript_hash(client_hello, server_challenge);
        let mut ikm = Vec::new();
        ikm.extend_from_slice(&dh);
        ikm.extend_from_slice(&ml_kem);
        ikm.extend_from_slice(psk);
        let hk = Hkdf::<Sha256>::new(Some(&transcript), &ikm);

        let mut c2s_key = [0u8; 32];
        let mut s2c_key = [0u8; 32];
        let mut c2s_nonce_prefix = [0u8; 4];
        let mut s2c_nonce_prefix = [0u8; 4];
        hk.expand(b"rmux web-share v1 key c2s", &mut c2s_key)
            .unwrap();
        hk.expand(b"rmux web-share v1 key s2c", &mut s2c_key)
            .unwrap();
        hk.expand(b"rmux web-share v1 nonce c2s", &mut c2s_nonce_prefix)
            .unwrap();
        hk.expand(b"rmux web-share v1 nonce s2c", &mut s2c_nonce_prefix)
            .unwrap();

        assert_eq!(keys.c2s_key, c2s_key);
        assert_eq!(keys.s2c_key, s2c_key);
        assert_eq!(keys.c2s_nonce_prefix, c2s_nonce_prefix);
        assert_eq!(keys.s2c_nonce_prefix, s2c_nonce_prefix);
    }

    fn independent_transcript_hash(client_hello: &[u8], server_challenge: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"rmux web-share v1 transcript");
        hasher.update((client_hello.len() as u64).to_le_bytes());
        hasher.update(client_hello);
        hasher.update((server_challenge.len() as u64).to_le_bytes());
        hasher.update(server_challenge);
        hasher.finalize().into()
    }
}
