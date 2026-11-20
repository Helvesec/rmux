#![cfg(unix)]

mod common;
#[path = "support/source_budget.rs"]
mod source_budget;

use std::error::Error;
use std::fs;

use common::{assert_success, stdout, CliHarness};
use serde_json::Value;
use source_budget::source_match_budget_exceeded;

// Frozen from the published startup bound: 256 matched files per selection,
// including repeated -f arguments. An overflowing group is rejected whole;
// earlier groups remain loaded, and later arguments are not inspected.
fn check_selection(groups: &[usize], repeated: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("diagnose-selection-limit")?;
    let mut patterns: Vec<String> = Vec::new();
    for (group, count) in groups.iter().enumerate() {
        if repeated && group > 0 {
            patterns.push(patterns[0].clone());
            continue;
        }
        let directory = harness.tmpdir().join(format!("group-{group}"));
        fs::create_dir_all(&directory)?;
        for index in 0..*count {
            fs::write(
                directory.join(format!("{index:03}.conf")),
                "set -ga @selected x\n",
            )?;
        }
        patterns.push(directory.join("*.conf").to_str().unwrap().to_owned());
    }
    let tail = harness.tmpdir().join("tail.conf");
    fs::write(&tail, "set -g @tail yes\n")?;
    let mut args = Vec::new();
    for pattern in &patterns {
        args.extend(["-f", pattern.as_str()]);
    }
    let mut accepted = 0;
    let mut overflow = None;
    for (index, count) in groups.iter().enumerate() {
        if accepted + count > 256 {
            overflow = Some(index);
            break;
        }
        accepted += count;
    }
    // A sentinel after the overflow proves that the selection stops there.
    if overflow.is_some() {
        args.extend(["-f", tail.to_str().unwrap()]);
    }
    let mut startup = args.clone();
    startup.extend(["new-session", "-d", "-s", "limit"]);
    assert_success(&harness.run(&startup)?);
    assert_eq!(
        stdout(&harness.run(&["show-options", "-gqv", "@selected"])?).trim(),
        "x".repeat(accepted),
        "startup must reject the entire group that exceeds the published bound"
    );
    let messages = stdout(&harness.run(&["show-messages"])?);
    assert_eq!(source_match_budget_exceeded(&messages), overflow.is_some());
    assert!(stdout(&harness.run(&["show-options", "-gqv", "@tail"])?)
        .trim()
        .is_empty());

    args.extend(["diagnose", "--json"]);
    let output = harness.run(&args)?;
    assert!(output.status.success(), "diagnose exited {}", output.status);
    let json: Value = serde_json::from_slice(&output.stdout)?;
    let files = json["compatibility"]["configurations"].as_array().unwrap();
    assert_eq!(files.len(), accepted + usize::from(overflow.is_some()));
    for file in &files[..accepted] {
        assert_eq!(file["status"], "static-only");
        assert_eq!(file["commands"], serde_json::json!(["set-option"]));
    }
    if let Some(index) = overflow {
        assert_eq!(files[accepted]["status"], "too-many-matches");
        assert_eq!(files[accepted]["commands"], serde_json::json!([]));
        assert!(files[accepted]["path"]
            .as_str()
            .unwrap()
            .ends_with(&format!(
                "group-{}/*.conf",
                if repeated { 0 } else { index }
            )));
    }
    // Every accepted group retains its sorted order and explicit repetitions.
    for (index, file) in files[..accepted].iter().enumerate() {
        let group = if repeated {
            0
        } else {
            usize::from(index >= groups[0])
        };
        let offset = if index >= groups[0] {
            index - groups[0]
        } else {
            index
        };
        assert!(file["path"]
            .as_str()
            .unwrap()
            .ends_with(&format!("group-{group}/{offset:03}.conf")));
    }
    Ok(())
}

#[test]
fn diagnose_selection_accepts_256_matches() -> Result<(), Box<dyn Error>> {
    check_selection(&[256], false)
}

#[test]
fn diagnose_selection_rejects_257_matches() -> Result<(), Box<dyn Error>> {
    check_selection(&[257], false)
}

#[test]
fn diagnose_selection_accepts_cumulative_256_matches() -> Result<(), Box<dyn Error>> {
    check_selection(&[128, 128], false)
}

#[test]
fn diagnose_selection_rejects_cumulative_257_matches() -> Result<(), Box<dyn Error>> {
    check_selection(&[128, 129], false)
}

#[test]
fn diagnose_selection_rejects_repeated_129_matches() -> Result<(), Box<dyn Error>> {
    check_selection(&[129, 129], true)
}

#[test]
fn explicit_source_file_stops_at_the_shared_match_limit() -> Result<(), Box<dyn Error>> {
    for count in [129, 257] {
        let harness = CliHarness::new("source-selection-limit")?;
        let directory = harness.tmpdir().join("configs");
        fs::create_dir_all(&directory)?;
        for index in 0..count {
            fs::write(
                directory.join(format!("{index:03}.conf")),
                "set -ga @selected x\n",
            )?;
        }
        assert_success(&harness.run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "source-limit",
        ])?);
        let pattern = directory.join("*.conf");
        let pattern = pattern.to_str().unwrap();
        let mut args = vec!["source-file", pattern];
        if count == 129 {
            args.push(pattern);
        }
        // An invalid later argument must never enter the error report.
        args.push("[");
        let output = harness.run(&args)?;
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(
            source_match_budget_exceeded(&errors),
            "missing selection limit: {errors}"
        );
        assert!(
            !errors.contains("invalid source-file glob"),
            "selection continued: {errors}"
        );
        assert_eq!(
            stdout(&harness.run(&["show-options", "-gqv", "@selected"])?).trim(),
            "x".repeat(if count == 129 { 129 } else { 0 })
        );
    }
    Ok(())
}

// L07.3 requires the same selection across all three entry paths. The
// published 256-match bound admits A128 + B128; a directly named directory
// fails strict reading and must not consume a match between those groups.
fn check_rejected_direct_path(entry: &str) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("direct-reject-budget")?;
    let mut paths = Vec::new();
    for group in ["A", "D", "B"] {
        let directory = harness.tmpdir().join(group);
        fs::create_dir_all(&directory)?;
        if group == "D" {
            paths.push(directory.to_str().unwrap().to_owned());
        } else {
            for index in 0..128 {
                fs::write(
                    directory.join(format!("{index:03}.conf")),
                    "set -ga @selected x\n",
                )?;
            }
            paths.push(directory.join("*.conf").to_str().unwrap().to_owned());
        }
    }
    if entry == "source-file" {
        assert_success(&harness.run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "direct-reject",
        ])?);
        let output = harness.run(&["source-file", &paths[0], &paths[1], &paths[2]])?;
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(
            errors.contains(&paths[1]),
            "missing directory error: {errors}"
        );
        assert!(
            !source_match_budget_exceeded(&errors),
            "directory consumed a match: {errors}"
        );
    } else {
        let mut args = Vec::new();
        for path in &paths {
            args.extend(["-f", path.as_str()]);
        }
        if entry == "diagnose" {
            args.extend(["diagnose", "--json"]);
            let output = harness.run(&args)?;
            assert!(output.status.success());
            let json: Value = serde_json::from_slice(&output.stdout)?;
            let files = json["compatibility"]["configurations"].as_array().unwrap();
            assert_eq!(
                files.len(),
                257,
                "A128 + rejected D + B128 must be inspected"
            );
            assert_eq!(files[128]["status"], "not-regular");
            for (index, file) in files.iter().enumerate() {
                if index == 128 {
                    continue;
                }
                assert_eq!(file["status"], "static-only");
                assert_eq!(file["commands"], serde_json::json!(["set-option"]));
                let (group, offset) = if index < 128 {
                    ("A", index)
                } else {
                    ("B", index - 129)
                };
                assert!(file["path"]
                    .as_str()
                    .unwrap()
                    .ends_with(&format!("{group}/{offset:03}.conf")));
            }
            return Ok(());
        }
        args.extend(["new-session", "-d", "-s", "direct-reject"]);
        assert_success(&harness.run(&args)?);
        let errors = stdout(&harness.run(&["show-messages"])?);
        assert!(
            errors.contains(&paths[1]),
            "missing directory error: {errors}"
        );
        assert!(
            !source_match_budget_exceeded(&errors),
            "directory consumed a match: {errors}"
        );
    }
    assert_eq!(
        stdout(&harness.run(&["show-options", "-gqv", "@selected"])?).trim(),
        "x".repeat(256)
    );
    Ok(())
}

#[test]
fn startup_rejected_direct_directory_preserves_match_budget() -> Result<(), Box<dyn Error>> {
    check_rejected_direct_path("startup")
}

#[test]
fn source_file_rejected_direct_directory_preserves_match_budget() -> Result<(), Box<dyn Error>> {
    check_rejected_direct_path("source-file")
}

#[test]
fn diagnose_rejected_direct_directory_preserves_match_budget() -> Result<(), Box<dyn Error>> {
    check_rejected_direct_path("diagnose")
}
