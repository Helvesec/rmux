use super::*;

#[test]
fn rejected_direct_reads_keep_selection_but_only_globs_consume_budget() {
    let root = super::tests::temp_source_path("direct-budget");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("directory.conf")).unwrap();
    File::create(root.join("oversized.conf"))
        .unwrap()
        .set_len(MAX_SOURCE_CONFIG_BYTES + 1)
        .unwrap();
    // The raw file is admitted; lossy decoding crosses the 32-MiB bound.
    fs::write(root.join("decoded.conf"), vec![0xff; 12 * 1024 * 1024]).unwrap();
    for name in ["directory", "oversized", "decoded"] {
        for quiet in [false, true] {
            for glob in [false, true] {
                let path = format!("{name}.{}", if glob { "*" } else { "conf" });
                let read = source_inputs_for_path_with_diagnostics(
                    &path,
                    Some(&root),
                    quiet,
                    None,
                    SourceReadPolicy::Strict,
                )
                .unwrap();
                assert_eq!(read.matched_files, 1, "{path}");
                assert_eq!(read.budgeted_files, usize::from(glob), "{path}");
                assert!(read.inputs.is_empty(), "{path}");
                if !glob {
                    assert_eq!(read.content_bytes, 0, "{path}");
                }
                let mut budget = crate::config_paths::ConfigPathMatchBudget::default();
                budget.reserve(128).unwrap();
                budget.reserve(read.budgeted_files).unwrap();
                assert_eq!(budget.reserve(128).is_err(), glob, "{path}");
                let mut loaded = LoadedSourceFile::default();
                loaded.record_matched_files(read.matched_files);
                loaded.push_read_error(read.error.expect("read must fail"));
                assert!(!loaded.selection_failed(), "{path}");
            }
        }
    }
    // Best-effort imports retain their existing accounting of ignored entries.
    let read = source_inputs_for_path_with_diagnostics(
        "directory.conf",
        Some(&root),
        true,
        None,
        SourceReadPolicy::BestEffort,
    )
    .unwrap();
    assert!(read.error.is_none());
    assert_eq!(read.budgeted_files, 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn admitted_direct_reads_consume_budget_before_parsing_or_inspection() {
    let root = super::tests::temp_source_path("admitted-direct-budget");
    fs::create_dir(&root).unwrap();
    let mut decoded_boundary = vec![b'#'];
    decoded_boundary.extend(vec![0xff; (MAX_SOURCE_AGGREGATE_BYTES - 2) / 3]);
    decoded_boundary.push(b'\n');
    for (name, contents) in [
        ("empty.conf", Vec::new()),
        ("parse.conf", b"not-a-command\n".to_vec()),
        ("invalid-utf8.conf", b"#\xff\n".to_vec()),
        ("inspection.conf", "#\n".repeat(512 * 1024 + 1).into_bytes()),
        ("boundary.conf", decoded_boundary.clone()),
    ] {
        fs::write(root.join(name), &contents).unwrap();
        for policy in [SourceReadPolicy::Strict, SourceReadPolicy::BestEffort] {
            let read =
                source_inputs_for_path_with_diagnostics(name, Some(&root), false, None, policy)
                    .unwrap();
            assert!(read.error.is_none(), "{name}");
            assert_eq!(read.matched_files, 1, "{name}");
            assert_eq!(read.budgeted_files, 1, "{name}");
            assert_eq!(read.inputs.len(), 1, "{name}");
            assert_eq!(read.content_bytes, String::from_utf8_lossy(&contents).len());
        }
    }
    decoded_boundary.push(b'\n');
    fs::write(root.join("one-over.conf"), &decoded_boundary).unwrap();
    for policy in [SourceReadPolicy::Strict, SourceReadPolicy::BestEffort] {
        let read = source_inputs_for_path_with_diagnostics(
            "one-over.conf",
            Some(&root),
            false,
            None,
            policy,
        )
        .unwrap();
        assert!(read.error.is_some());
        assert_eq!(read.matched_files, 1);
        assert_eq!(read.budgeted_files, 0);
        assert_eq!(read.content_bytes, 0);
    }
    fs::remove_dir_all(root).unwrap();
}
