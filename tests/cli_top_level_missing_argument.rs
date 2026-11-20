use std::process::{Command, Output, Stdio};

// Measured with tmux 3.7b: only branding differs for this diagnostic.
const USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";

fn run(args: &[&str], tmux: bool) -> Output {
    let mut command = Command::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )));
    command
        .args(["-N", "-L"])
        .arg(format!("rmux-cli-missing-argument-{}", std::process::id()))
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("RMUX")
        .env_remove("RMUX_INTERNAL_INVOKED_AS_TMUX")
        .stdin(Stdio::null());
    if tmux {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.arg0("tmux");
        }
        #[cfg(not(unix))]
        command.env("RMUX_INTERNAL_INVOKED_AS_TMUX", "1");
    }
    command.output().expect("run isolated CLI")
}

fn assert_same(actual: Output, expected: &Output, args: &[&str]) {
    assert_eq!(actual.status.code(), expected.status.code(), "{args:?}");
    assert_eq!(actual.stdout, expected.stdout, "{args:?}");
    assert_eq!(actual.stderr, expected.stderr, "{args:?}");
}

#[test]
fn top_level_missing_arguments_use_tmux_diagnostic() {
    for tmux in [false, true] {
        for flag in ['L', 'S', 'f', 'T', 'c'] {
            for cluster in ["", "v", "2u", "uvv"] {
                let option = format!("-{cluster}{flag}");
                for args in [vec![option.as_str()], vec![&option, "value", &option]] {
                    let output = run(&args, tmux);
                    assert_eq!(output.status.code(), Some(1), "{args:?}, tmux={tmux}");
                    assert!(output.stdout.is_empty(), "{args:?}, tmux={tmux}");
                    assert_eq!(
                        output.stderr,
                        format!("rmux: option requires an argument -- {flag}\n{USAGE}").as_bytes(),
                        "{args:?}, tmux={tmux}"
                    );
                }
            }
        }
    }
}

#[test]
fn top_level_missing_arguments_preserve_earlier_decisions() {
    for tmux in [false, true] {
        for earlier in [
            "-V",
            "-h",
            "--version",
            "--help",
            "-x",
            "--nope",
            "--version=x",
        ] {
            let expected = run(&[earlier], tmux);
            for flag in ['L', 'S', 'f', 'T', 'c'] {
                for option in [format!("-{flag}"), format!("-uv{flag}")] {
                    let args = [earlier, &option];
                    assert_same(run(&args, tmux), &expected, &args);
                }
            }
        }
    }
}

#[test]
fn top_level_present_values_are_not_missing_arguments() {
    for tmux in [false, true] {
        for decisive in ["-V", "-x"] {
            let expected = run(&[decisive], tmux);
            for flag in ['L', 'S', 'f', 'T', 'c'] {
                for option in [format!("-{flag}"), format!("-uv{flag}")] {
                    for value in ["", "value", "--help", "--version", "--", "-x", "-V"] {
                        let args = [option.as_str(), value, decisive];
                        assert_same(run(&args, tmux), &expected, &args);
                    }
                    // In getopt short options, '=' is part of the value.
                    for suffix in ["=", "=value", "--help", "--version", "Vh"] {
                        let attached = format!("{option}{suffix}");
                        let args = [attached.as_str(), decisive];
                        assert_same(run(&args, tmux), &expected, &args);
                    }
                }
            }
        }
    }
}
