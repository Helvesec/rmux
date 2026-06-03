//! Ephemeral X25519 key agreement.

use x25519_dalek::{EphemeralSecret, PublicKey};
use zeroize::Zeroizing;

/// An ephemeral X25519 key pair.
///
/// The secret is generated from the OS CSPRNG and is **consumed** when the
/// shared secret is computed ([`Ephemeral::into_shared_secret`]), enforcing the
/// single-use property that gives forward secrecy. The secret is zeroized on
/// drop (via x25519-dalek's `zeroize` feature).
pub struct Ephemeral {
    secret: EphemeralSecret,
    public: PublicKey,
}

/// Generates a fresh ephemeral X25519 key pair from the OS CSPRNG.
pub fn generate_ephemeral() -> Ephemeral {
    let secret = EphemeralSecret::random();
    let public = PublicKey::from(&secret);
    Ephemeral { secret, public }
}

impl Ephemeral {
    /// Returns the public key as 32 raw bytes (to send to the peer).
    pub fn public_bytes(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    /// Consumes this ephemeral key, computing the X25519 shared secret with the
    /// peer's public key.
    ///
    /// The all-zero (low-order-point) result is not rejected here; the key
    /// schedule rejects it during derivation.
    ///
    /// The returned secret is wrapped in [`Zeroizing`] so it is wiped from
    /// memory when dropped.
    pub fn into_shared_secret(self, peer_public: &[u8; 32]) -> Zeroizing<[u8; 32]> {
        let peer = PublicKey::from(*peer_public);
        Zeroizing::new(self.secret.diffie_hellman(&peer).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    fn unhex(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for index in 0..32 {
            out[index] = u8::from_str_radix(&s[index * 2..index * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn x25519_matches_rfc7748_so_it_interops_with_webcrypto() {
        // RFC 7748 section 6.1. WebCrypto (the browser side) produces the same
        // shared secret K for this vector, so the daemon's x25519-dalek and the
        // browser's WebCrypto agree — the cross-implementation interop the
        // record layer depends on.
        let alice = unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let bob_public = unhex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let shared = unhex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(x25519_dalek::x25519(alice, bob_public), shared);
    }
}
