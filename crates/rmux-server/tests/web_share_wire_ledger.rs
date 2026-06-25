#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::read_to_string(root.join(relative)).expect("source file is readable")
}

#[test]
fn web_share_protocol_version_and_capabilities_stay_v1_compatible() {
    let protocol = repo_file("crates/rmux-server/src/web/protocol/mod.rs");
    let crypto = repo_file("crates/rmux-server/src/web/crypto.rs");
    let handshake = repo_file("crates/rmux-server/src/web/protocol/handshake.rs");

    assert!(
        protocol.contains("pub(crate) const WEB_SHARE_PROTOCOL_VERSION: u16 = 1;"),
        "web-share protocol version must stay v1 unless a lockstep deploy is planned"
    );
    assert!(
        crypto.contains("pub(super) const E2EE_CAPABILITY: &str = \"e2ee-token-auth\";"),
        "E2EE capability must stay stable"
    );
    for capability in ["terminal-palette-v1", "pane-frame-v1"] {
        assert!(
            protocol.contains(capability),
            "server capability {capability:?} must remain advertised"
        );
    }
    assert!(
        handshake.contains(
            "\"capabilities\":[\"e2ee-token-auth\",\"terminal-palette-v1\",\"pane-frame-v1\"]"
        ),
        "challenge fixture must keep the full v1 capability list"
    );
}

#[test]
fn web_share_binary_opcodes_stay_stable() {
    let protocol = repo_file("crates/rmux-server/src/web/protocol/mod.rs");

    for constant in [
        "const WS_OUTPUT_RAW: u8 = 0x01;",
        "const WS_RESIZE_NOTIFY: u8 = 0x02;",
        "const WS_SNAPSHOT_FULL: u8 = 0x10;",
        "const WS_SESSION_VIEW: u8 = 0x11;",
        "const WS_SESSION_PANE_FRAME: u8 = 0x12;",
        "const WS_INPUT_TEXT: u8 = 0x80;",
        "const WS_INPUT_KEY: u8 = 0x81;",
        "const WS_RESIZE_REQUEST: u8 = 0x82;",
        "const WS_ATTACH_INPUT: u8 = 0x83;",
        "const WS_SESSION_RESIZE_PANE: u8 = 0x84;",
    ] {
        assert!(
            protocol.contains(constant),
            "web-share opcode changed or moved without ledger update: {constant}"
        );
    }
}

#[test]
fn web_share_resource_caps_are_enforced_in_protocol_and_accept_path() {
    let protocol = repo_file("crates/rmux-server/src/web/protocol/mod.rs");
    let outbound = repo_file("crates/rmux-server/src/web/outbound.rs");
    let server = repo_file("crates/rmux-server/src/web/server.rs");

    for cap in [
        "const MAX_SESSION_RESIZE_DIMENSION: u16 = 4096;",
        "const MAX_SESSION_RESIZE_CELLS: u32 = 1_000_000;",
        "const MAX_PANE_RESIZE_CELLS: u16 = 10_000;",
    ] {
        assert!(
            protocol.contains(cap),
            "missing protocol resource cap: {cap}"
        );
    }
    assert!(
        outbound.contains("const BACKLOG_BYTES_MAX: usize = 2 * 1024 * 1024;"),
        "web outbound backlog budget must stay explicit"
    );
    assert!(
        server.contains("const PRE_AUTH_SLOTS_PER_IP: usize = 4;"),
        "pre-auth per-IP cap must stay explicit"
    );
}

#[test]
fn web_share_close_codes_and_handshake_rejection_stay_stable() {
    let protocol = repo_file("crates/rmux-server/src/web/protocol/mod.rs");
    let streams = repo_file("crates/rmux-server/src/web/server/streams.rs");

    assert!(
        protocol.contains(
            "pub(crate) const HANDSHAKE_REJECTED: (u16, &str) = (4000, \"handshake_rejected\");"
        ),
        "pre-auth failures must keep the uniform public close pair"
    );
    assert!(
        streams.contains("const SLOW_VIEWER_CLOSE_CODE: u16 = 4001;"),
        "slow-viewer close code must stay stable"
    );
}
