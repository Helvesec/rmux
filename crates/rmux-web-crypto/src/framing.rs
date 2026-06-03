//! Text/binary "kind byte" framing on top of encrypted records.
//!
//! Each plaintext record is prefixed with a single kind byte
//! (`0x00` = text, `0x01` = binary) before being sealed, matching the
//! web-share wire semantics. The lower record layer seals opaque bytes and is
//! unaware of this byte.

use crate::error::Error;
use crate::record::{RecordOpener, RecordSealer};

/// Kind byte for a UTF-8 text record.
const KIND_TEXT: u8 = 0x00;
/// Kind byte for a binary record.
const KIND_BINARY: u8 = 0x01;

/// A decrypted web-share message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A UTF-8 text message.
    Text(String),
    /// A binary message.
    Binary(Vec<u8>),
}

/// Seals outgoing text/binary messages for one direction.
pub struct Sealer {
    inner: RecordSealer,
}

impl Sealer {
    pub(crate) fn new(inner: RecordSealer) -> Self {
        Self { inner }
    }

    /// Seals a text message into a wire frame.
    pub fn seal_text(&mut self, text: &str) -> Result<Vec<u8>, Error> {
        self.seal(KIND_TEXT, text.as_bytes())
    }

    /// Seals a binary message into a wire frame.
    pub fn seal_binary(&mut self, body: &[u8]) -> Result<Vec<u8>, Error> {
        self.seal(KIND_BINARY, body)
    }

    fn seal(&mut self, kind: u8, body: &[u8]) -> Result<Vec<u8>, Error> {
        let mut plaintext = Vec::with_capacity(1 + body.len());
        plaintext.push(kind);
        plaintext.extend_from_slice(body);
        self.inner.seal(&plaintext)
    }
}

/// Opens incoming text/binary messages for one direction.
pub struct Opener {
    inner: RecordOpener,
}

impl Opener {
    pub(crate) fn new(inner: RecordOpener) -> Self {
        Self { inner }
    }

    /// Opens a wire frame into a [`Message`].
    ///
    /// Never panics on attacker-controlled input: a frame that decrypts to no
    /// bytes yields [`Error::EmptyPlaintext`], an unknown kind byte yields
    /// [`Error::UnknownKind`], and invalid UTF-8 in a text record yields
    /// [`Error::InvalidUtf8`].
    pub fn open(&mut self, frame: &[u8]) -> Result<Message, Error> {
        let plaintext = self.inner.open(frame)?;
        let (&kind, body) = plaintext.split_first().ok_or(Error::EmptyPlaintext)?;
        match kind {
            KIND_TEXT => {
                let text = core::str::from_utf8(body).map_err(|_| Error::InvalidUtf8)?;
                Ok(Message::Text(text.to_owned()))
            }
            KIND_BINARY => Ok(Message::Binary(body.to_vec())),
            other => Err(Error::UnknownKind(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A raw client sealer plus a matching server opener: both on the
    // client-to-server direction, both starting at sequence 0, so we can craft
    // records that bypass the kind-byte Sealer and exercise Opener edge cases.
    fn raw_c2s_pair() -> (RecordSealer, Opener) {
        let psk = b"unit-test-psk-not-a-real-secret!";
        let dh = [9u8; 32];
        let sealer = crate::schedule::derive(psk, &dh, &[0x42u8; 32], b"h", b"c")
            .unwrap()
            .into_client()
            .0;
        let opener = crate::schedule::derive(psk, &dh, &[0x42u8; 32], b"h", b"c")
            .unwrap()
            .into_server()
            .1;
        (sealer, Opener::new(opener))
    }

    #[test]
    fn empty_plaintext_is_rejected_not_panicked() {
        let (mut sealer, mut opener) = raw_c2s_pair();
        let frame = sealer.seal(&[]).unwrap();
        assert_eq!(opener.open(&frame), Err(Error::EmptyPlaintext));
    }

    #[test]
    fn unknown_kind_byte_is_rejected() {
        let (mut sealer, mut opener) = raw_c2s_pair();
        let frame = sealer.seal(&[0x7f, b'x']).unwrap();
        assert_eq!(opener.open(&frame), Err(Error::UnknownKind(0x7f)));
    }

    #[test]
    fn invalid_utf8_text_is_rejected() {
        let (mut sealer, mut opener) = raw_c2s_pair();
        let frame = sealer.seal(&[KIND_TEXT, 0xff, 0xfe]).unwrap();
        assert_eq!(opener.open(&frame), Err(Error::InvalidUtf8));
    }
}
