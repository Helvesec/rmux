#![cfg(unix)]

use std::error::Error;
use std::fs;
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

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()))
}
