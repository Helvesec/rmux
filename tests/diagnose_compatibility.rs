#![cfg(unix)]

mod common;

use common::{stdout, CliHarness};
use serde_json::Value;
use std::{error::Error, fs};

// L07.4: the published registry, passive import boundary, and versioned shared
// report are the contract. No tmux behavior or existing golden is redefined.
#[test]
fn diagnose_compatibility() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("diagnose-compatibility")?;
    let config = harness.tmpdir().join("import.conf");
    let include = harness.tmpdir().join("included.conf");
    let marker = harness.tmpdir().join("executed");
    fs::write(
        &include,
        format!("run-shell 'touch {}'\n", marker.display()),
    )?;
    fs::write(&config, format!(
        "PRIVATE=DO-NOT-DISCLOSE\r\nsetenv -g PRIVATE DO-NOT-DISCLOSE\rsource-file {}\nrun-shell 'touch {}'\nbind x {{ display -p DO-NOT-DISCLOSE }}\n",
        include.display(), marker.display()
    ))?;
    let args = ["-f", config.to_str().unwrap(), "diagnose", "--json"];
    let output = harness.run(&args)?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let json: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(json["schema_version"], 1);
    assert_eq!(
        json["compatibility"]["configurations"][0]["status"],
        "static-only"
    );
    assert_eq!(
        json["compatibility"]["configurations"][0]["commands"],
        serde_json::json!([
            "bind-key",
            "display-message",
            "run-shell",
            "set-environment",
            "source-file"
        ])
    );
    let human = harness.run(&["-f", config.to_str().unwrap(), "diagnose", "--human"])?;
    assert!(human.status.success());
    assert!(human.stderr.is_empty());
    let rendered = stdout(&human);
    let registry = include_str!("reference/tmux_compat/divergences.toml");
    let ids = json["divergences"]
        .as_array()
        .expect("diagnose must link actual registry IDs");
    assert!(!ids.is_empty());
    for id in ids {
        let id = id.as_str().expect("registry ID");
        assert!(registry.contains(&format!("id = \"{id}\"")));
        assert!(
            rendered.contains(&format!("#{id}")),
            "human/JSON registry links disagree"
        );
    }
    assert!(
        ids.contains(&Value::from("C-D56")),
        "source normalization has a registered divergence"
    );
    for report in [stdout(&output), rendered] {
        assert!(!report.contains("DO-NOT-DISCLOSE"));
        assert!(report.contains("diagnose never executes them"));
    }
    assert!(!marker.exists(), "import inspection executed a command");
    assert!(
        !harness.socket_path().exists(),
        "import inspection started a daemon"
    );
    Ok(())
}
