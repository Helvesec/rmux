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
fn public_spec_reference_the_compatibility_reference() {
    let overview = repo_file("public overview");
    let specification = repo_file("spec/specification.txt");
    let manpage = repo_file("rmux.1");

    for needle in [
        "tests/tmux_compat_harness.rs",
        "tests/tmux_compat_surface_matrix.rs",
        FROZEN_TMUX_REFERENCE,
        ERROR_EXIT_MATRIX,
        "31d77e29b6c9fbb07d032018da78db3a8a38d979",
    ] {
        assert!(
            overview.contains(needle) || specification.contains(needle) || manpage.contains(needle),
            "expected public docs to mention {needle}"
        );
    }
}

#[test]
fn frozen_reference_records_digest_and_rejects_host_tmux_as_reference() {
    let reference = repo_file(FROZEN_TMUX_REFERENCE);

    for needle in [
        "artifact: frozen_tmux_reference",
        "frozen_tmux_binary_acquisition:",
        "source_sha: \"31d77e29b6c9fbb07d032018da78db3a8a38d979\"",
        "binary_sha256: \"525149cdac8d41b7e60ad68c4bab0670c8b769c646bab780e2a7b66239ad83a0\"",
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
