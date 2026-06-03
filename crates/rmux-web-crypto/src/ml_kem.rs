//! ML-KEM-768 (FIPS 203) wrappers for the post-quantum hybrid handshake.
//!
//! These are thin, allocation-free wrappers over the formally verified
//! `libcrux-ml-kem` crate. Randomness is always passed in by the caller — the
//! browser supplies WebCrypto entropy and tests supply fixed vectors — so no
//! internal RNG (and no `getrandom`) is pulled into the WASM bundle.
//!
//! The hybrid never relies on ML-KEM alone: its shared secret is mixed into the
//! key schedule *alongside* the X25519 DH secret, so the channel stays secure as
//! long as either primitive holds.

use libcrux_ml_kem::mlkem768::{self, MlKem768Ciphertext, MlKem768PublicKey};

/// Randomness consumed by [`KeyPair::generate`].
pub const KEYGEN_RANDOMNESS_LEN: usize = 64;
/// Randomness consumed by [`encapsulate`].
pub const ENCAPS_RANDOMNESS_LEN: usize = 32;
/// Serialized ML-KEM-768 encapsulation (public) key length.
pub const ENCAPSULATION_KEY_LEN: usize = 1184;
/// Serialized ML-KEM-768 ciphertext length.
pub const CIPHERTEXT_LEN: usize = 1088;
/// ML-KEM shared-secret length (mixed into the HKDF ikm).
pub const SHARED_SECRET_LEN: usize = 32;

/// A freshly generated ML-KEM-768 keypair.
///
/// The decapsulation (secret) key never leaves this struct; only the
/// encapsulation key is published on the wire (in the client hello).
pub struct KeyPair {
    inner: mlkem768::MlKem768KeyPair,
}

impl KeyPair {
    /// Generates a keypair from caller-supplied entropy.
    #[must_use]
    pub fn generate(randomness: [u8; KEYGEN_RANDOMNESS_LEN]) -> Self {
        Self {
            inner: mlkem768::generate_key_pair(randomness),
        }
    }

    /// The encapsulation key the peer will encapsulate to.
    #[must_use]
    pub fn encapsulation_key(&self) -> [u8; ENCAPSULATION_KEY_LEN] {
        *self.inner.public_key().as_slice()
    }

    /// Decapsulates a peer ciphertext into the hybrid shared secret.
    #[must_use]
    pub fn decapsulate(&self, ciphertext: &[u8; CIPHERTEXT_LEN]) -> [u8; SHARED_SECRET_LEN] {
        let ciphertext = MlKem768Ciphertext::from(ciphertext);
        mlkem768::decapsulate(self.inner.private_key(), &ciphertext)
    }
}

/// Encapsulates to a peer encapsulation key.
///
/// Returns `None` if `encapsulation_key` is not a valid ML-KEM-768 encapsulation
/// key (the FIPS 203 §7.2 modulus check) — the caller MUST treat that as a
/// handshake rejection (fail closed) rather than proceeding. Otherwise returns
/// the ciphertext to send back (in the server challenge) and the shared secret to
/// mix into the key schedule.
#[must_use]
pub fn encapsulate(
    encapsulation_key: &[u8; ENCAPSULATION_KEY_LEN],
    randomness: [u8; ENCAPS_RANDOMNESS_LEN],
) -> Option<([u8; CIPHERTEXT_LEN], [u8; SHARED_SECRET_LEN])> {
    let encapsulation_key = MlKem768PublicKey::from(encapsulation_key);
    if !mlkem768::validate_public_key(&encapsulation_key) {
        return None;
    }
    let (ciphertext, shared_secret) = mlkem768::encapsulate(&encapsulation_key, randomness);
    Some((*ciphertext.as_slice(), shared_secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encapsulate_decapsulate_round_trip_agrees() {
        let keypair = KeyPair::generate([7u8; KEYGEN_RANDOMNESS_LEN]);
        let ek = keypair.encapsulation_key();
        assert_eq!(ek.len(), ENCAPSULATION_KEY_LEN);

        let (ciphertext, server_secret) =
            encapsulate(&ek, [9u8; ENCAPS_RANDOMNESS_LEN]).expect("valid ek");
        assert_eq!(ciphertext.len(), CIPHERTEXT_LEN);

        let client_secret = keypair.decapsulate(&ciphertext);
        assert_eq!(
            client_secret, server_secret,
            "both sides derive the same secret"
        );
        assert_ne!(client_secret, [0u8; SHARED_SECRET_LEN]);
    }

    #[test]
    fn invalid_encapsulation_key_is_rejected() {
        // An all-0xFF buffer is not a valid ML-KEM encapsulation key (modulus
        // check fails), so the server must fail closed rather than encapsulate.
        assert!(encapsulate(
            &[0xFFu8; ENCAPSULATION_KEY_LEN],
            [0u8; ENCAPS_RANDOMNESS_LEN]
        )
        .is_none());
    }

    #[test]
    fn distinct_encapsulations_yield_distinct_secrets() {
        let keypair = KeyPair::generate([1u8; KEYGEN_RANDOMNESS_LEN]);
        let ek = keypair.encapsulation_key();
        let (_, first) = encapsulate(&ek, [2u8; ENCAPS_RANDOMNESS_LEN]).expect("valid ek");
        let (_, second) = encapsulate(&ek, [3u8; ENCAPS_RANDOMNESS_LEN]).expect("valid ek");
        assert_ne!(first, second);
    }
}
