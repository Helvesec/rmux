#![forbid(unsafe_code)]
//! rmux web-share end-to-end crypto core.
//!
//! This crate owns the whole web-share cryptographic boundary:
//!
//! - ephemeral **X25519** key generation and Diffie-Hellman ([`Ephemeral`]);
//! - **ML-KEM-768** wrappers for the post-quantum hybrid shared secret
//!   ([`ml_kem`]);
//! - deriving session keys from a PSK, an X25519 DH shared secret, an ML-KEM
//!   shared secret, and the exact handshake transcript bytes ([`derive_server_session`],
//!   [`derive_client_session`], [`schedule`]);
//! - authenticated ChaCha20-Poly1305 records ([`RecordSealer`],
//!   [`RecordOpener`]);
//! - the web-share **text/binary "kind byte"** framing layered on top of
//!   opaque records ([`Sealer`], [`Opener`], [`Message`]);
//! - optional browser WASM bindings behind the `wasm` feature.
//!
//! It deliberately knows nothing about network transports, JSON, or HTTP; those
//! live in the rmux-server web module, which keeps this crate free of any
//! dependency on rmux-server (no circular dependency).
//!
//! Forward secrecy comes from per-connection X25519 and ML-KEM secrets.
//! Authentication comes from the PSK mixed into the key schedule. The PSK must
//! be high-entropy — rmux uses `SHA-256(256-bit token)`, never a low-entropy PIN.

extern crate alloc;

mod error;
mod framing;
pub mod ml_kem;
pub mod record;
pub mod schedule;
mod session;
pub mod transcript;
#[cfg(feature = "wasm")]
mod wasm;
#[cfg(feature = "x25519")]
mod x25519;

pub use error::Error;
pub use framing::{Message, Opener, Sealer};
pub use record::{RecordOpener, RecordSealer, ENCRYPTED_FRAME};
pub use schedule::SessionKeys;
pub use session::{derive_client_session, derive_server_session};
pub use transcript::transcript_hash;
#[cfg(feature = "x25519")]
pub use x25519::{generate_ephemeral, Ephemeral};
