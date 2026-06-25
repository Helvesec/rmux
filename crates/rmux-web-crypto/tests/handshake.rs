//! Full X25519 + ML-KEM hybrid handshake plus encrypted record round-trip.
//!
//! Exercises the native ephemeral key agreement, so it only applies to the
//! x25519 feature (default). The browser/WASM build runs `--features wasm-test`
//! with x25519 off — `generate_ephemeral` does not exist there — so the whole
//! file is gated out rather than failing to compile that gate.
#![cfg(feature = "x25519")]

use rmux_web_crypto::{derive_client_session, derive_server_session, generate_ephemeral, Message};

const PSK: &[u8] = b"a-high-entropy-32-byte-test-psk!";
const HELLO: &[u8] = b"client-hello-wire-bytes";
const CHALLENGE: &[u8] = b"server-challenge-wire-bytes";
/// A fixed ML-KEM shared secret for the record-layer round-trip tests (the
/// hybrid agreement itself is exercised in `hybrid_*`).
const MLKEM_SS: [u8; 32] = [0x42; 32];

/// Runs a fresh ephemeral X25519 agreement and returns the matching
/// `(client_dh, server_dh)` shared secrets.
fn agree() -> ([u8; 32], [u8; 32]) {
    let client = generate_ephemeral();
    let server = generate_ephemeral();
    let client_pub = client.public_bytes();
    let server_pub = server.public_bytes();
    let client_dh = client.into_shared_secret(&server_pub);
    let server_dh = server.into_shared_secret(&client_pub);
    assert_eq!(*client_dh, *server_dh, "X25519 agreement must match");
    (*client_dh, *server_dh)
}

#[test]
fn full_handshake_round_trip_both_directions() {
    let (client_dh, server_dh) = agree();
    let (mut client_sealer, mut client_opener) =
        derive_client_session(PSK, &client_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let (mut server_sealer, mut server_opener) =
        derive_server_session(PSK, &server_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();

    // client -> server (c2s)
    let f1 = client_sealer.seal_text("hello server").unwrap();
    assert_eq!(
        server_opener.open(&f1).unwrap(),
        Message::Text("hello server".into())
    );
    let f2 = client_sealer.seal_binary(&[1, 2, 3]).unwrap();
    assert_eq!(
        server_opener.open(&f2).unwrap(),
        Message::Binary(vec![1, 2, 3])
    );

    // server -> client (s2c)
    let g1 = server_sealer.seal_binary(&[0xaa, 0xbb]).unwrap();
    assert_eq!(
        client_opener.open(&g1).unwrap(),
        Message::Binary(vec![0xaa, 0xbb])
    );
    let g2 = server_sealer.seal_text("ready").unwrap();
    assert_eq!(
        client_opener.open(&g2).unwrap(),
        Message::Text("ready".into())
    );
}

#[test]
fn empty_messages_round_trip() {
    let (client_dh, server_dh) = agree();
    let (mut cs, _) = derive_client_session(PSK, &client_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let (_, mut so) = derive_server_session(PSK, &server_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let f = cs.seal_text("").unwrap();
    assert_eq!(so.open(&f).unwrap(), Message::Text(String::new()));
    let f = cs.seal_binary(&[]).unwrap();
    assert_eq!(so.open(&f).unwrap(), Message::Binary(Vec::new()));
}

#[test]
fn mismatched_psk_cannot_open() {
    let (client_dh, server_dh) = agree();
    let (mut cs, _) = derive_client_session(PSK, &client_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let (_, mut so) = derive_server_session(
        b"a-DIFFERENT-32-byte-test-psk!!!!",
        &server_dh,
        &MLKEM_SS,
        HELLO,
        CHALLENGE,
    )
    .unwrap();
    let f = cs.seal_text("secret").unwrap();
    assert!(so.open(&f).is_err());
}

#[test]
fn tampered_frame_fails_without_panic() {
    let (client_dh, server_dh) = agree();
    let (mut cs, _) = derive_client_session(PSK, &client_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let (_, mut so) = derive_server_session(PSK, &server_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let mut f = cs.seal_text("tamper me").unwrap();
    let last = f.len() - 1;
    f[last] ^= 0x01;
    assert!(so.open(&f).is_err());
}

#[test]
fn hybrid_x25519_ml_kem_round_trip_and_depends_on_the_kem_secret() {
    use rmux_web_crypto::ml_kem;

    let (client_dh, server_dh) = agree();

    // ML-KEM-768: client generates the keypair, server encapsulates to its
    // encapsulation key, client decapsulates the ciphertext. Both agree.
    let kem = ml_kem::KeyPair::generate([5u8; ml_kem::KEYGEN_RANDOMNESS_LEN]);
    let ek = kem.encapsulation_key();
    let (ct, server_ss) = ml_kem::encapsulate(&ek, [6u8; ml_kem::ENCAPS_RANDOMNESS_LEN]).unwrap();
    let client_ss = kem.decapsulate(&ct);
    assert_eq!(&*client_ss, &server_ss, "ML-KEM both sides agree");

    // Hybrid session: the same DH + the matching ML-KEM secret. On the wire ek/ct
    // ride in HELLO/CHALLENGE so the transcript binds them; here they stand in.
    let (mut cs, _) = derive_client_session(PSK, &client_dh, &client_ss, HELLO, CHALLENGE).unwrap();
    let (_, mut so) = derive_server_session(PSK, &server_dh, &server_ss, HELLO, CHALLENGE).unwrap();
    let frame = cs.seal_text("post-quantum hello").unwrap();
    assert_eq!(
        so.open(&frame).unwrap(),
        Message::Text("post-quantum hello".into())
    );

    // The ML-KEM secret is load-bearing: a server holding a DIFFERENT ML-KEM
    // secret (same DH + psk) cannot open the client's frame.
    let (mut cs2, _) =
        derive_client_session(PSK, &client_dh, &client_ss, HELLO, CHALLENGE).unwrap();
    let (_, mut wrong_so) =
        derive_server_session(PSK, &server_dh, &MLKEM_SS, HELLO, CHALLENGE).unwrap();
    let hybrid_frame = cs2.seal_text("nope").unwrap();
    assert!(
        wrong_so.open(&hybrid_frame).is_err(),
        "keys must depend on the ML-KEM secret"
    );
}
