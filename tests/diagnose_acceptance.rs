#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;

use common::{assert_success, stdout, CliHarness};
use serde_json::Value;

#[test]
fn diagnose_inspects_each_startup_glob_match_and_preserves_repetitions(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("diagnose-config-glob")?;
    let directory = harness.tmpdir().join("configs");
    fs::create_dir_all(&directory)?;
    for (name, value) in [("a.conf", "A"), ("b.conf", "B")] {
        fs::write(
            directory.join(name),
            format!("set -ga @glob-loaded {value}\n"),
        )?;
    }
    let pattern = directory.join("*.conf");
    let pattern = pattern.to_str().ok_or("UTF-8 fixture path")?;
    assert_success(&harness.run(&[
        "-f",
        pattern,
        "-f",
        pattern,
        "new-session",
        "-d",
        "-s",
        "glob",
    ])?);
    assert_eq!(
        stdout(&harness.run(&["show-options", "-gqv", "@glob-loaded"])?).trim(),
        "ABAB"
    );
    let output = harness.run(&["-f", pattern, "-f", pattern, "diagnose", "--json"])?;
    assert!(output.status.success(), "diagnose exited {}", output.status);
    let json: Value = serde_json::from_slice(&output.stdout)?;
    let files = json["compatibility"]["configurations"]
        .as_array()
        .ok_or("configuration inventory")?;
    assert_eq!(
        files.len(),
        4,
        "diagnose must inspect the files that startup selected"
    );
    for (file, name) in files.iter().zip(["a.conf", "b.conf", "a.conf", "b.conf"]) {
        assert!(file["path"].as_str().unwrap().ends_with(name));
        assert_eq!(file["status"], "static-only");
        assert_eq!(file["commands"], serde_json::json!(["set-option"]));
    }
    Ok(())
}

#[test]
fn diagnose_glob_errors_are_bounded_passive_and_do_not_trigger_fallback(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("diagnose-glob-errors")?;
    let directory = harness.tmpdir().join("configs");
    fs::create_dir_all(&directory)?;
    for index in 0..257 {
        fs::write(
            directory.join(format!("{index:03}.conf")),
            "run-shell 'exit 91'\n",
        )?;
    }
    for (pattern, expected) in [
        ("*.conf", "too-many-matches"),
        ("[", "invalid-pattern"),
        ("absent-*.conf", "missing"),
    ] {
        let path = directory.join(pattern);
        let output = harness.run(&["-f", path.to_str().unwrap(), "diagnose", "--json"])?;
        assert!(output.status.success(), "diagnose exited {}", output.status);
        let json: Value = serde_json::from_slice(&output.stdout)?;
        let files = json["compatibility"]["configurations"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["status"], expected);
        assert_eq!(files[0]["commands"], serde_json::json!([]));
        assert!(!harness.socket_path().exists());
    }
    Ok(())
}

#[test]
fn diagnose_json_reports_source_located_startup_config_errors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("diagnose-startup-config-errors")?;
    let config_dir = harness.tmpdir().join("xdg").join("rmux");
    fs::create_dir_all(&config_dir)?;
    fs::write(
        config_dir.join("rmux.conf"),
        "set -g @diagnose-before yes\n\
         if-shell -F '1' {\n\
         set -g @diagnose-after no\n",
    )?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let diagnose = harness.run(&["diagnose", "--json"])?;
    assert!(
        diagnose.status.success(),
        "diagnose --json failed: {}",
        String::from_utf8_lossy(&diagnose.stderr)
    );
    let json: Value = serde_json::from_slice(&diagnose.stdout)?;
    let messages = json
        .pointer("/config/messages")
        .and_then(Value::as_array)
        .ok_or("diagnose JSON must expose config.messages as an array")?;
    let rendered_messages = serde_json::to_string(messages)?;
    assert!(
        rendered_messages.contains("rmux.conf")
            && (rendered_messages.contains("unmatched }")
                || rendered_messages.contains("missing }")),
        "diagnose must surface source-located startup config diagnostics, got: {rendered_messages}"
    );

    let show_messages = harness.run(&["show-messages"])?;
    assert!(
        show_messages.status.success(),
        "show-messages failed: {}",
        String::from_utf8_lossy(&show_messages.stderr)
    );
    assert!(
        stdout(&show_messages).contains("rmux.conf"),
        "show-messages should retain the underlying config diagnostic"
    );

    Ok(())
}

#[test]
fn diagnose_configuration_inventory_is_passive_bounded_and_private() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("diagnose-config-trust")?;
    let home = harness.tmpdir().join("home");
    fs::create_dir_all(&home)?;
    let config = home.join("inspection.conf");
    let marker = home.join("executed");
    fs::write(&config, format!(
        "PRIVATE=DO-NOT-DISCLOSE\nsetenv -g PRIVATE DO-NOT-DISCLOSE\nrun 'touch {}'\nbind x {{ display -p DO-NOT-DISCLOSE }}\nsource included.conf\n", marker.display()
    ))?;
    let invalid = home.join("invalid.conf");
    fs::write(&invalid, "DO-NOT-DISCLOSE\n")?;
    let oversized = home.join("oversized.conf");
    fs::File::create(&oversized)?.set_len(1024 * 1024 + 1)?;
    let fifo = home.join("input.fifo");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()?
        .success());
    let paths = [&config, &invalid, &oversized, &fifo];
    let mut args = vec![];
    for path in paths {
        args.extend(["-f", path.to_str().ok_or("UTF-8 fixture path")?]);
    }
    // Explicit repetitions remain separate observations, just like startup.
    args.extend(["-f", config.to_str().unwrap(), "diagnose", "--json"]);
    let json_output = harness.run(&args)?;
    assert!(json_output.status.success(), "diagnose JSON must exit zero");
    assert!(json_output.stderr.is_empty());
    let json: Value = serde_json::from_slice(&json_output.stdout)?;
    let expected: Value =
        serde_json::from_str(include_str!("reference/diagnose/compatibility.json"))?;
    assert_eq!(json["compatibility"], expected);
    *args.last_mut().unwrap() = "--human";
    let human_output = harness.run(&args)?;
    assert!(
        human_output.status.success(),
        "diagnose human must exit zero"
    );
    assert!(human_output.stderr.is_empty());
    let human = stdout(&human_output);
    assert!(human.contains(include_str!("reference/diagnose/compatibility.txt")));
    for output in [&json_output, &human_output] {
        assert!(!stdout(output).contains("DO-NOT-DISCLOSE"));
        assert!(!stdout(output).contains(home.to_str().unwrap()));
    }
    assert!(
        !marker.exists(),
        "configuration inspection must never execute commands"
    );
    assert!(
        !harness.socket_path().exists(),
        "diagnose must not start a daemon"
    );

    // A live daemon may already hold an error containing imported values.
    // The support report keeps its class/location but not the raw payload.
    assert_success(&harness.run(&[
        "-f",
        invalid.to_str().unwrap(),
        "new-session",
        "-d",
        "-s",
        "private-error",
    ])?);
    let messages = harness.run(&["show-messages"])?;
    assert!(stdout(&messages).contains("DO-NOT-DISCLOSE"));
    for format in ["--human", "--json"] {
        let output = harness.run(&["diagnose", format])?;
        assert!(output.status.success());
        let rendered = stdout(&output);
        assert!(rendered.contains("unknown command:"));
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("DO-NOT-DISCLOSE"));
    }
    Ok(())
}
