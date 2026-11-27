use super::*;

#[test]
fn glob_budget_rejection_keeps_selection_and_discards_all_inputs() {
    let root = super::tests::temp_source_path("aggregate-selection");
    fs::create_dir(&root).unwrap();
    let chunk = format!("#{}\n", "a".repeat(1022)).repeat(11 * 1024);
    for name in ["a.conf", "b.conf", "c.conf"] {
        fs::write(root.join(name), &chunk).unwrap();
    }
    for policy in [SourceReadPolicy::Strict, SourceReadPolicy::BestEffort] {
        for quiet in [false, true] {
            let read =
                source_inputs_for_path_with_diagnostics("*.conf", Some(&root), quiet, None, policy)
                    .unwrap();
            assert_eq!(read.matched_files, 3);
            assert_eq!(read.budgeted_files, 3);
            assert_eq!(read.content_bytes, chunk.len() * 3);
            assert!(read.content_bytes > MAX_SOURCE_AGGREGATE_BYTES);
            assert!(
                read.inputs.is_empty(),
                "rejected group must not run partially"
            );
            let error = read
                .error
                .expect("aggregate budget still rejects the group");
            assert!(error.to_string().contains("aggregate bytes"));
            let mut loaded = LoadedSourceFile::default();
            loaded.record_matched_files(read.matched_files);
            loaded.push_read_error(error);
            assert!(!loaded.selection_failed());
            assert!(loaded.has_errors());
        }
        let mut count = 0;
        let mut bytes = 0;
        for name in ["a.conf", "b.conf", "c.conf"] {
            let read =
                source_inputs_for_path_with_diagnostics(name, Some(&root), false, None, policy)
                    .unwrap();
            assert!(read.error.is_none());
            assert_eq!(read.inputs.len(), 1);
            count += read.matched_files;
            bytes += read.content_bytes;
        }
        assert_eq!(count, 3);
        assert_eq!(bytes, chunk.len() * 3);
        assert!(bytes > MAX_SOURCE_AGGREGATE_BYTES);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn glob_accepts_exact_aggregate_boundary_and_rejects_one_more_byte() {
    let root = super::tests::temp_source_path("aggregate-boundary");
    fs::create_dir(&root).unwrap();
    let chunk = "#".repeat(MAX_SOURCE_AGGREGATE_BYTES / 2);
    for name in ["a.conf", "b.conf"] {
        fs::write(root.join(name), &chunk).unwrap();
    }
    for extra in [false, true] {
        if extra {
            fs::write(root.join("c.conf"), "#").unwrap();
        }
        for policy in [SourceReadPolicy::Strict, SourceReadPolicy::BestEffort] {
            let read =
                source_inputs_for_path_with_diagnostics("*.conf", Some(&root), false, None, policy)
                    .unwrap();
            assert_eq!(read.matched_files, if extra { 3 } else { 2 });
            assert_eq!(read.budgeted_files, read.matched_files);
            assert_eq!(
                read.content_bytes,
                MAX_SOURCE_AGGREGATE_BYTES + usize::from(extra)
            );
            assert_eq!(read.error.is_some(), extra);
            assert_eq!(read.inputs.len(), if extra { 0 } else { 2 });
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn oversized_stdin_keeps_selection_without_retaining_contents() {
    let contents = "#".repeat(MAX_SOURCE_AGGREGATE_BYTES + 1);
    let read = source_inputs_for_path_with_diagnostics(
        "-",
        None,
        false,
        Some(&contents),
        SourceReadPolicy::Strict,
    )
    .unwrap();
    assert_eq!(read.matched_files, 1);
    assert_eq!(read.budgeted_files, 1);
    assert_eq!(read.content_bytes, contents.len());
    assert!(read.inputs.is_empty());
    assert!(read.error.unwrap().to_string().contains("aggregate bytes"));
}

#[test]
fn interrupted_glob_preserves_prior_matches_and_bytes_without_partial_inputs() {
    let path = super::tests::temp_source_path("interrupted-glob");
    let contents = "set -g @must-not-run yes\n";
    fs::write(&path, contents).unwrap();
    for policy in [SourceReadPolicy::Strict, SourceReadPolicy::BestEffort] {
        for selected in [false, true] {
            let entries = selected
                .then(|| Ok(path.clone()))
                .into_iter()
                .chain(std::iter::once(Err(RmuxError::Server(
                    "source-file glob failed: injected".into(),
                ))));
            let read = read_source_entries("*.conf", entries, false, policy);
            assert_eq!(read.matched_files, usize::from(selected));
            assert_eq!(read.budgeted_files, read.matched_files);
            assert_eq!(
                read.content_bytes,
                if selected { contents.len() } else { 0 }
            );
            assert!(read.inputs.is_empty());
            let error = read.error.unwrap();
            assert_eq!(
                error.to_string(),
                "server error: source-file glob failed: injected"
            );
            let mut loaded = LoadedSourceFile::default();
            loaded.record_matched_files(read.matched_files);
            loaded.push_read_error(error);
            assert_eq!(loaded.selection_failed(), !selected);
        }
    }
    fs::remove_file(path).unwrap();
}
