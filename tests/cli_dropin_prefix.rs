use std::ffi::OsString;
use std::process::{Command, Output, Stdio};

fn run(args: &[OsString], tmux: bool) -> Output {
    let binary = std::env::var_os("RMUX_TEST_DROPIN_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_rmux").into());
    let binary = rmux_os::build_identity::resolve(&binary).expect("candidate executable identity");
    let mut command = Command::new(binary);
    command.args(args).stdin(Stdio::null());
    for name in ["TMUX", "TMUX_PANE", "RMUX", "RMUX_INTERNAL_INVOKED_AS_TMUX"] {
        command.env_remove(name);
    }
    if tmux {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.arg0("tmux");
        }
        #[cfg(not(unix))]
        command.env("RMUX_INTERNAL_INVOKED_AS_TMUX", "1");
    }
    command.output().expect("run CLI")
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn same(actual: Output, expected: &Output, args: &[OsString]) {
    assert_eq!(actual.status.code(), expected.status.code(), "{args:?}");
    assert_eq!(actual.stdout, expected.stdout, "{args:?}");
    assert_eq!(actual.stderr, expected.stderr, "{args:?}");
}

#[test]
fn doctor_and_setup_help_project_global_prefixes_live() {
    for tmux in [false, true] {
        for tail in [
            vec!["doctor", "tmux-dropin"],
            vec!["doctor", "--help"],
            vec!["setup", "--help"],
            vec!["setup", "unknown"],
            vec!["setup", "tmux-shim", "extra"],
        ] {
            let expected = run(&os(&tail), tmux);
            assert_eq!(
                expected.status.code(),
                Some(if tail.contains(&"unknown") || tail.contains(&"extra") {
                    1
                } else {
                    0
                })
            );
            for flag in ['L', 'S', 'c', 'f', 'T'] {
                for value in ["unused-prefix", "--", "-V", "--help", ""] {
                    let mut prefixes = vec![
                        os(&["-N", "-u", &format!("-{flag}"), value]),
                        os(&["-N", &format!("-u{flag}"), value]),
                    ];
                    if !value.is_empty() {
                        prefixes.push(os(&["-N", &format!("-u{flag}{value}")]));
                    }
                    for mut prefix in prefixes {
                        prefix.extend(os(&tail));
                        same(run(&prefix, tmux), &expected, &prefix);
                    }
                }
            }
            let mut separated = os(&["-qNuLname", "--"]);
            separated.extend(os(&tail));
            same(run(&separated, tmux), &expected, &separated);
        }
        for decision in ["-V", "-h", "--version", "--help", "-x", "-uVx", "-uxV"] {
            let expected = run(&os(&[decision]), tmux);
            for command in ["doctor", "setup"] {
                let actual = os(&[decision, "-uLname", command, "--help"]);
                same(run(&actual, tmux), &expected, &actual);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn doctor_and_setup_help_accept_opaque_native_prefix_values_live() {
    use std::os::unix::ffi::OsStringExt;
    for tmux in [false, true] {
        for tail in [vec!["doctor", "tmux-dropin"], vec!["setup", "--help"]] {
            let expected = run(&os(&tail), tmux);
            for flag in [b'L', b'S', b'f', b'c', b'T'] {
                for prefix in [
                    vec![
                        "-N".into(),
                        OsString::from_vec(vec![b'-', b'u', flag, 0xff]),
                    ],
                    vec![
                        "-N".into(),
                        OsString::from_vec(vec![b'-', b'u', flag]),
                        OsString::from_vec(vec![0xff]),
                    ],
                ] {
                    let mut args = prefix;
                    args.extend(os(&tail));
                    same(run(&args, tmux), &expected, &args);
                }
            }
        }
    }
}
