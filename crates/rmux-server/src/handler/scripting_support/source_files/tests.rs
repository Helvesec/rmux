use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use super::glob_pattern_for_source_path;

use super::{
    source_inputs_for_path, source_inputs_for_path_with_diagnostics, LoadedSourceFile,
    SourceReadPolicy,
};
use rmux_proto::RmuxError;

#[test]
fn source_file_stdin_preserves_utf8_bom_like_tmux() {
    let inputs = source_inputs_for_path(
        "-",
        None,
        false,
        Some("\u{feff}set -g status off"),
        SourceReadPolicy::Strict,
    )
    .expect("stdin source should load");

    assert_eq!(inputs[0].contents, "\u{feff}set -g status off");
}

#[test]
fn source_file_path_preserves_utf8_bom_like_tmux() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-source-bom-{}-{unique}.conf",
        std::process::id()
    ));
    std::fs::write(&path, "\u{feff}set -g status-left ok").expect("write source file");

    let inputs = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect("file source should load");
    let _ = std::fs::remove_file(&path);

    assert_eq!(inputs[0].contents, "\u{feff}set -g status-left ok");
}

#[test]
fn source_file_path_decodes_reversed_bom_lossily_like_tmux() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-source-reversed-bom-{}-{unique}.conf",
        std::process::id()
    ));
    std::fs::write(&path, b"\xff\xfeset -g status-left ok").expect("write source file");

    let inputs = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect("file source should load");
    let _ = std::fs::remove_file(&path);

    assert_eq!(inputs[0].contents, "\u{fffd}\u{fffd}set -g status-left ok");
}

#[test]
fn tmux_best_effort_source_skips_oversized_files() {
    let path = temp_source_path("oversized-tmux-fallback");
    let contents = "x".repeat((super::MAX_SOURCE_CONFIG_BYTES + 1) as usize);
    std::fs::write(&path, contents).expect("write oversized source file");

    let inputs = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::BestEffort,
    )
    .expect("best-effort tmux source should skip oversized files");
    let _ = std::fs::remove_file(&path);

    assert!(inputs.is_empty());
}

#[test]
fn strict_source_rejects_oversized_files() {
    let path = temp_source_path("oversized-strict-source");
    let contents = "x".repeat((super::MAX_SOURCE_CONFIG_BYTES + 1) as usize);
    std::fs::write(&path, contents).expect("write oversized source file");

    let error = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect_err("strict source should reject oversized files");
    let _ = std::fs::remove_file(&path);

    assert!(
        error.to_string().contains("source file exceeds 16 MiB"),
        "unexpected error: {error}"
    );
}

#[test]
fn strict_source_accepts_multi_megabyte_configs_below_limit() {
    let path = temp_source_path("large-strict-source");
    let contents = format!(
        "{}set -g status off\n",
        "# large config padding\n".repeat(80_000)
    );
    assert!(
        contents.len() as u64 > 1024 * 1024,
        "test fixture should exceed the old 1 MiB limit"
    );
    assert!(
        contents.len() as u64 <= super::MAX_SOURCE_CONFIG_BYTES,
        "test fixture should stay below the current source-file limit"
    );
    std::fs::write(&path, contents.clone()).expect("write large source file");

    let inputs = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect("strict source should accept multi-megabyte configs below the hard limit");
    let _ = std::fs::remove_file(&path);

    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].contents, contents);
}

#[test]
fn strict_source_rejects_directories_with_directory_error() {
    let path = temp_source_path("directory-strict-source");
    std::fs::create_dir(&path).expect("create directory source entry");

    let error = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect_err("strict source should reject directories");
    let _ = std::fs::remove_dir(&path);

    assert!(
        error.to_string().contains("Input/output error"),
        "unexpected error: {error}"
    );
}

#[test]
fn strict_source_glob_reports_directories_and_reads_regular_entries() {
    let root = temp_source_path("glob-with-directory-strict-source");
    std::fs::create_dir(&root).expect("create glob source root");
    let first = root.join("a.conf");
    let second = root.join("b.conf");
    let directory = root.join("nested");
    std::fs::write(&first, "set -g @a yes\n").expect("write first source");
    std::fs::write(&second, "set -g @b yes\n").expect("write second source");
    std::fs::create_dir(&directory).expect("create directory glob match");

    let read = source_inputs_for_path_with_diagnostics(
        &format!("{}/*", root.to_string_lossy()),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect("strict glob source should keep readable matches");
    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
    let _ = std::fs::remove_dir(&directory);
    let _ = std::fs::remove_dir(&root);

    let contents = read
        .inputs
        .iter()
        .map(|input| input.contents.as_str())
        .collect::<Vec<_>>();
    assert_eq!(read.inputs.len(), 2);
    assert!(contents.contains(&"set -g @a yes\n"));
    assert!(contents.contains(&"set -g @b yes\n"));
    let error = read.error.expect("globbed directory should be reported");
    assert!(
        error.to_string().contains("Input/output error"),
        "unexpected error: {error}"
    );
}

#[test]
fn source_file_glob_rejects_excessive_match_count() {
    let root = temp_source_path("glob-match-limit");
    std::fs::create_dir(&root).expect("create glob match root");
    for index in 0..=super::MAX_SOURCE_MATCHED_FILES {
        std::fs::write(root.join(format!("{index:04}.conf")), b"").expect("write glob match");
    }

    let read = source_inputs_for_path_with_diagnostics(
        &format!("{}/*.conf", root.to_string_lossy()),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect("match-limit rejection preserves selection metadata");
    std::fs::remove_dir_all(&root).expect("remove glob match root");
    assert_eq!(read.matched_files, super::MAX_SOURCE_MATCHED_FILES + 1);
    assert!(read.inputs.is_empty());
    let error = read
        .error
        .expect("glob match count beyond the cap must fail");

    assert!(
        error.to_string().contains("matched too many files"),
        "{error}"
    );
}

#[test]
fn source_file_aggregate_content_limit_fails_before_retention() {
    assert_eq!(
        super::reserve_source_contents(super::MAX_SOURCE_AGGREGATE_BYTES - 1, 1, "*.conf")
            .expect("exact aggregate limit is accepted"),
        super::MAX_SOURCE_AGGREGATE_BYTES
    );
    let error = super::reserve_source_contents(super::MAX_SOURCE_AGGREGATE_BYTES, 1, "*.conf")
        .expect_err("aggregate content over the cap must fail");
    assert!(error.to_string().contains("aggregate bytes"), "{error}");
}

#[cfg(unix)]
#[test]
fn strict_source_rejects_fifo_without_blocking() {
    let path = temp_source_path("fifo-strict-source");
    create_test_fifo(&path);

    let error = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::Strict,
    )
    .expect_err("strict source should reject fifo");
    let _ = std::fs::remove_file(&path);

    assert!(
        error
            .to_string()
            .contains("source file is not a regular file"),
        "unexpected error: {error}"
    );
}

#[test]
fn tmux_best_effort_source_skips_non_regular_files() {
    let path = temp_source_path("non-regular-tmux-fallback");
    std::fs::create_dir(&path).expect("create non-regular source entry");

    let inputs = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::BestEffort,
    )
    .expect("best-effort tmux source should skip non-regular files");
    let _ = std::fs::remove_dir(&path);

    assert!(inputs.is_empty());
}

#[cfg(unix)]
#[test]
fn tmux_best_effort_source_skips_fifo_without_blocking() {
    let path = temp_source_path("fifo-tmux-fallback");
    create_test_fifo(&path);

    let inputs = source_inputs_for_path(
        &path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::BestEffort,
    )
    .expect("best-effort tmux source should skip fifo");
    let _ = std::fs::remove_file(&path);

    assert!(inputs.is_empty());
}

#[cfg(unix)]
#[test]
fn tmux_best_effort_source_skips_symlink_to_fifo_without_blocking() {
    let fifo_path = temp_source_path("symlink-target-fifo-tmux-fallback");
    let symlink_path = temp_source_path("symlink-tmux-fallback");
    create_test_fifo(&fifo_path);
    std::os::unix::fs::symlink(&fifo_path, &symlink_path).expect("create source symlink");

    let inputs = source_inputs_for_path(
        &symlink_path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::BestEffort,
    )
    .expect("best-effort tmux source should skip symlink to fifo");
    let _ = std::fs::remove_file(&symlink_path);
    let _ = std::fs::remove_file(&fifo_path);

    assert!(inputs.is_empty());
}

#[cfg(unix)]
#[test]
fn tmux_best_effort_source_accepts_symlink_to_regular_file() {
    let target_path = temp_source_path("symlink-target-regular-tmux-fallback");
    let symlink_path = temp_source_path("regular-symlink-tmux-fallback");
    std::fs::write(&target_path, "set -g status off\n").expect("write target config");
    std::os::unix::fs::symlink(&target_path, &symlink_path).expect("create source symlink");

    let inputs = source_inputs_for_path(
        &symlink_path.to_string_lossy(),
        None,
        false,
        None,
        SourceReadPolicy::BestEffort,
    )
    .expect("best-effort tmux source should accept regular-file symlinks");
    let _ = std::fs::remove_file(&symlink_path);
    let _ = std::fs::remove_file(&target_path);

    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].contents, "set -g status off\n");
}

#[cfg(unix)]
#[test]
fn fallback_path_dedupe_collapses_symlinked_entries() {
    let target_path = temp_source_path("fallback-dedupe-target");
    let symlink_path = temp_source_path("fallback-dedupe-symlink");
    let missing_path = temp_source_path("fallback-dedupe-missing");
    std::fs::write(&target_path, "set -g status off\n").expect("write target config");
    std::os::unix::fs::symlink(&target_path, &symlink_path).expect("create source symlink");

    let deduped = super::dedupe_existing_source_paths(vec![
        target_path.to_string_lossy().into_owned(),
        symlink_path.to_string_lossy().into_owned(),
        missing_path.to_string_lossy().into_owned(),
    ]);
    let _ = std::fs::remove_file(&symlink_path);
    let _ = std::fs::remove_file(&target_path);

    assert_eq!(
        deduped,
        vec![
            target_path.to_string_lossy().into_owned(),
            missing_path.to_string_lossy().into_owned()
        ],
        "fallback discovery must not source the same real file twice"
    );
}

#[test]
fn loaded_source_file_tracks_errors_for_fallback_gating() {
    let mut loaded = LoadedSourceFile::default();
    assert!(!loaded.loaded_any_file());
    assert!(!loaded.has_errors());

    loaded.push_error(RmuxError::Server("permission denied".to_owned()));

    assert!(!loaded.loaded_any_file());
    assert!(loaded.has_errors());
}

#[cfg(windows)]
#[test]
fn windows_relative_source_file_uses_glob_safe_separators() {
    let pattern = glob_pattern_for_source_path(
        "nested\\*.conf",
        Some(std::path::Path::new(r"C:\Users\RMUXUser\rmux")),
    );

    assert_eq!(pattern, "C:/Users/RMUXUser/rmux/nested/*.conf");
}

#[cfg(windows)]
#[test]
fn windows_absolute_source_file_uses_forward_slashes() {
    let pattern = glob_pattern_for_source_path(r"C:\Users\RMUXUser\rmux\config.conf", None);

    assert_eq!(pattern, "C:/Users/RMUXUser/rmux/config.conf");
}

#[cfg(windows)]
#[test]
fn windows_null_device_config_paths_are_ignored() {
    assert!(super::is_windows_null_config_path("NUL"));
    assert!(super::is_windows_null_config_path("nul:"));
    assert!(super::is_windows_null_config_path(r"C:\tmp\NUL"));
    assert!(super::is_windows_null_config_path(r"C:\tmp\NUL.conf"));
    assert!(super::is_windows_null_config_path(r"\\.\NUL"));
    assert!(!super::is_windows_null_config_path(r"C:\tmp\null.conf"));
    assert!(!super::is_windows_null_config_path(r"C:\tmp\nulled"));
}

#[cfg(unix)]
#[test]
fn unix_dev_null_config_path_is_empty() {
    let inputs = source_inputs_for_path("/dev/null", None, false, None, SourceReadPolicy::Strict)
        .expect("/dev/null should behave like an empty config file");

    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].current_file, "/dev/null");
    assert!(inputs[0].contents.is_empty());
}

pub(super) fn temp_source_path(label: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-source-{label}-{}-{unique}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn create_test_fifo(path: &std::path::Path) {
    let output = std::process::Command::new("mkfifo")
        .arg(path)
        .output()
        .expect("run mkfifo");
    assert!(
        output.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_selection_count_survives_read_errors_and_empty_files() {
    let root = temp_source_path("selection-vs-read");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("empty.conf"), "").unwrap();
    std::fs::create_dir(root.join("directory.conf")).unwrap();
    for (name, quiet, matches, has_error) in [
        ("missing.conf", false, 0, true),
        ("missing.conf", true, 0, false),
        ("missing-*.conf", false, 0, true),
        ("empty.conf", false, 1, false),
        ("directory.conf", false, 1, true),
        ("*.conf", false, 2, true),
    ] {
        let read = source_inputs_for_path_with_diagnostics(
            name,
            Some(&root),
            quiet,
            None,
            SourceReadPolicy::Strict,
        )
        .unwrap();
        assert_eq!(read.matched_files, matches, "{name}");
        assert_eq!(
            read.budgeted_files,
            if name == "directory.conf" { 0 } else { matches },
            "{name}"
        );
        assert_eq!(read.error.is_some(), has_error, "{name}");
        let mut loaded = LoadedSourceFile::default();
        loaded.record_matched_files(read.matched_files);
        if let Some(error) = read.error {
            loaded.push_read_error(error);
        }
        assert_eq!(
            loaded.selection_failed(),
            has_error && matches == 0,
            "{name}"
        );
        // Diagnostics are unchanged for callers of the historical helper.
        assert_eq!(
            source_inputs_for_path(name, Some(&root), quiet, None, SourceReadPolicy::Strict)
                .is_err(),
            has_error,
            "{name}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
