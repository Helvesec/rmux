use std::process::{Command, Output, Stdio};

fn run(args: &[&str], tmux: bool) -> Output {
    let mut command = Command::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )));
    command
        .args(["-N", "-L"])
        .arg(format!("r3q-test-{}", std::process::id()))
        .args(args)
        .env_remove("TMUX")
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

fn assert_same(control: &[&str], subject: &[&str], tmux: bool) {
    let expected = run(control, tmux);
    let actual = run(subject, tmux);
    assert_eq!(actual.status.code(), expected.status.code(), "{subject:?}");
    assert_eq!(actual.stdout, expected.stdout, "{subject:?}");
    assert_eq!(
        comparable_stderr(&actual.stderr),
        comparable_stderr(&expected.stderr),
        "{subject:?}"
    );
}

fn comparable_stderr(stderr: &[u8]) -> (&[u8], &[u8]) {
    if cfg!(windows) {
        if let Some(parts) = windows_missing_server_parts(stderr) {
            return parts;
        }
    }
    (stderr, &[])
}

// endpoint_for_label rotates the instance nonce after a short-lived resolver exits.
// Accept only the observed native missing-server diagnostic; compare every byte
// before and after the nonce, including the SID, integrity level and hashed key.
fn windows_missing_server_parts(stderr: &[u8]) -> Option<(&[u8], &[u8])> {
    const SUFFIX: &str = " (No such file or directory)\n";
    let text = std::str::from_utf8(stderr).ok()?;
    let endpoint = text
        .strip_prefix(r"error connecting to \\.\pipe\rmux-")?
        .strip_suffix(SUFFIX)?;
    let (sid, endpoint) = endpoint.split_once("-il-")?;
    let sid = sid.strip_prefix("S-1-")?;
    if !sid.contains('-')
        || sid
            .split('-')
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let (integrity, components) = endpoint.split_once("-g-")?;
    if !matches!(
        integrity,
        "untrusted" | "low" | "medium" | "high" | "system"
    ) {
        return None;
    }
    let (key, nonce) = components.split_once('-')?;
    if key.len() != 64
        || nonce.len() != 32
        || !key
            .bytes()
            .chain(nonce.bytes())
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let nonce_end = stderr.len() - SUFFIX.len();
    Some((&stderr[..nonce_end - nonce.len()], &stderr[nonce_end..]))
}

// tmux 3.7b consumes historical top-level q; it does not enable quiet output.
#[test]
fn legacy_q_preserves_commands_and_extension_ownership() {
    for tmux in [false, true] {
        for command in [
            vec!["list-sessions"],
            vec!["list-commands"],
            vec!["list-keys"],
            vec!["show-options", "-q", "@absent"],
            vec!["set-option", "-q", "@test", "on"],
            vec!["source-file", "-q", "absent"],
            vec!["list-sessions", "-q"],
            vec!["capabilities", "--help"],
            vec!["diagnose", "--help"],
            vec!["doctor", "--help"],
            vec!["setup", "--help"],
            vec!["claude", "--help"],
            vec!["--", "-q", "-V"],
            vec!["", "-q"],
            vec!["-", "-q"],
        ] {
            for prefix in [vec!["-q"], vec!["-qq", "-q"], vec!["-qNq", "-q"]] {
                let mut args = prefix;
                args.extend(&command);
                assert_same(&command, &args, tmux);
            }
        }
    }
    let absent = run(&["-q", "list-sessions"], false);
    assert_eq!(absent.status.code(), Some(1));
    assert!(absent.stdout.is_empty());
    let error = String::from_utf8_lossy(&absent.stderr);
    #[cfg(not(windows))]
    assert!(
        error.contains("r3q-test-"),
        "runtime endpoint diagnostic: {error}"
    );
    // Windows represents the label by a hashed key, already compared above.
    #[cfg(windows)]
    assert!(
        windows_missing_server_parts(&absent.stderr).is_some(),
        "runtime endpoint diagnostic: {error}"
    );
    assert!(!error.contains("unknown option"));
}

#[test]
fn legacy_q_preserves_first_decision_and_retained_d_u_errors() {
    for tmux in [false, true] {
        for earlier in [
            "-V",
            "-h",
            "--version",
            "--help",
            "-x",
            "--nope",
            "-d",
            "-U",
        ] {
            for later in ["-q", "-V", "-h", "-L", "--help", "--version"] {
                assert_same(&[earlier, later], &["-q", earlier, later], tmux);
                assert_same(&[earlier, later], &[earlier, "-q", later], tmux);
            }
        }
        for (control, subject) in [
            ("-Vh", "-qVh"),
            ("-hV", "-qhV"),
            ("-xV", "-qxV"),
            ("-dV", "-qdV"),
            ("-UV", "-qUV"),
        ] {
            assert_same(&[control], &[subject], tmux);
        }
        for flag in ["d", "U"] {
            let output = run(&["-q", &format!("-{flag}")], tmux);
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr)
                .starts_with(&format!("rmux: unknown option -- {flag}\nusage:")));
        }
    }
}

#[test]
fn legacy_q_keeps_missing_values_and_value_like_q_at_runtime() {
    for tmux in [false, true] {
        for flag in ["L", "S", "f", "T", "c"] {
            for cluster in ["", "u"] {
                let option = format!("-{cluster}{flag}");
                let with_q = format!("-q{cluster}q{flag}");
                assert_same(&[&option], &["-q", &with_q], tmux);
                for value in ["", "q", "-q", "-qq", "--", "--help", "--version", "-V"] {
                    for tail in ["list-sessions", "-x", "-V"] {
                        assert_same(
                            &[&option, value, tail],
                            &["-q", &with_q, value, "-q", tail],
                            tmux,
                        );
                    }
                }
            }
        }
    }
}

mod stderr_fixture_tests {
    use super::{comparable_stderr, windows_missing_server_parts};

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const FIRST_NONCE: &str = "0123456789abcdef0123456789abcdef";
    const NEXT_NONCE: &str = "fedcba9876543210fedcba9876543210";

    fn diagnostic(nonce: &str) -> String {
        format!(
            "error connecting to \\\\.\\pipe\\rmux-S-1-5-21-100-200-300-1000-il-medium-g-\
             {KEY}-{nonce} (No such file or directory)\n"
        )
    }

    #[test]
    fn comparison_ignores_only_the_windows_instance_nonce() {
        let first = diagnostic(FIRST_NONCE);
        let next = diagnostic(NEXT_NONCE);
        assert_ne!(first, next);
        let prefix = format!(
            "error connecting to \\\\.\\pipe\\rmux-S-1-5-21-100-200-300-1000-il-medium-g-{KEY}-"
        );
        let expected = Some((prefix.as_bytes(), &b" (No such file or directory)\n"[..]));
        assert_eq!(windows_missing_server_parts(first.as_bytes()), expected);
        assert_eq!(windows_missing_server_parts(next.as_bytes()), expected);

        if cfg!(windows) {
            assert_eq!(
                comparable_stderr(first.as_bytes()),
                comparable_stderr(next.as_bytes())
            );
        } else {
            // Even Windows-shaped stderr remains byte-for-byte on Unix.
            assert_eq!(
                comparable_stderr(first.as_bytes()),
                (first.as_bytes(), &[][..])
            );
            assert_eq!(
                comparable_stderr(next.as_bytes()),
                (next.as_bytes(), &[][..])
            );
            assert_ne!(
                comparable_stderr(first.as_bytes()),
                comparable_stderr(next.as_bytes())
            );
        }
    }

    #[test]
    fn comparison_retains_the_windows_endpoint_identity() {
        let first = diagnostic(FIRST_NONCE);
        let next = diagnostic(NEXT_NONCE);
        let expected = windows_missing_server_parts(first.as_bytes()).unwrap();
        for changed in [
            next.replace("-1000-il-", "-1001-il-"),
            next.replace(KEY, &"a".repeat(64)),
        ]
        .into_iter()
        .chain(["untrusted", "low", "high", "system"].map(|level| next.replace("medium", level)))
        {
            let actual = windows_missing_server_parts(changed.as_bytes())
                .expect("changed identity still has a valid managed diagnostic shape");
            assert_ne!(actual, expected, "{changed}");
            assert_ne!(
                comparable_stderr(changed.as_bytes()),
                comparable_stderr(first.as_bytes()),
                "{changed}"
            );
        }
    }

    #[test]
    fn comparison_preserves_other_errors_and_malformed_diagnostics() {
        let valid = diagnostic(FIRST_NONCE);
        let mut cases = vec![
            String::new(),
            "rmux: unknown option -- q\n".to_owned(),
            "error connecting to /tmp/r3q-test-123 (No such file or directory)\n".to_owned(),
            format!("warning: {valid}"),
            format!("{valid}extra output\n"),
            valid.trim_end().to_owned(),
        ];
        for (from, to) in [
            ("No such file or directory", "Permission denied"),
            ("No such file or directory", "Connection refused"),
            ("error connecting to", "no server running on"),
            (r"\\.\pipe\", r"\\server\pipe\"),
            ("rmux-", "tmux-"),
            ("S-1-", "S-x-"),
            ("-1000-il-", "-user-il-"),
            ("-1000-il-", "--il-"),
            ("S-1-5-21-100-200-300-1000", "S-1-5"),
            ("medium", "unknown"),
            ("-g-", "-legacy-"),
            (" (No such file or directory)", " No such file or directory"),
            ("\n", "\r\n"),
        ] {
            cases.push(valid.replace(from, to));
        }
        for malformed in [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            "é".repeat(32),
        ] {
            cases.push(valid.replace(KEY, &malformed));
        }
        for malformed in [
            String::new(),
            "a".repeat(31),
            "a".repeat(33),
            "A".repeat(32),
            "g".repeat(32),
            "-".repeat(32),
            "é".repeat(16),
        ] {
            cases.push(diagnostic(&malformed));
        }
        let mut invalid_utf8 = valid.into_bytes();
        invalid_utf8[0] = 0xff;
        for stderr in cases
            .into_iter()
            .map(String::into_bytes)
            .chain([invalid_utf8])
        {
            assert_eq!(windows_missing_server_parts(&stderr), None, "{stderr:?}");
            assert_eq!(comparable_stderr(&stderr), (&stderr[..], &[][..]));
        }
    }
}
