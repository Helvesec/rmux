//! The v9 cut activates control format subscriptions without changing DTO
//! layouts. Preserve the last v8 ledger, including RuntimeCommandExecution's
//! embedded v8 handshake, and keep its envelope rejected.

use std::collections::BTreeMap;
use std::path::Path;

use rmux_proto::{
    decode_frame, encode_frame, FrameDecoder, Request, Response, RmuxError,
    CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION,
};
use serde::{de::DeserializeOwned, Serialize};

const CURRENT: &str = "wire-fixtures/ledger-v1-current-wire";
const V8: &str = "reference/wire/v8/ledger-v1-current-wire";
const V7: &str = "reference/wire/v7/ledger-v1-current-wire";
const RUNTIME_REQUEST: &str = "runtime_command_execution_request.bin";

fn read_ledger(directory: &str, count: usize) -> BTreeMap<String, Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests")
        .join(directory);
    let mut fixtures = BTreeMap::new();
    for entry in std::fs::read_dir(&root).expect("read fixture ledger") {
        let path = entry.expect("read fixture entry").path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("bin") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(name.ends_with("_request.bin") || name.ends_with("_response.bin"));
        let bytes = std::fs::read(&path).expect("read fixture");
        assert_eq!(bytes.first(), Some(&RMUX_FRAME_MAGIC), "{name}");
        let length = u32::from_le_bytes(bytes[2..6].try_into().unwrap()) as usize;
        assert_eq!(length + 6, bytes.len(), "{name}");
        fixtures.insert(name, bytes);
    }
    assert_eq!(
        fixtures.len(),
        count,
        "{directory}: complete fixture ledger"
    );
    fixtures
}

#[test]
fn complete_v8_ledger_is_preserved_and_v9_changes_only_version_fields() {
    assert_eq!(RMUX_WIRE_VERSION, 9);
    let historical = read_ledger(V8, 33);
    let current = read_ledger(CURRENT, 42);
    assert_eq!(
        historical.keys().collect::<Vec<_>>(),
        current
            .keys()
            .filter(historical_comparison_fixture)
            .collect::<Vec<_>>()
    );
    for (name, old) in historical {
        let new = &current[&name];
        assert_eq!(old[1], 8, "{name}: frozen v8 envelope");
        assert_eq!(new[1], 9, "{name}: v9 envelope");
        // Expected bytes come from the preserved v8 corpus, not encode_frame.
        let mut expected = old.clone();
        expected[1] = 9;
        if name == RUNTIME_REQUEST {
            // After the six-byte envelope and tag 131, the offer begins with
            // HandshakeRequest's two little-endian u32 version bounds.
            // HandshakeRequest::requiring uses the current wire for both.
            assert_eq!(&old[6..10], &131_u32.to_le_bytes());
            for offset in [10, 14] {
                assert_eq!(&old[offset..offset + 4], &8_u32.to_le_bytes());
                expected[offset..offset + 4].copy_from_slice(&9_u32.to_le_bytes());
            }
        }
        assert_eq!(
            new, &expected,
            "{name}: only documented version fields change"
        );
    }
}

#[test]
fn runtime_offer_requires_current_envelope_and_valid_embedded_handshake() {
    let fixtures = read_ledger(CURRENT, 42);
    let current = &fixtures[RUNTIME_REQUEST];
    let Request::RuntimeCommandExecution(offer) = decode_frame::<Request>(current).unwrap() else {
        panic!("runtime execution request tag");
    };
    assert_eq!(offer.handshake.minimum_wire_version, 9);
    assert_eq!(offer.handshake.maximum_wire_version, 9);
    assert_eq!(
        offer.handshake.required_capabilities,
        [CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION]
    );
    offer
        .handshake
        .validate_against(&[CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION])
        .unwrap();

    // A v9 envelope does not repair an embedded v8-only offer. It decodes but
    // fails negotiation, so patching just the envelope is insufficient.
    let mut stale_offer = current.clone();
    stale_offer[10..14].copy_from_slice(&8_u32.to_le_bytes());
    stale_offer[14..18].copy_from_slice(&8_u32.to_le_bytes());
    let Request::RuntimeCommandExecution(stale) = decode_frame::<Request>(&stale_offer).unwrap()
    else {
        panic!("runtime execution request tag");
    };
    assert!(matches!(
        stale
            .handshake
            .validate_against(&[CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION]),
        Err(RmuxError::UnsupportedWireVersion {
            got: 9,
            minimum: 8,
            maximum: 8
        })
    ));

    // Conversely a current embedded offer cannot admit an old envelope.
    let mut old_envelope = current.clone();
    old_envelope[1] = 8;
    assert_unsupported(decode_frame::<Request>(&old_envelope), 8, RUNTIME_REQUEST);
}

#[test]
fn historical_v7_and_v8_ledgers_are_rejected_before_payload_decoding() {
    let current = read_ledger(CURRENT, 42);
    for (directory, count, version) in [(V7, 31, 7), (V8, 33, 8)] {
        for (name, bytes) in read_ledger(directory, count) {
            assert_eq!(bytes[1], version as u8, "{name}");
            assert_rejection_and_recovery(&name, &bytes, version, &current[&name]);
        }
    }
}

#[test]
fn future_v10_envelopes_are_rejected_before_payload_decoding() {
    for (name, current) in read_ledger(CURRENT, 42) {
        let mut future = current.clone();
        future[1] = 10;
        assert_rejection_and_recovery(&name, &future, 10, &current);
    }
}

fn historical_comparison_fixture(name: &&String) -> bool {
    let name = name.as_str();
    // Frames introduced after the frozen v8 corpus have no historical twin to
    // compare against: the new-pane input offer is one of them.
    !name.starts_with("split_window_command_v1_")
        && !name.starts_with("new_pane_input_v1_")
        && !matches!(
            name,
            "runtime_command_execution_v2_request.bin"
                | "export_window_snapshot_request.bin"
                | "restore_window_snapshot_request.bin"
                | "export_window_snapshot_response.bin"
                | "restore_window_snapshot_response.bin"
        )
}

fn assert_rejection_and_recovery(name: &str, old: &[u8], version: u32, current: &[u8]) {
    if name.ends_with("_request.bin") {
        assert_typed_rejection_and_recovery::<Request>(name, old, version, current);
    } else {
        assert_typed_rejection_and_recovery::<Response>(name, old, version, current);
    }
}

fn assert_typed_rejection_and_recovery<T>(name: &str, old: &[u8], version: u32, current: &[u8])
where
    T: DeserializeOwned + Serialize,
{
    // Both a full frame and just magic/version must hit the version check,
    // even when a DTO happens to have the same payload layout as v9.
    for rejected in [old, &old[..2]] {
        assert_unsupported(decode_frame::<T>(rejected), version, name);
        let mut decoder = FrameDecoder::new();
        decoder.push_bytes(rejected);
        assert_unsupported(decoder.next_frame::<T>(), version, name);
        assert!(decoder.remaining_bytes().is_empty(), "{name}");
        assert_current_roundtrip::<T>(&mut decoder, current, name);
    }

    // Poll between the fragmented magic and version bytes. The rejection
    // must clear the decoder before a fresh current frame can be accepted.
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&old[..1]);
    assert!(decoder.next_frame::<T>().unwrap().is_none(), "{name}");
    decoder.push_bytes(&old[1..2]);
    assert_unsupported(decoder.next_frame::<T>(), version, name);
    assert!(decoder.remaining_bytes().is_empty(), "{name}");
    assert_current_roundtrip::<T>(&mut decoder, current, name);
}

fn assert_unsupported<T>(result: Result<T, RmuxError>, version: u32, name: &str) {
    assert!(
        matches!(result, Err(RmuxError::UnsupportedWireVersion {
            got,
            minimum: 9,
            maximum: 9,
        }) if got == version),
        "{name}: expected unsupported v{version} with exact supported range 9..=9"
    );
}

fn assert_current_roundtrip<T>(decoder: &mut FrameDecoder, current: &[u8], name: &str)
where
    T: DeserializeOwned + Serialize,
{
    let decoded: T = decode_frame(current).expect("v9 frame decodes");
    assert_eq!(encode_frame(&decoded).unwrap(), current, "{name}");
    for (index, byte) in current.iter().enumerate() {
        decoder.push_bytes(std::slice::from_ref(byte));
        let decoded = decoder.next_frame::<T>().expect("v9 byte stream decodes");
        if index + 1 == current.len() {
            assert_eq!(
                encode_frame(&decoded.expect("complete v9 frame")).unwrap(),
                current,
                "{name}"
            );
        } else {
            assert!(decoded.is_none(), "{name}: premature frame at byte {index}");
        }
    }
    assert!(decoder.remaining_bytes().is_empty(), "{name}");
    assert!(decoder.next_frame::<T>().unwrap().is_none(), "{name}");
}
