#![cfg(unix)]

mod common;

use common::{FrozenTmuxBinary, FROZEN_TMUX_ENV, TMUX_ORACLE_ENV};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn source_file_parse_only_clear_history_matches_tmux_oracle() -> Result<(), Box<dyn Error>> {
    let tmux = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: tmux 3.7b oracle unavailable via {TMUX_ORACLE_ENV}/{FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let harness = SourceFileOracleHarness::new("source-file-clear-history")?;
    let config = harness.tmpdir().join("clear-history.tmux.conf");
    fs::write(&config, "clear-history\nset -g @after yes\n")?;

    harness.rmux_success(["-f", "/dev/null", "new-session", "-d", "-s", "s"])?;
    harness.tmux_success(&tmux, ["-f", "/dev/null", "new-session", "-d", "-s", "s"])?;

    let tmux_output = harness.tmux_output(&tmux, ["source-file", "-n", "-v"], &config)?;
    let rmux_output = harness.rmux_output(["source-file", "-n", "-v"], &config)?;

    assert_eq!(rmux_output.status.code(), tmux_output.status.code());
    assert_eq!(
        String::from_utf8_lossy(&rmux_output.stdout),
        String::from_utf8_lossy(&tmux_output.stdout),
        "rmux source-file -n -v stdout diverged from tmux"
    );
    assert_eq!(
        String::from_utf8_lossy(&rmux_output.stderr),
        String::from_utf8_lossy(&tmux_output.stderr),
        "rmux source-file -n -v stderr diverged from tmux"
    );

    Ok(())
}

#[test]
fn source_file_parse_only_does_not_execute_shell_or_apply_options() -> Result<(), Box<dyn Error>> {
    let harness = SourceFileOracleHarness::new("source-file-parse-only-effects")?;
    let marker = harness.tmpdir().join("parse-only-run-shell-marker");
    let config = harness.tmpdir().join("parse-only.tmux.conf");
    fs::write(
        &config,
        format!(
            "set -g @parse-only-applied no\nrun-shell 'touch {}'\n",
            shell_quote(&marker)
        ),
    )?;

    harness.rmux_success(["-f", "/dev/null", "new-session", "-d", "-s", "s"])?;
    harness.rmux_success_with_path(["source-file", "-n", "-v"], &config)?;

    assert!(
        !marker.exists(),
        "source-file -n executed run-shell and created {}",
        marker.display()
    );
    let option = harness.rmux_output(["show-options", "-gqv"], "@parse-only-applied")?;
    assert!(
        String::from_utf8_lossy(&option.stdout).trim().is_empty(),
        "source-file -n applied set-option side effects; stdout={}, stderr={}",
        String::from_utf8_lossy(&option.stdout),
        String::from_utf8_lossy(&option.stderr)
    );

    Ok(())
}

struct SourceFileOracleHarness {
    rmux_label: String,
    tmux_socket: PathBuf,
    tmpdir: PathBuf,
}

impl SourceFileOracleHarness {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = unique_id(label);
        let tmpdir = PathBuf::from("/tmp").join(&unique);
        let _ = fs::remove_dir_all(&tmpdir);
        fs::create_dir_all(&tmpdir)?;
        let harness = Self {
            rmux_label: unique,
            tmux_socket: tmpdir.join("tmux.sock"),
            tmpdir,
        };
        let _ = harness.rmux_output(["kill-server"], "");
        Ok(harness)
    }

    fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    fn rmux_success<I, S>(&self, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.rmux_output(args, "")?;
        assert_success("rmux", &output)
    }

    fn rmux_success_with_path<I, S>(&self, args: I, path: &Path) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.rmux_output(args, path)?;
        assert_success("rmux", &output)
    }

    fn tmux_success<I, S>(&self, tmux: &Path, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.tmux_output(tmux, args, Path::new(""))?;
        assert_success("tmux", &output)
    }

    fn rmux_output<I, S, P>(&self, args: I, final_arg: P) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
        P: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(rmux_binary());
        command.arg("-L").arg(&self.rmux_label).args(args);
        if !final_arg.as_ref().is_empty() {
            command.arg(final_arg);
        }
        Ok(command.output()?)
    }

    fn tmux_output<I, S, P>(
        &self,
        tmux: &Path,
        args: I,
        final_arg: P,
    ) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
        P: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(tmux);
        command.arg("-S").arg(&self.tmux_socket).args(args);
        if !final_arg.as_ref().is_empty() {
            command.arg(final_arg);
        }
        Ok(command.output()?)
    }
}

impl Drop for SourceFileOracleHarness {
    fn drop(&mut self) {
        let _ = self.rmux_output(["kill-server"], "");
        if let FrozenTmuxBinary::Available(tmux) = FrozenTmuxBinary::discover_optional() {
            let _ = self.tmux_output(&tmux, ["kill-server"], "");
        }
        let _ = fs::remove_dir_all(&self.tmpdir);
    }
}

fn rmux_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_rmux"))
}

fn assert_success(program: &str, output: &Output) -> Result<(), Box<dyn Error>> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} command failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn shell_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    let short_label: String = label
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .take(16)
        .collect();
    format!(
        "rx-{}-{}-{}",
        short_label,
        std::process::id(),
        nanos % 1_000_000
    )
    .chars()
    .map(|ch| {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            ch
        } else {
            '-'
        }
    })
    .collect()
}
