//! Browser WASM bindings for the same record channel used by the daemon.
//!
//! WebCrypto owns X25519 in the browser, so these bindings accept the already
//! computed PSK hash and X25519 DH shared secret bytes. ML-KEM runs in WASM,
//! with caller-supplied WebCrypto entropy. These bindings do not expose any
//! rmux protocol JSON; this module is only the encrypted text/binary transport.

use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

#[cfg(feature = "wasm-test")]
use crate::derive_server_session;
use crate::{derive_client_session, ml_kem, Error as CryptoError, Message, Opener, Sealer};

/// A browser ML-KEM-768 keypair: generated from WebCrypto entropy, its secret key
/// never leaves WASM. The encapsulation key goes in the client hello; the server
/// ciphertext is decapsulated back into the hybrid ML-KEM shared secret.
#[wasm_bindgen]
pub struct MlKemKeyPair {
    inner: ml_kem::KeyPair,
}

#[wasm_bindgen]
impl MlKemKeyPair {
    /// Generates a keypair from exactly 64 bytes of `crypto.getRandomValues`.
    #[wasm_bindgen(constructor)]
    pub fn new(randomness: &[u8]) -> Result<MlKemKeyPair, JsValue> {
        let randomness: [u8; ml_kem::KEYGEN_RANDOMNESS_LEN] = randomness
            .try_into()
            .map_err(|_| JsValue::from_str("ml-kem keygen randomness must be 64 bytes"))?;
        Ok(Self {
            inner: ml_kem::KeyPair::generate(randomness),
        })
    }

    /// The encapsulation key to send in the client hello (1184 bytes).
    #[wasm_bindgen(js_name = encapsulationKey)]
    pub fn encapsulation_key(&self) -> Vec<u8> {
        self.inner.encapsulation_key().to_vec()
    }

    /// Decapsulates the server ciphertext (exactly 1088 bytes) into the ML-KEM
    /// shared secret (32 bytes). Fails closed on a wrong-length ciphertext.
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<Vec<u8>, JsValue> {
        let ciphertext: [u8; ml_kem::CIPHERTEXT_LEN] = ciphertext
            .try_into()
            .map_err(|_| JsValue::from_str("ml-kem ciphertext must be 1088 bytes"))?;
        let shared_secret = self.inner.decapsulate(&ciphertext);
        Ok(shared_secret.to_vec())
    }
}

/// A derived client session: seals client-to-server frames and opens
/// server-to-client frames.
#[wasm_bindgen]
pub struct ClientSession {
    sealer: Sealer,
    opener: Opener,
}

/// The server side of a session.
///
/// Production browsers use [`ClientSession`]. This binding exists so
/// browser-side tests can run a real encrypted daemon mock without duplicating
/// the crypto in TypeScript.
#[cfg(feature = "wasm-test")]
#[wasm_bindgen]
pub struct ServerSession {
    sealer: Sealer,
    opener: Opener,
}

/// A decrypted message returned to JavaScript. Exactly one of `text` / `binary`
/// is set, indicated by `isText`.
#[wasm_bindgen]
pub struct Opened {
    message: Message,
}

#[wasm_bindgen]
impl ClientSession {
    /// Derives the client session from the PSK, the X25519 DH shared secret, the
    /// ML-KEM shared secret, and the exact handshake transcript bytes.
    ///
    /// - `psk`: `SHA-256(token)`, 32 bytes, computed in the browser.
    /// - `dh`: X25519 shared secret, exactly 32 bytes, from WebCrypto.
    /// - `ml_kem_secret`: ML-KEM-768 shared secret, exactly 32 bytes.
    /// - `client_hello` / `server_challenge`: exact wire bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(
        psk: &[u8],
        dh: &[u8],
        ml_kem_secret: &[u8],
        client_hello: &[u8],
        server_challenge: &[u8],
    ) -> Result<ClientSession, JsValue> {
        let dh = dh_array(dh)?;
        let ml_kem_secret = ml_kem_array(ml_kem_secret)?;
        let (sealer, opener) =
            derive_client_session(psk, &dh, &ml_kem_secret, client_hello, server_challenge)
                .map_err(js_error)?;
        Ok(Self { sealer, opener })
    }

    /// Opens a wire frame, returning a tagged [`Opened`] message.
    pub fn open(&mut self, frame: &[u8]) -> Result<Opened, JsValue> {
        self.opener
            .open(frame)
            .map(|message| Opened { message })
            .map_err(js_error)
    }

    /// Seals a binary message, returning the wire frame.
    #[wasm_bindgen(js_name = sealBinary)]
    pub fn seal_binary(&mut self, body: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.sealer.seal_binary(body).map_err(js_error)
    }

    /// Seals a UTF-8 text message, returning the wire frame.
    #[wasm_bindgen(js_name = sealText)]
    pub fn seal_text(&mut self, text: &str) -> Result<Vec<u8>, JsValue> {
        self.sealer.seal_text(text).map_err(js_error)
    }
}

#[cfg(feature = "wasm-test")]
#[wasm_bindgen]
impl ServerSession {
    /// Derives the server session. Mirrors [`ClientSession::new`] but seals the
    /// server-to-client direction and opens client-to-server.
    #[wasm_bindgen(constructor)]
    pub fn new(
        psk: &[u8],
        dh: &[u8],
        ml_kem_secret: &[u8],
        client_hello: &[u8],
        server_challenge: &[u8],
    ) -> Result<ServerSession, JsValue> {
        let dh = dh_array(dh)?;
        let ml_kem_secret = ml_kem_array(ml_kem_secret)?;
        let (sealer, opener) =
            derive_server_session(psk, &dh, &ml_kem_secret, client_hello, server_challenge)
                .map_err(js_error)?;
        Ok(Self { sealer, opener })
    }

    /// Server-side ML-KEM encapsulation for the browser test mock: takes the
    /// client's encapsulation key (1184 bytes) + 32 bytes of randomness and
    /// returns `ciphertext (1088) || shared_secret (32)`. Fails closed on a
    /// malformed encapsulation key.
    #[cfg(feature = "wasm-test")]
    #[wasm_bindgen(js_name = mlKemEncapsulate)]
    pub fn ml_kem_encapsulate(
        encapsulation_key: &[u8],
        randomness: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        let encapsulation_key: [u8; ml_kem::ENCAPSULATION_KEY_LEN] =
            encapsulation_key
                .try_into()
                .map_err(|_| JsValue::from_str("ml-kem encapsulation key must be 1184 bytes"))?;
        let randomness: [u8; ml_kem::ENCAPS_RANDOMNESS_LEN] = randomness
            .try_into()
            .map_err(|_| JsValue::from_str("ml-kem encaps randomness must be 32 bytes"))?;
        let (ciphertext, shared_secret) = ml_kem::encapsulate(&encapsulation_key, randomness)
            .ok_or_else(|| JsValue::from_str("invalid ml-kem encapsulation key"))?;
        let mut out = Vec::with_capacity(ml_kem::CIPHERTEXT_LEN + ml_kem::SHARED_SECRET_LEN);
        out.extend_from_slice(&ciphertext);
        out.extend_from_slice(&shared_secret);
        Ok(out)
    }

    /// Opens a client-to-server wire frame.
    pub fn open(&mut self, frame: &[u8]) -> Result<Opened, JsValue> {
        self.opener
            .open(frame)
            .map(|message| Opened { message })
            .map_err(js_error)
    }

    /// Seals a binary message (server to client).
    #[wasm_bindgen(js_name = sealBinary)]
    pub fn seal_binary(&mut self, body: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.sealer.seal_binary(body).map_err(js_error)
    }

    /// Seals a UTF-8 text message (server to client).
    #[wasm_bindgen(js_name = sealText)]
    pub fn seal_text(&mut self, text: &str) -> Result<Vec<u8>, JsValue> {
        self.sealer.seal_text(text).map_err(js_error)
    }
}

#[wasm_bindgen]
impl Opened {
    /// `true` for a text message, `false` for a binary message.
    #[wasm_bindgen(getter, js_name = isText)]
    pub fn is_text(&self) -> bool {
        matches!(self.message, Message::Text(_))
    }

    /// The UTF-8 text, if this is a text message.
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> Option<String> {
        match &self.message {
            Message::Text(text) => Some(text.clone()),
            Message::Binary(_) => None,
        }
    }

    /// The raw bytes, if this is a binary message.
    #[wasm_bindgen(getter)]
    pub fn binary(&self) -> Option<Vec<u8>> {
        match &self.message {
            Message::Text(_) => None,
            Message::Binary(bytes) => Some(bytes.clone()),
        }
    }
}

fn dh_array(dh: &[u8]) -> Result<Zeroizing<[u8; 32]>, JsValue> {
    let dh: [u8; 32] = dh
        .try_into()
        .map_err(|_| JsValue::from_str("invalid X25519 shared secret length"))?;
    Ok(Zeroizing::new(dh))
}

fn ml_kem_array(secret: &[u8]) -> Result<Zeroizing<[u8; 32]>, JsValue> {
    let secret: [u8; 32] = secret
        .try_into()
        .map_err(|_| JsValue::from_str("invalid ML-KEM shared secret length"))?;
    Ok(Zeroizing::new(secret))
}

fn js_error(_: CryptoError) -> JsValue {
    JsValue::from_str("crypto operation failed")
}
