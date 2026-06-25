#![cfg(unix)]

use std::fs;
use std::path::PathBuf;

fn renderer_tests_source() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::read_to_string(root.join("crates/rmux-server/src/renderer/tests.rs"))
        .expect("renderer tests source is readable")
}

#[test]
fn attach_render_golden_coverage_matrix_is_present() {
    let source = renderer_tests_source();

    for fixture in [
        "attach_render_golden_normal_idle_pane_is_byte_stable",
        "pane_render_applies_window_style_to_default_cells",
        "pane_render_active_style_overlays_window_style_for_default_cells",
        "pane_render_keeps_padding_for_split_panes_to_avoid_clearing_neighbors",
        "two_pane_sessions_render_the_main_vertical_border_column_and_exact_frame_bytes",
        "rendered_pane_line_truncates_to_pane_width_without_counting_sgr",
    ] {
        assert!(
            source.contains(fixture),
            "missing attach render golden fixture: {fixture}"
        );
    }
}

#[test]
fn attach_render_golden_exact_frame_assertion_stays_exact() {
    let source = renderer_tests_source();

    assert!(
        source.contains("assert_eq!(\n        super::render_pane_screen"),
        "attach render golden must compare exact bytes, not only substrings"
    );
    assert!(
        source.contains("\\x1b[s\\x1b[0m\\x1b[1;1H\\x1b[0mD\\x1b[0m\\x1b[K"),
        "normal idle pane golden must keep cursor-save/reset/clear bytes pinned"
    );
    assert!(
        source.contains("\\x1b[0m\\x1b[u"),
        "normal idle pane golden must keep cursor restore bytes pinned"
    );
}
