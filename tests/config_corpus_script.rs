#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn smoke_config_corpus_reports_parse_only_stdout_diagnostics() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("rmux-config-corpus-script");
    let corpus = root.join("corpus");
    fs::create_dir_all(&corpus)?;
    fs::write(
        corpus.join("bad.conf"),
        "set -g @before yes\nnot-a-command\n",
    )?;
    let results = root.join("results.tsv");

    let output = Command::new("bash")
        .arg("scripts/smoke-config-corpus.sh")
        .arg(&corpus)
        .arg("--rmux")
        .arg(env!("CARGO_BIN_EXE_rmux"))
        .arg("--keep-going")
        .arg("--results")
        .arg(&results)
        .output()?;

    assert!(
        !output.status.success(),
        "invalid corpus should fail; stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let tsv = fs::read_to_string(&results)?;
    assert!(
        tsv.contains("unknown command: not-a-command"),
        "parse-only stdout diagnostic should be recorded in TSV, got {tsv:?}"
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn vendored_gpakosz_corpus_passes_parse_only_and_startup_fallback() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("rmux-vendored-config-corpus");
    fs::create_dir_all(&root)?;
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/reference/config-corpus/gpakosz-oh-my-tmux");

    for mode in ["parse-only", "startup-fallback"] {
        let results = root.join(format!("{mode}.tsv"));
        let output = Command::new("bash")
            .arg("scripts/smoke-config-corpus.sh")
            .arg(&corpus)
            .arg("--rmux")
            .arg(env!("CARGO_BIN_EXE_rmux"))
            .arg("--mode")
            .arg(mode)
            .arg("--results")
            .arg(&results)
            .output()?;

        assert!(
            output.status.success(),
            "vendored config corpus failed in {mode}; stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout)?;
        assert!(
            stdout.contains("total=1") && stdout.contains("failed=0"),
            "vendored config corpus did not exercise exactly one clean config in {mode}: {stdout:?}"
        );
        let tsv = fs::read_to_string(&results)?;
        assert!(
            tsv.lines().any(|line| line.starts_with("ok\t")),
            "vendored config corpus TSV should contain an ok row in {mode}: {tsv:?}"
        );
    }

    fs::remove_dir_all(root)?;
    Ok(())
}

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()))
}
