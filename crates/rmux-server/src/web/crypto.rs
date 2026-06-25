use std::io;

use base64::Engine;
use rmux_web_crypto::{Message, Opener, Sealer};
use serde::Deserialize;
use zeroize::Zeroizing;

use super::websocket::{WebSocketMessage, WebSocketReader, WebSocketWriter};

pub(super) const E2EE_CAPABILITY: &str = "e2ee-token-auth";

/// A parsed web-share client hello.
///
/// `raw` is the EXACT hello text received on the wire. It is bound into the
/// session key schedule as part of the handshake transcript, so it must be the
/// untouched bytes, not a re-serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientHello {
    pub(super) token_id: String,
    pub(super) client_nonce: String,
    pub(super) client_public: [u8; 32],
    /// The client's ML-KEM-768 encapsulation key, length-validated to exactly
    /// 1184 bytes at parse time (a [u8; N>32] does not derive Eq/Debug, so it is
    /// held as a Vec). Its ML-KEM validity is checked at encapsulation time.
    pub(super) client_ml_kem_ek: Vec<u8>,
    pub(super) raw: String,
}

pub(super) struct EncryptedWebSocketReader {
    reader: WebSocketReader,
    opener: FrameOpener,
}

pub(super) struct EncryptedWebSocketWriter {
    writer: WebSocketWriter,
    sealer: FrameSealer,
    seal_scratch: Vec<u8>,
}

/// Thin newtype around the [`rmux_web_crypto::Opener`] (server-to-client opener
/// from the caller's point of view: the server opens client-to-server frames).
pub(super) struct FrameOpener {
    opener: Opener,
}

/// Thin newtype around the [`rmux_web_crypto::Sealer`].
pub(super) struct FrameSealer {
    sealer: Sealer,
}

/// Encodes bytes as base64url without padding (the web-share wire encoding).
pub(super) fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub(super) fn random_handshake_nonce() -> io::Result<String> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|error| {
        io::Error::other(format!("failed to create web-share e2ee nonce: {error}"))
    })?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce))
}

/// Derives the server side of a web-share session from the token-derived PSK, the
/// X25519 DH shared secret, the ML-KEM shared secret, and the exact handshake
/// transcript bytes.
///
/// Returns the opener for client-to-server frames and the sealer for
/// server-to-client frames, ready to wrap into the encrypted reader/writer.
pub(super) fn derive_server_crypto(
    psk: &[u8],
    dh: &[u8; 32],
    ml_kem_secret: &[u8; 32],
    client_hello_bytes: &[u8],
    server_challenge_bytes: &[u8],
) -> io::Result<(FrameOpener, FrameSealer)> {
    let (sealer, opener) = rmux_web_crypto::derive_server_session(
        psk,
        dh,
        ml_kem_secret,
        client_hello_bytes,
        server_challenge_bytes,
    )
    .map_err(|error| io::Error::other(format!("failed to derive web-share session: {error}")))?;
    Ok((FrameOpener { opener }, FrameSealer { sealer }))
}

pub(super) fn parse_client_hello(text: &str, protocol_version: u16) -> Result<ClientHello, ()> {
    let hello = serde_json::from_str::<ClientHelloWire>(text).map_err(|_| ())?;
    if hello.kind != "hello" || hello.protocol_version != protocol_version {
        return Err(());
    }
    if !hello
        .capabilities
        .iter()
        .any(|capability| capability == E2EE_CAPABILITY)
    {
        return Err(());
    }
    if !super::secrets::valid_token_id_shape(&hello.token_id)
        || decode_nonce(&hello.client_nonce).is_err()
    {
        return Err(());
    }
    let client_public = decode_client_public(&hello.client_public)?;
    let client_ml_kem_ek = decode_ml_kem_ek(&hello.client_ml_kem_ek)?;
    Ok(ClientHello {
        token_id: hello.token_id,
        client_nonce: hello.client_nonce,
        client_public,
        client_ml_kem_ek,
        raw: text.to_owned(),
    })
}

/// Decodes and length-validates the client's ML-KEM-768 encapsulation key
/// (base64url, exactly 1184 bytes). A wrong length is rejected here; ML-KEM
/// validity is enforced at encapsulation time.
fn decode_ml_kem_ek(encoded: &str) -> Result<Vec<u8>, ()> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ())?;
    if decoded.len() != rmux_web_crypto::ml_kem::ENCAPSULATION_KEY_LEN {
        return Err(());
    }
    Ok(decoded)
}

/// The result of a successful ML-KEM encapsulation: the public ciphertext for
/// the challenge and the (zeroizing) shared secret for the key schedule.
type MlKemEncapsulation = (
    [u8; rmux_web_crypto::ml_kem::CIPHERTEXT_LEN],
    Zeroizing<[u8; 32]>,
);

/// Encapsulates to the client's ML-KEM encapsulation key, returning the
/// ciphertext (for the challenge) and the shared secret (for the schedule).
///
/// `Ok(None)` means the key failed ML-KEM validation — the caller MUST collapse
/// that to the uniform pre-ready rejection (fail closed), not bypass the delay.
/// `Err` is a server RNG failure.
pub(super) fn encapsulate_ml_kem(
    client_ml_kem_ek: &[u8],
) -> io::Result<Option<MlKemEncapsulation>> {
    let Ok(ek) =
        <&[u8; rmux_web_crypto::ml_kem::ENCAPSULATION_KEY_LEN]>::try_from(client_ml_kem_ek)
    else {
        return Ok(None);
    };
    let mut randomness = Zeroizing::new([0u8; rmux_web_crypto::ml_kem::ENCAPS_RANDOMNESS_LEN]);
    getrandom::fill(randomness.as_mut()).map_err(|error| {
        io::Error::other(format!(
            "failed to create ml-kem encaps randomness: {error}"
        ))
    })?;
    // The ciphertext is public; the shared secret is wiped when it drops.
    Ok(rmux_web_crypto::ml_kem::encapsulate(ek, *randomness)
        .map(|(ciphertext, shared_secret)| (ciphertext, Zeroizing::new(shared_secret))))
}

impl EncryptedWebSocketReader {
    pub(super) fn new(reader: WebSocketReader, opener: FrameOpener) -> Self {
        Self { reader, opener }
    }

    pub(super) async fn read_message(&mut self) -> io::Result<WebSocketMessage> {
        let message = self.reader.read_message().await?;
        match message {
            WebSocketMessage::Binary(bytes) => self.opener.open_message(&bytes),
            WebSocketMessage::Ping(payload) => Ok(WebSocketMessage::Ping(payload)),
            WebSocketMessage::Pong => Ok(WebSocketMessage::Pong),
            WebSocketMessage::Close => Ok(WebSocketMessage::Close),
            WebSocketMessage::Text(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plaintext websocket text after e2ee handshake",
            )),
        }
    }
}

impl EncryptedWebSocketWriter {
    pub(super) fn new(writer: WebSocketWriter, sealer: FrameSealer) -> Self {
        Self {
            writer,
            sealer,
            seal_scratch: Vec::new(),
        }
    }

    pub(super) async fn write_text(&mut self, text: &str) -> io::Result<()> {
        self.seal_scratch.clear();
        self.sealer.seal_text_into(text, &mut self.seal_scratch)?;
        self.writer.write_binary(&self.seal_scratch).await
    }

    pub(super) async fn write_binary(&mut self, payload: &[u8]) -> io::Result<()> {
        self.seal_scratch.clear();
        self.sealer
            .seal_binary_into(payload, &mut self.seal_scratch)?;
        self.writer.write_binary(&self.seal_scratch).await
    }

    pub(super) async fn write_close(&mut self) -> io::Result<()> {
        self.writer.write_close().await
    }

    pub(super) async fn write_close_code(&mut self, code: u16, reason: &str) -> io::Result<()> {
        self.writer.write_close_code(code, reason).await
    }

    pub(super) async fn write_pong(&mut self, payload: &[u8]) -> io::Result<()> {
        self.writer.write_pong(payload).await
    }
}

impl FrameOpener {
    pub(super) fn open_message(&mut self, frame: &[u8]) -> io::Result<WebSocketMessage> {
        match self
            .opener
            .open(frame)
            .map_err(|_| invalid_data("e2ee open failed"))?
        {
            Message::Text(text) => Ok(WebSocketMessage::Text(text)),
            Message::Binary(payload) => Ok(WebSocketMessage::Binary(payload)),
        }
    }
}

impl FrameSealer {
    pub(super) fn seal_text_into(&mut self, text: &str, dst: &mut Vec<u8>) -> io::Result<()> {
        self.sealer
            .seal_text_into(text, dst)
            .map_err(|_| io::Error::other("e2ee seal failed"))
    }

    pub(super) fn seal_binary_into(&mut self, payload: &[u8], dst: &mut Vec<u8>) -> io::Result<()> {
        self.sealer
            .seal_binary_into(payload, dst)
            .map_err(|_| io::Error::other("e2ee seal failed"))
    }
}

fn decode_nonce(nonce: &str) -> io::Result<Vec<u8>> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(nonce)
        .map_err(|_| invalid_data("invalid e2ee nonce"))?;
    if decoded.len() != 16 {
        return Err(invalid_data("invalid e2ee nonce length"));
    }
    Ok(decoded)
}

fn decode_client_public(client_public: &str) -> Result<[u8; 32], ()> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(client_public)
        .map_err(|_| ())?;
    <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| ())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientHelloWire {
    #[serde(rename = "type")]
    kind: String,
    protocol_version: u16,
    capabilities: Vec<String>,
    token_id: String,
    client_nonce: String,
    client_public: String,
    client_ml_kem_ek: String,
}

#[cfg(test)]
mod tests {
    use super::{derive_server_crypto, FrameSealer, WebSocketMessage};
    use crate::web::secrets::SecretHash;
    use base64::Engine;
    use rmux_web_crypto::{derive_client_session, generate_ephemeral};
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(FrameSealer: Clone);

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn token_id_is_stable_and_not_the_secret_hash() {
        let secret = SecretHash::from_secret("token");

        assert_eq!(
            secret.token_id(),
            SecretHash::from_secret("token").token_id()
        );
        assert_ne!(
            secret.token_id(),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret.as_bytes())
        );
    }

    #[test]
    fn e2ee_round_trips_text_and_binary_in_order() {
        let secret = SecretHash::from_secret("token");
        let psk = secret.as_bytes();

        // Full X25519 agreement, exactly as the live handshake performs it.
        let client_eph = generate_ephemeral();
        let server_eph = generate_ephemeral();
        let client_public = client_eph.public_bytes();
        let server_public = server_eph.public_bytes();

        let client_hello = br#"{"type":"hello","client_public":"..."}"#;
        let server_challenge = br#"{"type":"challenge","server_public":"..."}"#;

        let client_dh = client_eph.into_shared_secret(&server_public);
        let server_dh = server_eph.into_shared_secret(&client_public);
        assert_eq!(*client_dh, *server_dh, "X25519 agreement must converge");

        // A matching ML-KEM shared secret on both sides (the agreement itself is
        // tested in rmux-web-crypto); this exercises the hybrid record layer.
        let ml_kem_ss = [0x55u8; 32];
        let (mut server_open, mut server_seal) =
            derive_server_crypto(&psk, &server_dh, &ml_kem_ss, client_hello, server_challenge)
                .expect("derive server");
        let (mut client_seal, mut client_open) =
            derive_client_session(&psk, &client_dh, &ml_kem_ss, client_hello, server_challenge)
                .expect("derive client");

        let frame = client_seal.seal_text(r#"{"type":"auth"}"#).expect("seal");
        assert_eq!(
            server_open.open_message(&frame).expect("open"),
            WebSocketMessage::Text(r#"{"type":"auth"}"#.to_owned())
        );

        let mut frame = Vec::new();
        server_seal
            .seal_binary_into(&[0x10, b'o', b'k'], &mut frame)
            .expect("seal into");
        assert_eq!(
            client_open.open(&frame).expect("open"),
            rmux_web_crypto::Message::Binary(vec![0x10, b'o', b'k'])
        );

        frame.clear();
        server_seal
            .seal_binary_into(&[0x11, b'o', b'k'], &mut frame)
            .expect("seal into");
        assert_eq!(
            client_open.open(&frame).expect("open"),
            rmux_web_crypto::Message::Binary(vec![0x11, b'o', b'k'])
        );
    }

    #[test]
    fn client_hello_rejects_missing_e2ee_capability() {
        let client_public = b64(&generate_ephemeral().public_bytes());
        let ek = b64(&rmux_web_crypto::ml_kem::KeyPair::generate([1u8; 64]).encapsulation_key());
        let text = format!(
            r#"{{"type":"hello","protocol_version":1,"capabilities":["token-auth"],"token_id":"aaaaaaaaaaaaaaaaaaaaaa","client_nonce":"AQIDBAUGBwgJCgsMDQ4PEA","client_public":"{client_public}","client_ml_kem_ek":"{ek}"}}"#
        );

        assert!(super::parse_client_hello(&text, 1).is_err());
    }

    #[test]
    fn client_hello_preserves_exact_raw_wire_text() {
        let client_public = b64(&generate_ephemeral().public_bytes());
        let ek = b64(&rmux_web_crypto::ml_kem::KeyPair::generate([1u8; 64]).encapsulation_key());
        let text = format!(
            r#"{{"type":"hello","protocol_version":1,"capabilities":["terminal-palette-v1","e2ee-token-auth"],"token_id":"VANRFV6FYQX1QTOi-BMVrQ","client_nonce":"AQIDBAUGBwgJCgsMDQ4PEA","client_public":"{client_public}","client_ml_kem_ek":"{ek}"}}"#
        );

        let hello = super::parse_client_hello(&text, 1).expect("valid hello");
        assert_eq!(hello.raw, text);
    }

    #[test]
    fn e2ee_wrong_token_fails_to_open() {
        // Identical handshake transcript + DH, but the client authenticated with
        // a different token (PSK). The server's AEAD must reject the first frame
        // — implicit authentication by the token, end to end at the server layer.
        let server_secret = SecretHash::from_secret("server-token");
        let wrong_secret = SecretHash::from_secret("attacker-token");
        let client_eph = generate_ephemeral();
        let server_eph = generate_ephemeral();
        let client_public = client_eph.public_bytes();
        let server_public = server_eph.public_bytes();
        let client_hello = br#"{"type":"hello"}"#;
        let server_challenge = br#"{"type":"challenge"}"#;
        let client_dh = client_eph.into_shared_secret(&server_public);
        let server_dh = server_eph.into_shared_secret(&client_public);

        let ml_kem_ss = [0x55u8; 32];
        let (mut server_open, _server_seal) = derive_server_crypto(
            &server_secret.as_bytes(),
            &server_dh,
            &ml_kem_ss,
            client_hello,
            server_challenge,
        )
        .expect("derive server");
        let (mut client_seal, _client_open) = derive_client_session(
            &wrong_secret.as_bytes(),
            &client_dh,
            &ml_kem_ss,
            client_hello,
            server_challenge,
        )
        .expect("derive client");

        let frame = client_seal.seal_text(r#"{"type":"auth"}"#).expect("seal");
        assert!(
            server_open.open_message(&frame).is_err(),
            "a frame sealed under the wrong token must fail AEAD authentication"
        );
    }
}
