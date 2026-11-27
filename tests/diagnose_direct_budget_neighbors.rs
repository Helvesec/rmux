#![cfg(unix)]

mod common;
#[path = "support/source_budget.rs"]
mod source_budget;

use std::error::Error;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use common::{assert_success, stdout, CliHarness};
use serde_json::Value;
use source_budget::source_match_budget_exceeded;

#[test]
fn budget_diagnostic_ignores_client_ids_and_timestamps() {
    for messages in [
        "00:13: client-2256221 command: show-messages\n\
         00:13: config error: Permission denied: /configs/middle.conf\n",
        "00:13: client-256 command: show-messages\n",
        "2026-10-16T06:25:56.256Z: client-42 command: show-messages\n",
        "source-file: Permission denied: /configs/256/middle.conf\n",
    ] {
        assert!(!source_match_budget_exceeded(messages), "{messages}");
    }
    assert!(source_match_budget_exceeded(
        "00:13: config error: source-file command exceeds 256 matched files or 33554432 aggregate bytes\n"
    ));
    assert!(source_match_budget_exceeded(
        "source-file command exceeds 256 matched files or 33554432 aggregate bytes\n"
    ));
    assert!(source_match_budget_exceeded(
        "source-file glob '/configs/*.conf' matched too many files (maximum 256)\n"
    ));
}

// Expectations follow strict source admission, not diagnostic parse statuses:
// failed direct reads cost zero; every glob entry and successful direct read
// costs one, even when passive inspection cannot parse that file.
fn check_middle(kind: &str, glob: bool, debit: bool, status: &str) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("direct-budget-neighbor")?;
    let mut groups = Vec::new();
    for name in ["A", "B"] {
        let directory = harness.tmpdir().join(name);
        fs::create_dir_all(&directory)?;
        for index in 0..128 {
            fs::write(
                directory.join(format!("{index:03}.conf")),
                "set -ga @selected x\n",
            )?;
        }
        groups.push(directory.join("*.conf").to_str().unwrap().to_owned());
    }
    let middle = harness.tmpdir().join("middle.conf");
    match kind {
        "directory" => fs::create_dir(&middle)?,
        "directory-link" => {
            let target = harness.tmpdir().join("directory");
            fs::create_dir(&target)?;
            symlink(target, &middle)?;
        }
        "missing" => {}
        "dangling-link" => symlink(harness.tmpdir().join("absent"), &middle)?,
        "fifo" => assert!(std::process::Command::new("mkfifo")
            .arg(&middle)
            .status()?
            .success()),
        "unreadable" => {
            fs::write(&middle, "# unreadable\n")?;
            fs::set_permissions(&middle, fs::Permissions::from_mode(0o0))?;
            assert!(
                fs::read(&middle).is_err(),
                "fixture requires an unprivileged reader"
            );
        }
        "oversized" => fs::File::create(&middle)?.set_len(16 * 1024 * 1024 + 1)?,
        "inspection-limit" => fs::write(&middle, "#\n".repeat(512 * 1024 + 1))?,
        "decoded-over-limit" => fs::write(&middle, vec![0xff; 12 * 1024 * 1024])?,
        "decoded-at-limit" | "decoded-one-over" => {
            let mut bytes = vec![b'#'];
            bytes.extend(vec![0xff; (32 * 1024 * 1024 - 2) / 3]);
            bytes.push(b'\n');
            if kind == "decoded-one-over" {
                bytes.push(b'\n');
            }
            fs::write(&middle, bytes)?;
        }
        "invalid-utf8" => fs::write(&middle, b"#\xff\n")?,
        "parse-error" => fs::write(&middle, "not-a-command\n")?,
        "empty" => fs::write(&middle, "")?,
        "null" => {}
        _ => panic!("unknown fixture"),
    }
    let middle_arg = if glob {
        harness
            .tmpdir()
            .join("middle.*")
            .to_str()
            .unwrap()
            .to_owned()
    } else if kind == "null" {
        "/dev/null".to_owned()
    } else {
        middle.to_str().unwrap().to_owned()
    };
    let tail = harness.tmpdir().join("tail.conf");
    fs::write(&tail, "set -g @tail yes\n")?;
    let mut paths = vec![groups[0].as_str(), middle_arg.as_str(), groups[1].as_str()];
    // The selection must stop once B exceeds the shared bound.
    if debit {
        paths.push(tail.to_str().unwrap());
    }
    let mut args = Vec::new();
    for path in &paths {
        args.extend(["-f", *path]);
    }
    let mut diagnose = args.clone();
    diagnose.extend(["diagnose", "--json"]);
    let output = harness.run(&diagnose)?;
    assert!(output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout)?;
    let files = json["compatibility"]["configurations"].as_array().unwrap();
    assert_eq!(files.len(), if debit { 130 } else { 257 });
    assert_eq!(files[128]["status"], status);
    if debit {
        assert_eq!(files[129]["status"], "too-many-matches");
    } else {
        for file in &files[129..] {
            assert_eq!(file["status"], "static-only");
            assert_eq!(file["commands"], serde_json::json!(["set-option"]));
        }
    }
    assert!(
        !harness.socket_path().exists(),
        "inspection must remain passive"
    );
    args.extend(["new-session", "-d", "-s", "neighbors"]);
    assert_success(&harness.run(&args)?);
    let selected = "x".repeat(if debit { 128 } else { 256 });
    assert_eq!(
        stdout(&harness.run(&["show-options", "-gqv", "@selected"])?).trim(),
        selected
    );
    let messages = stdout(&harness.run(&["show-messages"])?);
    assert_eq!(source_match_budget_exceeded(&messages), debit, "{messages}");
    assert!(stdout(&harness.run(&["show-options", "-gqv", "@tail"])?)
        .trim()
        .is_empty());

    assert_success(&harness.run(&["set-option", "-gu", "@selected"])?);
    let mut source = vec!["source-file"];
    source.extend(paths);
    let output = harness.run(&source)?;
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(source_match_budget_exceeded(&errors), debit, "{errors}");
    assert_eq!(
        stdout(&harness.run(&["show-options", "-gqv", "@selected"])?).trim(),
        selected
    );
    assert!(stdout(&harness.run(&["show-options", "-gqv", "@tail"])?)
        .trim()
        .is_empty());
    if kind == "unreadable" {
        fs::set_permissions(&middle, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[test]
fn direct_read_rejections_preserve_the_budget() -> Result<(), Box<dyn Error>> {
    for (kind, status) in [
        ("directory-link", "not-regular"),
        ("missing", "missing"),
        ("dangling-link", "missing"),
        ("fifo", "not-regular"),
        ("unreadable", "unreadable"),
        ("oversized", "too-large"),
    ] {
        check_middle(kind, false, false, status)?;
    }
    Ok(())
}

#[test]
fn rejected_glob_entries_still_consume_the_budget() -> Result<(), Box<dyn Error>> {
    for (kind, status) in [
        ("directory", "not-regular"),
        ("fifo", "not-regular"),
        ("unreadable", "unreadable"),
        ("oversized", "too-large"),
    ] {
        check_middle(kind, true, true, status)?;
    }
    Ok(())
}

#[test]
fn successful_strict_reads_count_despite_diagnostic_rejection() -> Result<(), Box<dyn Error>> {
    for (kind, status) in [
        ("inspection-limit", "too-large"),
        ("invalid-utf8", "invalid-utf8"),
        ("parse-error", "parse-error"),
        ("empty", "static-only"),
        ("null", "not-regular"),
    ] {
        check_middle(kind, false, true, status)?;
    }
    Ok(())
}

// L07.3: selection uses the decoded 32-MiB source admission boundary,
// independently of diagnose's tighter passive inspection size limit.
#[test]
fn decoded_direct_rejections_preserve_the_budget() -> Result<(), Box<dyn Error>> {
    check_middle("decoded-over-limit", false, false, "too-large")?;
    check_middle("decoded-one-over", false, false, "too-large")?;
    check_middle("decoded-at-limit", false, true, "too-large")?;
    Ok(())
}
