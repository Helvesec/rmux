//! Every public command is exercised through its published schema and CLI consumer.
use std::collections::BTreeSet;
use std::process::Command;

use rmux_core::command_inventory::{command_short_option_spec, RMUX_EXTENSION_COMMANDS};
use rmux_core::command_parser::COMMAND_TABLE;

#[path = "cli_schema_full_tiny_equivalence/scratch.rs"]
mod scratch;

#[test]
fn public_command_schema_matches_full_and_tiny_consumers() {
    let mut covered = BTreeSet::new();
    let mut missing = Vec::new();
    for entry in COMMAND_TABLE.iter().chain(RMUX_EXTENSION_COMMANDS) {
        assert!(
            covered.insert(entry.name),
            "duplicate command: {}",
            entry.name
        );
        let options = command_short_option_spec(entry.name).expect("published option signature");
        for name in std::iter::once(entry.name).chain(entry.alias) {
            // Delegation to Claude is tested at its local argument boundary;
            // requesting install-skill with an extra argument cannot launch it.
            let tail: &[&str] = if name == "claude" {
                &["install-skill", "extra"]
            } else {
                &["--help"]
            };
            let current = run_binary(env!("CARGO_BIN_EXE_rmux"), name, tail);
            if name == "claude" {
                assert_eq!(current.0, 1);
                assert_eq!(current.2, "usage: rmux claude install-skill\n");
            } else {
                assert_eq!(current.0, 0, "{name}: {current:?}");
                assert!(current.2.is_empty(), "{name}: {current:?}");
                assert!(!current.1.is_empty(), "{name}");
            }
            if matches!(entry.name, "new-pane" | "set-pane-placement") {
                for flag in ('A'..='Z').chain('a'..='z').chain('0'..='9') {
                    if (options.is_boolean(flag) || options.takes_value(flag))
                        && !current
                            .1
                            .lines()
                            .any(|line| line.trim_start().starts_with(&format!("-{flag}")))
                    {
                        missing.push(format!("{name} -{flag}"));
                    }
                }
            }
            if cfg!(feature = "tiny-cli") && !cfg!(debug_assertions) {
                let full = std::env::var("RMUX_FULL_BINARY_PATH")
                    .expect("native tiny proof needs its full helper");
                assert_eq!(current, run_binary(&full, name, tail), "full/tiny {name}");
            }
        }
    }
    assert_eq!(
        covered.len(),
        COMMAND_TABLE.len() + RMUX_EXTENSION_COMMANDS.len()
    );
    for name in ["diagnose", "choose-session", "choose-window"] {
        let current = run_binary(env!("CARGO_BIN_EXE_rmux"), name, &["--help"]);
        assert_eq!(current.0, 0, "{name}: {current:?}");
        assert!(current.2.is_empty(), "{name}: {current:?}");
        assert!(!current.1.is_empty(), "{name}");
        if cfg!(feature = "tiny-cli") && !cfg!(debug_assertions) {
            let full = std::env::var("RMUX_FULL_BINARY_PATH")
                .expect("native tiny proof needs its full helper");
            assert_eq!(
                current,
                run_binary(&full, name, &["--help"]),
                "full/tiny {name}"
            );
        }
    }
    assert!(
        missing.is_empty(),
        "published flags missing from CLI help: {missing:?}"
    );
    println!(
        "PASS: {} schema commands, their aliases, and 3 local projections",
        covered.len()
    );
}

fn run_binary(binary: &str, name: &str, tail: &[&str]) -> (i32, String, String) {
    let binary = rmux_os::build_identity::resolve(std::ffi::OsStr::new(binary))
        .expect("candidate executable identity");
    let root = scratch::create_root();
    let mut command = Command::new(binary);
    if name != "claude" {
        command.arg("-S").arg(root.join("s"));
    }
    let output = command
        .arg(name)
        .args(tail)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("RMUX")
        .env("RMUX_DISABLE_TMUX_FALLBACK", "1")
        .env("RMUX_DISABLE_TINY_CLI", "")
        .env("NO_COLOR", "1")
        .output()
        .expect("CLI schema consumer");
    assert!(!root.join("s").exists(), "help created a server: {name}");
    std::fs::remove_dir_all(root).unwrap();
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

// R1197-F1: run through the existing full/tiny consumer, with literal channels.
#[test]
fn diagnose_precedence_matches_full_and_tiny_consumers() {
    let conflict = (
        1,
        String::new(),
        "rmux diagnose: choose only one of --human or --json\n".to_owned(),
    );
    for formats in [["--human", "--json"], ["--json", "--human"]] {
        for (stop, expected) in [
            ("--help", (0, "usage: rmux diagnose [--human|--json]  --json: Emit JSON results on stdout; command errors use text on stderr with a nonzero exit and empty stdout. Supported unsuccessful results remain JSON\n".to_owned(), String::new())),
            ("--bad", (1, String::new(), "rmux diagnose: unknown argument '--bad'\n".to_owned())),
        ] {
            for (arguments, expected) in [
                ([formats[0], formats[1], stop], &conflict),
                ([stop, formats[0], formats[1]], &expected),
                ([formats[0], stop, formats[1]], &expected),
                ([formats[0], formats[0], stop], &expected),
            ] {
                let current = run_binary(env!("CARGO_BIN_EXE_rmux"), "diagnose", &arguments);
                assert_eq!(&current, expected, "{arguments:?}");
                if cfg!(feature = "tiny-cli") && !cfg!(debug_assertions) {
                    let full = std::env::var("RMUX_FULL_BINARY_PATH")
                        .expect("native tiny proof needs its full helper");
                    assert_eq!(current, run_binary(&full, "diagnose", &arguments),
                        "full/tiny diagnose {arguments:?}");
                }
            }
        }
    }
}
