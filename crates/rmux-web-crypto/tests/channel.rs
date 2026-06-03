//! End-to-end record-channel tests: round-trip, negative cases, transcript
//! binding, and a frozen output fixture guarding against format drift.

use rmux_web_crypto::{
    derive_client_session, schedule, transcript_hash, Error, RecordOpener, RecordSealer,
};

const PSK: &[u8] = b"a-high-entropy-pre-shared-secret";
const DH: [u8; 32] = [0x11; 32];
const HELLO: &[u8] = b"client-hello-bytes";
const CHALLENGE: &[u8] = b"server-challenge-bytes";

fn derive_pair() -> ((RecordSealer, RecordOpener), (RecordSealer, RecordOpener)) {
    let client_keys = schedule::derive(PSK, &DH, &[0x42u8; 32], HELLO, CHALLENGE).unwrap();
    let server_keys = schedule::derive(PSK, &DH, &[0x42u8; 32], HELLO, CHALLENGE).unwrap();
    (client_keys.into_client(), server_keys.into_server())
}

#[test]
fn round_trip_both_directions() {
    let ((mut c_seal, mut c_open), (mut s_seal, mut s_open)) = derive_pair();

    // Several client-to-server frames, in order.
    let c2s = [b"hello".as_slice(), b"world", b"third frame", b""];
    for msg in c2s {
        let frame = c_seal.seal(msg).unwrap();
        let got = s_open.open(&frame).unwrap();
        assert_eq!(got, msg);
    }

    // Several server-to-client frames, in order.
    let s2c = [b"ack".as_slice(), b"more data", b"done"];
    for msg in s2c {
        let frame = s_seal.seal(msg).unwrap();
        let got = c_open.open(&frame).unwrap();
        assert_eq!(got, msg);
    }
}

#[test]
fn flipped_ciphertext_byte_fails_decrypt() {
    let ((mut c_seal, _), (_, mut s_open)) = derive_pair();
    let mut frame = c_seal.seal(b"sensitive payload").unwrap();
    // Flip a byte inside the ciphertext (past the 9-byte header).
    let last = frame.len() - 1;
    frame[last] ^= 0x01;
    assert_eq!(s_open.open(&frame).unwrap_err(), Error::Decrypt);
}

#[test]
fn replay_same_frame_is_out_of_order() {
    let ((mut c_seal, _), (_, mut s_open)) = derive_pair();
    let frame = c_seal.seal(b"once").unwrap();
    assert_eq!(s_open.open(&frame).unwrap(), b"once");
    // Replaying the identical frame: seq no longer matches next_seq.
    assert_eq!(s_open.open(&frame).unwrap_err(), Error::OutOfOrder);
}

#[test]
fn out_of_order_delivery_rejected() {
    let ((mut c_seal, _), (_, mut s_open)) = derive_pair();
    let f0 = c_seal.seal(b"zero").unwrap();
    let f1 = c_seal.seal(b"one").unwrap();
    // Deliver frame 1 before frame 0.
    assert_eq!(s_open.open(&f1).unwrap_err(), Error::OutOfOrder);
    // Frame 0 still opens (state was not advanced by the rejected frame).
    assert_eq!(s_open.open(&f0).unwrap(), b"zero");
    assert_eq!(s_open.open(&f1).unwrap(), b"one");
}

#[test]
fn mismatched_psk_cannot_open() {
    let client_keys = schedule::derive(PSK, &DH, &[0x42u8; 32], HELLO, CHALLENGE).unwrap();
    let server_keys = schedule::derive(
        b"a-different-pre-shared-secret!!!",
        &DH,
        &[0x42u8; 32],
        HELLO,
        CHALLENGE,
    )
    .unwrap();
    let (mut c_seal, _) = client_keys.into_client();
    let (_, mut s_open) = server_keys.into_server();

    let frame = c_seal.seal(b"payload").unwrap();
    assert_eq!(s_open.open(&frame).unwrap_err(), Error::Decrypt);
}

#[test]
fn all_zero_dh_is_weak() {
    // `SessionKeys` does not implement `Debug` (it holds secrets), so match.
    match schedule::derive(PSK, &[0u8; 32], &[0x42u8; 32], HELLO, CHALLENGE) {
        Err(e) => assert_eq!(e, Error::WeakSharedSecret),
        Ok(_) => panic!("all-zero DH must be rejected"),
    }
}

#[test]
fn truncated_and_wrong_magic_are_malformed() {
    let ((mut c_seal, _), (_, mut s_open)) = derive_pair();
    let frame = c_seal.seal(b"payload").unwrap();

    // Truncated below the minimum frame length.
    let short = &frame[..frame.len().min(20)];
    let truncated = &short[..8.min(short.len())];
    assert_eq!(s_open.open(truncated).unwrap_err(), Error::MalformedFrame);

    // Empty frame.
    assert_eq!(s_open.open(&[]).unwrap_err(), Error::MalformedFrame);

    // Correct length but wrong magic byte.
    let mut wrong_magic = frame.clone();
    wrong_magic[0] = 0x00;
    assert_eq!(
        s_open.open(&wrong_magic).unwrap_err(),
        Error::MalformedFrame
    );
}

#[test]
fn transcript_binding_one_flipped_byte_diverges() {
    // Two derivations differing only by a single flipped byte inside the
    // client_hello (simulating a relay stripping/altering a capability).
    let mut tampered_hello = HELLO.to_vec();
    tampered_hello[0] ^= 0x01;

    let honest = schedule::derive(PSK, &DH, &[0x42u8; 32], HELLO, CHALLENGE).unwrap();
    let tampered = schedule::derive(PSK, &DH, &[0x42u8; 32], &tampered_hello, CHALLENGE).unwrap();

    // The peer that saw the honest bytes seals; the peer that saw the tampered
    // bytes cannot open, because the derived keys differ.
    let (mut honest_seal, _) = honest.into_client();
    let (_, mut tampered_open) = tampered.into_server();
    let frame = honest_seal.seal(b"capability-bound").unwrap();
    assert_eq!(tampered_open.open(&frame).unwrap_err(), Error::Decrypt);

    // Sanity: the underlying transcript hashes differ too.
    assert_ne!(
        transcript_hash(HELLO, CHALLENGE),
        transcript_hash(&tampered_hello, CHALLENGE)
    );
}

#[test]
fn reserialized_handshake_bytes_cannot_open_auth_frame() {
    let psk = b"raw-transcript-binding-psk";
    let dh = [0x24u8; 32];
    let ml_kem = [0x42u8; 32];
    let client_hello = br#"{"type":"hello","protocol_version":1,"capabilities":["e2ee-token-auth"],"token_id":"tok","client_nonce":"nonce","client_public":"pub","client_ml_kem_ek":"ek"}"#;
    let challenge_on_wire = br#"{"type":"challenge","protocol_version":1,"capabilities":["e2ee-token-auth"],"server_nonce":"nonce","server_public":"pub","server_ml_kem_ct":"ct"}"#;

    // Semantically the same JSON as `challenge_on_wire`, but the bytes differ
    // because a parser re-serialized it with different field order/spacing.
    let reserialized_challenge = br#"{"protocol_version":1,"type":"challenge","server_public":"pub","server_nonce":"nonce","server_ml_kem_ct":"ct","capabilities":["e2ee-token-auth"]}"#;

    let client = schedule::derive(psk, &dh, &ml_kem, client_hello, challenge_on_wire).unwrap();
    let server = schedule::derive(psk, &dh, &ml_kem, client_hello, reserialized_challenge).unwrap();

    let (mut client_seal, _) = client.into_client();
    let (_, mut server_open) = server.into_server();
    let frame = client_seal
        .seal(br#"{"type":"auth","pin":"482917"}"#)
        .unwrap();
    assert_eq!(server_open.open(&frame).unwrap_err(), Error::Decrypt);
}

#[test]
fn auth_text_frame_fixture_exact_bytes() {
    let psk = b"interop-v1-token-psk";
    let dh = [0x24u8; 32];
    let ml_kem = [0x42u8; 32];
    let client_hello = br#"{"type":"hello","protocol_version":1,"capabilities":["e2ee-token-auth"],"token_id":"tok","client_nonce":"nonce","client_public":"pub","client_ml_kem_ek":"ek"}"#;
    let server_challenge = br#"{"type":"challenge","protocol_version":1,"capabilities":["e2ee-token-auth"],"server_nonce":"nonce","server_public":"pub","server_ml_kem_ct":"ct"}"#;
    let auth =
        r#"{"type":"auth","protocol_version":1,"capabilities":["e2ee-token-auth"],"pin":"482917"}"#;

    let (mut sealer, _) =
        derive_client_session(psk, &dh, &ml_kem, client_hello, server_challenge).unwrap();
    let frame = sealer.seal_text(auth).unwrap();
    let got_hex: String = frame.iter().map(|byte| format!("{byte:02x}")).collect();

    assert_eq!(
        got_hex,
        "e00000000000000000d5511391a56ab7c68bcec1b0482717a2c24741dc13ea174be4a3bc851eab870629b98e2894629041b5d9a7b5fe7dd29456f5c59ed31e69a26bf44be676c60c1efa8405786b13a17e870b62e23ba2d0c2f488ce753e9ce25fb10f85d1c6fdedf5eea2d652643c45"
    );
}

/// FROZEN FIXTURE.
///
/// With a fixed `(psk, dh, client_hello, server_challenge)`, derive and seal a
/// fixed plaintext at seq 0, 1, 2 and assert the exact output bytes. These were
/// produced by running the implementation; the test guards against future
/// format drift.
#[test]
fn frozen_fixture_exact_bytes() {
    let psk = b"frozen-fixture-psk-32-bytes-ok!!";
    let dh = [0x42u8; 32];
    let client_hello = b"client-hello-fixture";
    let server_challenge = b"server-challenge-fixture";
    let plaintext = b"frozen-plaintext-payload";

    let keys = schedule::derive(psk, &dh, &[0x42u8; 32], client_hello, server_challenge).unwrap();
    let (mut sealer, _) = keys.into_client();

    // Regenerated for the v1 hybrid schedule (ikm now includes the ML-KEM secret).
    let expected: [&str; 3] = [
        "e000000000000000000d9c4dc87188fe68f60c4a6911835ff9d18741f88746fc0fc68f3e122c21f65ed36574e399db2223",
        "e000000000000000010d5ac1dd7e0cf1471022a452d8bd7a182ef55a8f61998a40eb7104c20cceea5fd136ee8641863432",
        "e00000000000000002049d3081564af5984e08bc920f7a49d1753153122800ac164f62e7b7e6765f81ca140648450f41bf",
    ];

    for exp_hex in expected {
        let frame = sealer.seal(plaintext).unwrap();
        let got_hex: String = frame.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got_hex, exp_hex);
    }
}
