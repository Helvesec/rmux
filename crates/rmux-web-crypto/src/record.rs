//! Authenticated record framing.
//!
//! A record on the wire is:
//!
//! ```text
//! [0xE0] || [seq: u64 big-endian, 8 bytes] || [ChaCha20-Poly1305 ciphertext+tag]
//! ```
//!
//! The 9-byte header (`magic || seq`) is used as the AEAD additional
//! authenticated data (AAD), so the magic byte and sequence number are
//! authenticated even though they are sent in the clear. The 12-byte nonce is
//! `nonce_prefix (4 bytes) || seq.to_be_bytes() (8 bytes)`; since the prefix is
//! fixed per direction and the sequence number is unique and monotonic, every
//! nonce is unique for the lifetime of the keys.
//!
//! This core seals *opaque* bytes. It deliberately adds no plaintext
//! "kind"/type byte (text vs binary) — that is a consumer concern, not part of
//! the channel core.

use alloc::vec::Vec;
use chacha20poly1305::aead::{Aead, AeadInPlace, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

use crate::error::Error;

/// Magic byte marking an encrypted frame.
pub const ENCRYPTED_FRAME: u8 = 0xE0;

/// Length of the cleartext, authenticated frame header (`magic || seq`).
const HEADER_LEN: usize = 1 + 8;
/// Length of the Poly1305 authentication tag.
const TAG_LEN: usize = 16;
/// Minimum frame length: header plus at least an empty ciphertext's tag.
const MIN_FRAME_LEN: usize = HEADER_LEN + TAG_LEN;

/// Builds the 12-byte ChaCha20-Poly1305 nonce from a prefix and sequence.
fn make_nonce(prefix: &[u8; 4], seq: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(prefix);
    nonce[4..].copy_from_slice(&seq.to_be_bytes());
    nonce
}

/// Builds the 9-byte cleartext header (`magic || seq`) used as AEAD AAD.
fn make_header(seq: u64) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[0] = ENCRYPTED_FRAME;
    header[1..].copy_from_slice(&seq.to_be_bytes());
    header
}

/// Seals (encrypts and authenticates) outgoing records for one direction.
pub struct RecordSealer {
    cipher: ChaCha20Poly1305,
    nonce_prefix: [u8; 4],
    next_seq: u64,
}

impl RecordSealer {
    /// Constructs a sealer from a cipher, nonce prefix, and starting sequence.
    pub(crate) fn new(cipher: ChaCha20Poly1305, nonce_prefix: [u8; 4]) -> Self {
        Self {
            cipher,
            nonce_prefix,
            next_seq: 0,
        }
    }

    /// Test-only constructor allowing the starting sequence to be set, so the
    /// [`Error::SequenceExhausted`] path can be exercised near `u64::MAX`.
    #[cfg(test)]
    pub(crate) fn with_seq(cipher: ChaCha20Poly1305, nonce_prefix: [u8; 4], next_seq: u64) -> Self {
        Self {
            cipher,
            nonce_prefix,
            next_seq,
        }
    }

    /// Seals one plaintext record, returning the full wire frame.
    ///
    /// The returned bytes are `[0xE0] || seq.to_be_bytes() || ciphertext+tag`.
    /// Fails closed with [`Error::SequenceExhausted`] if the 64-bit counter
    /// would overflow, so a nonce is never reused.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let mut frame = Vec::new();
        self.seal_into(plaintext, &mut frame)?;
        Ok(frame)
    }

    /// Seals one plaintext record into a caller-owned destination buffer.
    ///
    /// The full wire frame is appended to `dst`. Capacity is reserved before
    /// the sequence is used; if sealing fails after bytes have been appended,
    /// `dst` must be treated as poisoned by the caller and cleared before
    /// reuse. The sequence advances only after successful authentication.
    pub fn seal_into(&mut self, plaintext: &[u8], dst: &mut Vec<u8>) -> Result<(), Error> {
        self.seal_parts_into(&[plaintext], dst)
    }

    pub(crate) fn seal_parts_into(
        &mut self,
        plaintext_parts: &[&[u8]],
        dst: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if self.next_seq == u64::MAX {
            return Err(Error::SequenceExhausted);
        }
        let plaintext_len = plaintext_parts
            .iter()
            .map(|part| part.len())
            .try_fold(0usize, usize::checked_add)
            .ok_or(Error::SequenceExhausted)?;
        dst.reserve(HEADER_LEN + plaintext_len + TAG_LEN);

        let seq = self.next_seq;
        let header = make_header(seq);
        let nonce = make_nonce(&self.nonce_prefix, seq);

        let start = dst.len();
        dst.extend_from_slice(&header);
        for part in plaintext_parts {
            dst.extend_from_slice(part);
        }

        let tag = self
            .cipher
            .encrypt_in_place_detached(
                Nonce::from_slice(&nonce),
                &header,
                &mut dst[start + HEADER_LEN..],
            )
            .map_err(|_| Error::Decrypt)?;
        dst.extend_from_slice(&tag);

        // Advance only after a successful seal. The pre-check above leaves
        // one counter value unused, which avoids doing work with a terminal
        // nonce that cannot be safely advanced.
        self.next_seq += 1;
        Ok(())
    }
}

/// Opens (verifies and decrypts) incoming records for one direction.
pub struct RecordOpener {
    cipher: ChaCha20Poly1305,
    nonce_prefix: [u8; 4],
    next_seq: u64,
}

impl RecordOpener {
    /// Constructs an opener from a cipher, nonce prefix, and starting sequence.
    pub(crate) fn new(cipher: ChaCha20Poly1305, nonce_prefix: [u8; 4]) -> Self {
        Self {
            cipher,
            nonce_prefix,
            next_seq: 0,
        }
    }

    /// Test-only constructor allowing the starting sequence to be set.
    #[cfg(test)]
    pub(crate) fn with_seq(cipher: ChaCha20Poly1305, nonce_prefix: [u8; 4], next_seq: u64) -> Self {
        Self {
            cipher,
            nonce_prefix,
            next_seq,
        }
    }

    /// Opens one wire frame, returning the recovered plaintext.
    ///
    /// Enforces strict in-order delivery: a frame whose sequence number is not
    /// the next expected one yields [`Error::OutOfOrder`] (this rejects both
    /// replays and reordering). All attacker-controlled slicing is bounds
    /// checked first; this function never panics on malformed input.
    pub fn open(&mut self, frame: &[u8]) -> Result<Vec<u8>, Error> {
        if frame.len() < MIN_FRAME_LEN || frame[0] != ENCRYPTED_FRAME {
            return Err(Error::MalformedFrame);
        }

        // The first 9 bytes are the authenticated header.
        let header = &frame[..HEADER_LEN];
        let mut seq_bytes = [0u8; 8];
        seq_bytes.copy_from_slice(&header[1..HEADER_LEN]);
        let seq = u64::from_be_bytes(seq_bytes);

        if seq != self.next_seq {
            return Err(Error::OutOfOrder);
        }
        if self.next_seq == u64::MAX {
            return Err(Error::SequenceExhausted);
        }

        let nonce = make_nonce(&self.nonce_prefix, seq);
        let ciphertext = &frame[HEADER_LEN..];

        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: header,
                },
            )
            .map_err(|_| Error::Decrypt)?;

        // Advance only after successful authentication.
        self.next_seq += 1;

        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chacha20poly1305::KeyInit;

    fn cipher() -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new((&[9u8; 32]).into())
    }

    #[test]
    fn seal_into_appends_same_frame_as_seal() {
        let mut allocating = RecordSealer::with_seq(cipher(), [1, 2, 3, 4], 0);
        let mut append = RecordSealer::with_seq(cipher(), [1, 2, 3, 4], 0);
        let expected = allocating.seal(b"payload").unwrap();
        let mut dst = b"prefix".to_vec();

        append.seal_into(b"payload", &mut dst).unwrap();

        assert_eq!(&dst[..6], b"prefix");
        assert_eq!(&dst[6..], expected.as_slice());
    }

    #[test]
    fn seal_into_at_max_seq_fails_without_touching_destination() {
        let mut sealer = RecordSealer::with_seq(cipher(), [1, 2, 3, 4], u64::MAX);
        let mut dst = b"keep".to_vec();

        let err = sealer.seal_into(b"payload", &mut dst).unwrap_err();

        assert_eq!(err, Error::SequenceExhausted);
        assert_eq!(dst, b"keep");
    }

    #[test]
    fn seal_at_max_seq_fails_closed() {
        // Sealing at u64::MAX would use a nonce that cannot advance.
        let mut sealer = RecordSealer::with_seq(cipher(), [1, 2, 3, 4], u64::MAX);
        let err = sealer.seal(b"payload").unwrap_err();
        assert_eq!(err, Error::SequenceExhausted);
    }

    #[test]
    fn open_at_max_seq_fails_closed() {
        use chacha20poly1305::aead::{Aead, Payload};
        use chacha20poly1305::Nonce;

        // Build a valid frame at seq = u64::MAX directly with the cipher (the
        // sealer itself would fail closed before returning such a frame). Then
        // open it with an opener positioned at u64::MAX: authentication
        // would require a terminal sequence number and is rejected before
        // decryption.
        let prefix = [1u8, 2, 3, 4];
        let seq = u64::MAX;
        let header = make_header(seq);
        let nonce = make_nonce(&prefix, seq);
        let ct = cipher()
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: b"payload",
                    aad: &header,
                },
            )
            .unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&ct);

        let mut opener = RecordOpener::with_seq(cipher(), prefix, seq);
        let err = opener.open(&frame).unwrap_err();
        assert_eq!(err, Error::SequenceExhausted);
    }

    #[test]
    fn last_legal_frame_seals_then_next_fails() {
        // The last legal sequence (u64::MAX - 1) seals successfully and advances
        // the counter to u64::MAX; the very next seal then fails closed, so the
        // terminal nonce is never reused.
        let mut sealer = RecordSealer::with_seq(cipher(), [1, 2, 3, 4], u64::MAX - 1);
        assert!(sealer.seal(b"last legal frame").is_ok());
        assert_eq!(
            sealer.seal(b"would reuse the terminal nonce").unwrap_err(),
            Error::SequenceExhausted
        );
    }
}
