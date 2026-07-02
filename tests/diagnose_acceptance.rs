#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;

use common::{assert_success, stdout, CliHarness};
use serde_json::Value;

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
