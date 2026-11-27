//! Frozen regression contract for the D146 retirement and reference classes.

use super::*;

const COMMIT: &str = "commit:7ad3957f2f7839dc38138210576a91a7ac47ad63";
const RECEIPT: &str = "R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/oracle-independent.json";
const SUMMARY: &str =
    "RETIRED 2026-10-16 (D146): trailing bare format markers are consumed, matching tmux 3.7b";
const LIMITATION: &str = "Former divergence: A trailing bare format marker remains literal; explicitly retired by D146 after R2-197 measured tmux 3.7b on display-message -p and list-panes -F";
/// Tracked copies cited by 8be758a7; they stay current, resolvable references.
const TRACKED_EVIDENCE: [&str; 2] = [
    "tests/reference/tmux_compat/R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/contract-evidence.json",
    "tests/reference/tmux_compat/R2-D9-MACRED032-TRAILING-HASH-ORACLE-197/oracle-independent.json",
];
const REFLOW_TESTS: [&str; 4] = [
    "crates/rmux-core/src/screen/tests.rs::width_resize_keeps_cursor_content_visible_when_only_empty_tail_rows_move_product_divergence",
    "crates/rmux-core/src/screen/tests.rs::zero_history_reflow_overflow_never_becomes_scrollable_history_product_divergence",
    "crates/rmux-core/src/screen/tests.rs::reflow_overflow_capacity_freezes_on_normal_output_after_growth_product_divergence",
    "crates/rmux-core/src/screen/tests.rs::reflow_output_capacity_stays_bounded_but_height_transfers_preserve_rows",
];
const PARITY_TEST: &str = "crates/rmux-core/src/formats/tests/transformations.rs::trailing_bare_hash_stops_expansion_like_tmux";

fn retired_fixture() -> String {
    format!(
        "schema = 1\n\
         [policy]\n\
         oracle = 'tmux 3.7b'\n\
         unlisted_divergences_are_bugs = true\n\
         [[entry]]\n\
         id = 'C-D36'\n\
         summary = '{SUMMARY}'\n\
         inventory = 'none'\n\
         retired_by = 'D146'\n\
         historical_evidence = ['{COMMIT}', '{RECEIPT}']\n\
         tests = ['probe.rs::measured_probe']\n\
         limitations = ['{LIMITATION}']\n"
    )
}

fn snapshot_fixture() -> snapshot::Snapshot {
    snapshot::Snapshot {
        version: "fixture",
        defaults_platform: "fixture",
        top_level_help: String::new(),
        commands: vec![],
        cli_aliases: vec![],
        local_commands: vec![],
        options: vec![],
    }
}

#[test]
fn current_ledger_retains_d146_and_resolves_active_references() {
    let text = include_str!("../../../../tests/reference/tmux_compat/divergences.toml");
    let ledger = ledger::parse(text).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    ledger.validate_references(root).unwrap();
    let entry = ledger
        .entry
        .iter()
        .find(|entry| entry.id == "C-D36")
        .unwrap();
    let value = serde_json::to_value(entry).unwrap();
    assert_eq!(value["summary"], SUMMARY);
    assert_eq!(value["inventory"], "none");
    assert_eq!(value["retired_by"], "D146");
    assert_eq!(
        value["historical_evidence"],
        serde_json::json!([COMMIT, RECEIPT])
    );
    assert_eq!(value["tests"], serde_json::json!([PARITY_TEST]));
    assert_eq!(value["limitations"], serde_json::json!([LIMITATION]));
    // Retired provenance stays declared-only; the tracked receipt copy is the
    // current evidence and must name the same R2-197 oracle receipt.
    assert_eq!(value["evidence"], serde_json::json!(TRACKED_EVIDENCE));
    assert!(TRACKED_EVIDENCE[1].ends_with(&format!("/{RECEIPT}")));
    assert!(entry
        .evidence
        .iter()
        .all(|reference| !entry.historical_evidence.contains(reference)));
    let reflow = ledger
        .entry
        .iter()
        .find(|entry| entry.id == "C-D70")
        .unwrap();
    assert_eq!(reflow.inventory, "bounded");
    assert_eq!(reflow.tests, REFLOW_TESTS);
}

#[test]
fn retired_evidence_is_declared_history_but_tests_still_resolve() {
    let root = fixture();
    let text = retired_fixture();
    let ledger = ledger::parse(&text).unwrap();
    assert!(!root.join(RECEIPT).exists());
    assert!(ledger.validate_references(&root).is_err());
    fs::write(root.join("probe.rs"), "fn wrong_probe() {}\n").unwrap();
    assert!(ledger.validate_references(&root).is_err());
    fs::write(root.join("probe.rs"), "fn measured_probe() {}\n").unwrap();
    ledger.validate_references(&root).unwrap();
    // A retired entry's ordinary evidence remains a current source reference.
    let current = ledger::parse(&(text + "evidence = ['missing.rs']\n")).unwrap();
    assert!(current.validate_references(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn retirement_provenance_survives_render_publish_and_read_only_check() {
    let ledger = ledger::parse(&retired_fixture()).unwrap();
    let snapshot = snapshot_fixture();
    let artifacts = render::artifacts(&snapshot, &ledger).unwrap();
    let report: serde_json::Value = serde_json::from_str(&artifacts[4].content).unwrap();
    let entry = &report["divergences"]["entry"][0];
    assert_eq!(entry["retired_by"], "D146");
    assert_eq!(
        entry["historical_evidence"],
        serde_json::json!([COMMIT, RECEIPT])
    );
    assert_eq!(
        entry["tests"],
        serde_json::json!(["probe.rs::measured_probe"])
    );
    for value in [SUMMARY, LIMITATION, COMMIT, RECEIPT, "D146"] {
        assert!(artifacts[3].content.contains(value), "{value}");
    }
    assert!(artifacts[3]
        .content
        .contains("Historical evidence (declared only)"));
    let output = fixture();
    publish(&output, false, &artifacts).unwrap();
    publish(&output, true, &artifacts).unwrap();
    // Losing the retirement's recorded provenance must invalidate the checker.
    for index in [3, 4] {
        let path = output.join(artifacts[index].name);
        let changed = artifacts[index]
            .content
            .replace(COMMIT, "omitted provenance");
        fs::write(&path, &changed).unwrap();
        assert!(matches!(
            publish(&output, true, &artifacts),
            Err(Error::Stale(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), changed);
        fs::write(path, &artifacts[index].content).unwrap();
    }
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn active_policy_and_retirement_metadata_remain_strict() {
    let retired = retired_fixture();
    for mutation in [
        retired.replace("retired_by = 'D146'\n", ""),
        retired.replace("'D146'", "''"),
        retired.replace("'D146'", "'D0146'"),
        retired.replace("'D146'", "'D0'"),
        retired.replace("'D146'", "'unknown'"),
        retired.replace("inventory = 'none'", "inventory = 'bounded'"),
        retired.replace("inventory = 'none'", "inventory = 'deferred'"),
        retired.replace("inventory = 'none'", "inventory = 'unknown'"),
        retired.replace("tests = ['probe.rs::measured_probe']", "tests = []"),
        retired.replace(&format!("['{COMMIT}', '{RECEIPT}']"), "[]"),
        format!("{retired}unknown = 'field'\n"),
    ] {
        assert!(ledger::parse(&mutation).is_err(), "{mutation}");
    }
    let active = "schema = 1\n[policy]\noracle = 'tmux 3.7b'\nunlisted_divergences_are_bugs = true\n[[entry]]\nid = 'C-D1'\nsummary = 'active'\ninventory = 'bounded'\nevidence = ['probe.rs']\n";
    let root = fixture();
    fs::write(root.join("probe.rs"), "fn measured_probe() {}\n").unwrap();
    for inventory in ["none", "bounded", "deferred"] {
        ledger::parse(&active.replace("'bounded'", &format!("'{inventory}'")))
            .unwrap()
            .validate_references(&root)
            .unwrap();
    }
    for inventory in ["bounded", "deferred"] {
        ledger::parse(
            &(active.replace("'bounded'", &format!("'{inventory}'"))
                + "tests = ['probe.rs::measured_probe']\n"),
        )
        .unwrap()
        .validate_references(&root)
        .unwrap();
    }
    assert!(ledger::parse(
        &(active.replace("'bounded'", "'none'") + "tests = ['probe.rs::measured_probe']\n")
    )
    .is_err());
    for reference in [COMMIT, RECEIPT, "unknown:reference"] {
        let text = active.replace("probe.rs", reference);
        assert!(ledger::parse(&text)
            .unwrap()
            .validate_references(&root)
            .is_err());
        let text = format!("{active}historical_evidence = ['{reference}']\n");
        assert!(ledger::parse(&text).is_err());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_historical_and_current_paths_are_rejected_even_when_files_exist() {
    let root = fixture();
    fs::write(root.join("probe.rs"), "fn measured_probe() {}\n").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    let text = retired_fixture();
    for reference in [
        "",
        "../probe.rs",
        "nested/../probe.rs",
        "./probe.rs",
        "nested/./probe.rs",
        "nested//probe.rs",
        "/probe.rs",
        "C:/probe.rs",
        "C:\\probe.rs",
        "\\\\server\\probe.rs",
        "unknown:reference",
        "https://example.invalid/proof",
        "commit:",
        "commit:1234",
        "commit:7ad3957f2f7839dc38138210576a91a7ac47ad6z",
        "commit:7ad3957f2f7839dc38138210576a91a7ac47ad63::symbol",
        "receipt.json::symbol",
        "receipt.json/",
    ] {
        let changed = text.replace(RECEIPT, reference);
        assert!(
            ledger::parse(&changed)
                .and_then(|ledger| ledger.validate_references(&root))
                .is_err(),
            "historical: {reference}"
        );
    }
    for reference in [
        "",
        "../probe.rs",
        "nested/../probe.rs",
        "./probe.rs",
        "nested/./probe.rs",
        "nested//probe.rs",
        "/probe.rs",
        "C:/probe.rs",
        "C:\\probe.rs",
        "probe.rs::",
        "probe.rs::missing_probe",
        "probe.rs::measured_probe::extra",
        COMMIT,
    ] {
        let changed = text.replace("probe.rs::measured_probe", reference);
        assert!(
            ledger::parse(&changed)
                .and_then(|ledger| ledger.validate_references(&root))
                .is_err(),
            "current: {reference}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn current_references_follow_only_symlinks_within_the_source_root() {
    let root = fixture();
    let outside = fixture();
    fs::write(outside.join("probe.rs"), "fn measured_probe() {}\n").unwrap();
    std::os::unix::fs::symlink(outside.join("probe.rs"), root.join("probe.rs")).unwrap();
    let ledger = ledger::parse(&retired_fixture()).unwrap();
    assert!(ledger.validate_references(&root).is_err());
    fs::remove_file(root.join("probe.rs")).unwrap();
    fs::write(root.join("inside.rs"), "fn measured_probe() {}\n").unwrap();
    std::os::unix::fs::symlink(root.join("inside.rs"), root.join("probe.rs")).unwrap();
    ledger.validate_references(&root).unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}
