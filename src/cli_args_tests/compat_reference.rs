use super::*;

#[test]
fn public_compatibility_reference_files_exist() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    for path in [FROZEN_TMUX_REFERENCE, ERROR_EXIT_MATRIX] {
        assert!(
            root.join(path).is_file(),
            "expected compatibility reference file to exist: {path}"
        );
    }
}

#[test]
fn frozen_reference_records_digest_and_rejects_host_tmux_as_reference() {
    let reference = repo_file(FROZEN_TMUX_REFERENCE);

    for needle in [
        "artifact: frozen_tmux_reference",
        "frozen_tmux_binary_acquisition:",
        "source_sha: \"e802909de06012a4df6209d55e86487c56223163\"",
        "binary_sha256: \"eb05f981dfc0ed55f29b7dc8e13ed838827ebca2764a9fc559ae817d9cf1acd0\"",
        "used_for_tmux_compat_observations: false",
        "baseline_test_floor:",
    ] {
        assert!(
            reference.contains(needle),
            "expected frozen reference to mention {needle}"
        );
    }
}

#[test]
fn error_exit_matrix_records_live_coverage_links() {
    let matrix = repo_file(ERROR_EXIT_MATRIX);

    for needle in [
        "artifact: tmux_compat_error_exit_matrix",
        "wait-for-unlock-missing-channel",
        "wait-for-unlock-signaled-channel",
        "unknown-command",
        "tests/tmux_compat_surface_matrix.rs::",
    ] {
        assert!(
            matrix.contains(needle),
            "expected error/exit matrix to mention {needle}"
        );
    }
}
