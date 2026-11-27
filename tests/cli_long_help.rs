#![cfg(unix)]

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rmux_core::command_inventory::{render_list_commands_line, RMUX_EXTENSION_COMMANDS};
use rmux_core::command_parser::COMMAND_TABLE;

const SHORT_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";

// Preserve subprocess evidence, and avoid the daemon cleanup in the session harness.
struct HelpCli {
    root: PathBuf,
    sequence: Cell<usize>,
}

impl HelpCli {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::var_os("RMUX_TEST_TMPDIR")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join(format!(
                "h{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            sequence: Cell::new(0),
        }
    }

    fn run(&self, tmux: bool, args: &[&str]) -> (i32, String, String) {
        let sequence = self.sequence.get();
        self.sequence.set(sequence + 1);
        let out = self.root.join(format!("{sequence}.out"));
        let err = self.root.join(format!("{sequence}.err"));
        let mut child = Command::new(rmux_os::build_identity::cargo_binary(env!(
            "CARGO_BIN_EXE_rmux"
        )))
        .arg0(if tmux { "tmux" } else { "rmux" })
        .arg("-S")
        .arg(self.root.join("s"))
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("RMUX")
        .env("HOME", self.root.join("home"))
        .env("XDG_CONFIG_HOME", self.root.join("config"))
        .env("RMUX_DISABLE_TMUX_FALLBACK", "1")
        .env("RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE", "1")
        .env("RMUX_INTERNAL_BINARY_PATH", self.root.join("absent-daemon"))
        .env("RMUX_DAEMON_BINARY_PATH", self.root.join("absent-daemon"))
        .stdin(Stdio::null())
        .stdout(File::create(&out).unwrap())
        .stderr(File::create(&err).unwrap())
        .spawn()
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("help CLI timed out: {args:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(!self.root.join("s").exists(), "socket created: {args:?}");
        (
            status.code().unwrap(),
            fs::read_to_string(out).unwrap(),
            fs::read_to_string(err).unwrap(),
        )
    }
}

#[test]
fn diagnose_help_has_one_shared_usage_line() {
    let cli = HelpCli::new();
    let help = cli.run(false, &["--help"]);
    let diagnose = cli.run(false, &["diagnose", "--help"]);
    assert_eq!(help.0, 0);
    assert_eq!(diagnose.0, 0);
    assert!(help.2.is_empty());
    assert!(diagnose.2.is_empty());
    let (_, commands) = help.1.split_once("\nRMUX extensions:\n").unwrap();
    let (extensions, _) = commands.split_once("\nCommands:\n").unwrap();
    let usage = diagnose.1.strip_prefix("usage: rmux ").unwrap().trim_end();
    assert_eq!(
        extensions
            .lines()
            .filter(|line| line.trim() == usage)
            .count(),
        1
    );
}

#[test]
fn native_long_help_succeeds_without_daemon() {
    let (code, out, err) = HelpCli::new().run(false, &["--help"]);
    assert_eq!(code, 0, "native --help is a successful CLI request");
    assert!(err.is_empty(), "stderr={err:?}");
    assert!(!out.is_empty());
}

#[test]
fn long_help_has_exact_registry_membership_and_shared_usage() {
    let cli = HelpCli::new();
    let help = cli.run(false, &["--help"]);
    assert_eq!(help.0, 0);
    assert!(help.2.is_empty());
    let (preamble, commands) = help.1.split_once("\nCommands:\n").unwrap();
    let (usage, extensions) = preamble.split_once("\nRMUX extensions:\n").unwrap();
    assert_eq!(usage, SHORT_USAGE);
    assert!(COMMAND_TABLE.len() > 50);
    assert!(RMUX_EXTENSION_COMMANDS.len() > 10);
    let expected_commands = COMMAND_TABLE
        .iter()
        .map(|entry| entry.name)
        .collect::<BTreeSet<_>>();
    let expected_extensions = RMUX_EXTENSION_COMMANDS
        .iter()
        .map(|entry| entry.name)
        .chain(["diagnose"])
        .collect::<BTreeSet<_>>();
    for (section, expected) in [
        (commands, expected_commands),
        (extensions, expected_extensions),
    ] {
        let roots = section
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect::<Vec<_>>();
        assert_eq!(
            roots.len(),
            expected.len(),
            "duplicate or missing root lines"
        );
        assert_eq!(roots.into_iter().collect::<BTreeSet<_>>(), expected);
    }
    assert_eq!(commands, cli.run(false, &["list-commands"]).1);
    for entry in RMUX_EXTENSION_COMMANDS {
        let line = render_list_commands_line(None, entry.name, entry.alias);
        assert_eq!(
            extensions
                .lines()
                .filter(|candidate| candidate.trim() == line)
                .count(),
            1
        );
        assert_eq!(
            cli.run(false, &["list-commands", entry.name]),
            (0, format!("{line}\n"), String::new())
        );
    }
    let diagnose = cli.run(false, &["diagnose", "--help"]);
    assert_eq!(diagnose.0, 0);
    assert!(diagnose.2.is_empty());
    let diagnose_usage = diagnose.1.strip_prefix("usage: rmux ").unwrap().trim_end();
    assert_eq!(
        extensions
            .lines()
            .filter(|line| line.trim() == diagnose_usage)
            .count(),
        1
    );
    let absent = cli.run(false, &["list-commands", "diagnose"]);
    assert_eq!(absent.0, 1);
    assert_eq!(absent.2, "unknown command: diagnose\n");
    assert_eq!(cli.run(false, &["--help"]), help, "deterministic output");
}

#[test]
fn long_help_preserves_personality_ordering_and_value_boundaries() {
    let cli = HelpCli::new();
    let native = cli.run(false, &["--help"]);
    let tmux_help = cli.run(true, &["--help"]);
    assert_eq!(native.0, 0);
    assert_eq!(tmux_help, (1, String::new(), native.1.clone()));
    for tmux in [false, true] {
        let help = cli.run(tmux, &["--help"]);
        let short = cli.run(tmux, &["-h"]);
        let version = cli.run(tmux, &["-V"]);
        assert_eq!(short, (0, SHORT_USAGE.to_owned(), String::new()));
        let version_text = if tmux {
            "tmux 3.4".to_owned()
        } else {
            format!("rmux {}", env!("CARGO_PKG_VERSION"))
        };
        assert_eq!(version, (0, format!("{version_text}\n"), String::new()));
        let long_version = cli.run(tmux, &["--version"]);
        if tmux {
            assert_eq!(long_version.0, 1);
            assert!(long_version.1.is_empty());
        } else {
            assert_eq!(long_version, version);
        }
        for earlier in ["-h", "-V", "--version", "--nope", "-x", "--help=x", "--hel"] {
            let expected = cli.run(tmux, &[earlier]);
            assert_eq!(cli.run(tmux, &[earlier, "--help"]), expected, "{earlier}");
            assert_eq!(cli.run(tmux, &["--help", earlier]), help, "{earlier}");
        }
        for unknown in [
            "--nope",
            "--help=x",
            "--hel",
            "---help",
            "--vers",
            "--version=x",
        ] {
            assert_eq!(
                cli.run(tmux, &[unknown]),
                (1, String::new(), SHORT_USAGE.replace("2CDh", "2CD"))
            );
        }
        for flag in ['L', 'S', 'f', 'T', 'c'] {
            for prefix in [format!("-{flag}"), format!("-v{flag}")] {
                assert_eq!(cli.run(tmux, &[&prefix, "value", "--help"]), help);
                assert_eq!(cli.run(tmux, &[&prefix, "--help", "-V"]), version);
                assert_eq!(cli.run(tmux, &[&format!("{prefix}--help"), "-V"]), version);
                assert_eq!(
                    cli.run(tmux, &[&prefix, "--help", "-x"]),
                    cli.run(tmux, &["-x"])
                );
            }
        }
        for (cluster, expected) in [("-hV", &short), ("-Vh", &version)] {
            assert_eq!(&cli.run(tmux, &[cluster, "--help"]), expected);
        }
        for boundary in ["--", "-", "", "no-such-gh195-command"] {
            let result = cli.run(tmux, &[boundary, "--help"]);
            assert_eq!(result.0, 1);
            assert!(result.1.is_empty());
            assert!(!result.2.contains("RMUX extensions:"));
        }
        let command = cli.run(tmux, &["list-commands", "--help"]);
        assert_eq!(command.0, 0);
        assert!(!command.1.contains("RMUX extensions:"));
    }
}
