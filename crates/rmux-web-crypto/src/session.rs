//! Deriving an encrypted record session for the server or client side.

use crate::error::Error;
use crate::framing::{Opener, Sealer};

/// Derives the **server** side of a session.
///
/// The server seals on the server-to-client direction and opens the
/// client-to-server direction.
pub fn derive_server_session(
    psk: &[u8],
    dh_shared_secret: &[u8; 32],
    ml_kem_shared_secret: &[u8; 32],
    client_hello: &[u8],
    server_challenge: &[u8],
) -> Result<(Sealer, Opener), Error> {
    let keys = crate::schedule::derive(
        psk,
        dh_shared_secret,
        ml_kem_shared_secret,
        client_hello,
        server_challenge,
    )?;
    let (sealer, opener) = keys.into_server();
    Ok((Sealer::new(sealer), Opener::new(opener)))
}

/// Derives the **client** side of a session.
///
/// The client seals on the client-to-server direction and opens the
/// server-to-client direction.
pub fn derive_client_session(
    psk: &[u8],
    dh_shared_secret: &[u8; 32],
    ml_kem_shared_secret: &[u8; 32],
    client_hello: &[u8],
    server_challenge: &[u8],
) -> Result<(Sealer, Opener), Error> {
    let keys = crate::schedule::derive(
        psk,
        dh_shared_secret,
        ml_kem_shared_secret,
        client_hello,
        server_challenge,
    )?;
    let (sealer, opener) = keys.into_client();
    Ok((Sealer::new(sealer), Opener::new(opener)))
}
