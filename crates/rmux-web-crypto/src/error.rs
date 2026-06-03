//! Error type for the rmux web-share crypto core.

use core::fmt;

/// Errors produced by the web-share crypto core.
///
/// The variants are detailed so internal logic and tests can distinguish
/// failure modes precisely. The rmux-server web module collapses every failure
/// into one opaque close reason before anything reaches the wire, avoiding
/// crypto-oracle style distinctions for remote peers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The supplied Diffie-Hellman shared secret is all-zero.
    ///
    /// X25519 produces an all-zero output for low-order points (RFC 7748),
    /// which would make the resulting keys predictable; such a secret is
    /// rejected with a constant-time check.
    WeakSharedSecret,
    /// The HKDF key-derivation step failed.
    KeyDerivation,
    /// AEAD authentication/decryption failed.
    Decrypt,
    /// A frame arrived with a sequence number other than the next expected one.
    OutOfOrder,
    /// The 64-bit sequence counter is exhausted. The channel fails closed
    /// rather than wrapping or reusing a nonce.
    SequenceExhausted,
    /// A frame was too short, or did not begin with the expected magic byte.
    MalformedFrame,
    /// A record decrypted to zero bytes, so it carries no "kind" byte.
    EmptyPlaintext,
    /// A record carried an unrecognised "kind" byte.
    UnknownKind(u8),
    /// A text record did not contain valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::WeakSharedSecret => f.write_str("weak (all-zero) Diffie-Hellman shared secret"),
            Error::KeyDerivation => f.write_str("key derivation failed"),
            Error::Decrypt => f.write_str("AEAD authentication/decryption failed"),
            Error::OutOfOrder => f.write_str("frame sequence number out of order"),
            Error::SequenceExhausted => f.write_str("sequence counter exhausted"),
            Error::MalformedFrame => f.write_str("malformed frame"),
            Error::EmptyPlaintext => f.write_str("empty record plaintext (missing kind byte)"),
            Error::UnknownKind(k) => write!(f, "unknown record kind byte: {k:#04x}"),
            Error::InvalidUtf8 => f.write_str("text record was not valid UTF-8"),
        }
    }
}

impl std::error::Error for Error {}
