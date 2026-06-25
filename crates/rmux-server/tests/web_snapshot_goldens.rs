#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

fn web_snapshot_source() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::read_to_string(root.join("crates/rmux-server/src/handler_web_snapshot.rs"))
        .expect("web snapshot source is readable")
}

#[test]
fn web_snapshot_golden_coverage_matrix_is_present() {
    let source = web_snapshot_source();

    for fixture in [
        "web_snapshot_golden_default_modes_and_cursor_are_byte_stable",
        "web_snapshot_reasserts_dec_modes_and_scroll_region",
        "web_snapshot_resets_stale_modes_when_target_is_normal_screen",
        "web_session_snapshot_clears_saved_lines_before_rendering",
        "web_session_snapshot_reasserts_active_pane_modes",
        "web_snapshot_capture_preserves_screen_sequences",
    ] {
        assert!(
            source.contains(fixture),
            "missing web snapshot golden fixture: {fixture}"
        );
    }
}

#[test]
fn web_snapshot_reset_prefix_stays_pinned() {
    let source = web_snapshot_source();

    assert!(
        source.contains("SNAPSHOT_RESET_PREFIX"),
        "web snapshot reset prefix must stay centralized"
    );
    for sequence in [
        "\\x1b[?2026l",
        "\\x1b[?1049l",
        "\\x1b[?6l",
        "\\x1b[r",
        "\\x1b[3J",
        "\\x1b[2J",
        "\\x1b[H",
    ] {
        assert!(
            source.contains(sequence),
            "web snapshot reset prefix lost sequence {sequence}"
        );
    }
}
