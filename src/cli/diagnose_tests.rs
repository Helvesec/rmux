use super::*;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(unix)]
#[test]
fn default_config_paths_match_startup_file_identity_selection() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _home = EnvVarGuard::capture("HOME");
    let _xdg = EnvVarGuard::capture("XDG_CONFIG_HOME");
    let root = std::env::temp_dir().join(format!(
        "rmux-config-identity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let home = root.join("home");
    let xdg = root.join("xdg");
    std::fs::create_dir_all(&home).expect("home fixture");
    std::fs::create_dir_all(xdg.join("rmux")).expect("xdg fixture");
    let first = home.join(".rmux.conf");
    std::fs::write(&first, "set -g @loaded once\n").expect("config fixture");
    std::fs::hard_link(&first, xdg.join("rmux/rmux.conf")).expect("config alias");
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", &xdg);

    assert_eq!(
        default_config_paths(),
        vec![
            PathBuf::from("/etc/rmux.conf"),
            first,
            home.join(".config/rmux/rmux.conf"),
        ],
        "diagnose must report each default file identity once, in startup order, while retaining missing candidates"
    );
}

struct EnvVarGuard {
    name: &'static str,
    value: Option<OsString>,
}

impl EnvVarGuard {
    fn capture(name: &'static str) -> Self {
        Self {
            name,
            value: std::env::var_os(name),
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.value.as_ref() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn parser_detects_diagnose_after_socket_flags() {
    let invocation = parse_invocation(&[
        OsString::from("-Ldiag"),
        OsString::from("-Tclipboard,RGB"),
        OsString::from("diagnose"),
        OsString::from("--json"),
    ])
    .expect("parse diagnose")
    .expect("diagnose invocation");

    assert_eq!(invocation.format, DiagnoseFormat::Json);
    assert_eq!(invocation.socket_name, Some(OsString::from("diag")));
    assert_eq!(
        invocation.terminal_features,
        vec!["clipboard".to_owned(), "RGB".to_owned()]
    );
}

#[test]
fn parser_ignores_non_diagnose_commands() {
    assert_eq!(
        parse_invocation(&[OsString::from("new-session")]).expect("parse"),
        None
    );
}

#[test]
fn json_renderer_escapes_strings() {
    assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
}

#[test]
fn path_redaction_replaces_home_prefix() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let _home = EnvVarGuard::capture("HOME");
    let home = std::env::temp_dir().join("rmux-diagnose-home");
    std::env::set_var("HOME", &home);

    assert_eq!(redact_path(&home.join("rmux.conf")), "~/rmux.conf");
}

#[test]
fn config_message_filter_uses_message_prefix_not_substrings() {
    assert_eq!(
        config_message_from_show_messages_line("123: user text config ignored: not diagnostic"),
        None
    );
    assert_eq!(
        config_message_from_show_messages_line("123: config ignored: old option")
            .expect("config message"),
        "123: config ignored: old option"
    );
    assert_eq!(
        config_message_from_show_messages_line("123: /tmp/not-config:23: user text"),
        None
    );
}

#[test]
fn config_message_filter_includes_source_file_location_diagnostics() {
    assert_eq!(
        config_message_from_show_messages_line("123: /tmp/tmp.abc.in:23: unmatched }")
            .expect("source-file config diagnostic"),
        "123: /tmp/tmp.abc.in:23: unmatched }"
    );
    assert_eq!(
        config_message_from_show_messages_line(
            "123: C:\\Users\\RMUX\\.tmux.conf:16: unknown command: nope"
        )
        .expect("windows source-file config diagnostic"),
        "123: C:\\Users\\RMUX\\.tmux.conf:16: unknown command: nope"
    );
}

#[test]
fn config_message_filter_redacts_home_paths() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let _home = EnvVarGuard::capture("HOME");
    let home = std::env::temp_dir().join("rmux-diagnose-home");
    std::env::set_var("HOME", &home);
    let raw = format!(
        "123: config error: {}:2: unknown command: nope",
        home.join(".tmux.conf").display()
    );

    let redacted = config_message_from_show_messages_line(&raw).expect("config message");

    assert!(redacted.contains("~/.tmux.conf"), "{redacted}");
    assert!(
        !redacted.contains(&home.display().to_string()),
        "{redacted}"
    );
}

#[cfg(windows)]
#[test]
fn detected_shell_uses_windows_resolver_module() {
    assert_ne!(detected_shell(), "");
}

#[test]
fn parser_projects_ordered_metadata_without_deduplicating_configs() {
    let args = [
        "-q2uLfirst",
        "-Llast",
        "-Sfirst",
        "-uSlast",
        "-ffirst",
        "-f",
        "second",
        "-uffirst",
        "-T RGB,clipboard,,RGB ",
        "-uTclipboard,256",
        "-cignored",
        "--",
        "diagnose",
        "--json",
    ]
    .map(OsString::from);
    let invocation = parse_invocation(&args).unwrap().unwrap();
    assert_eq!(invocation.socket_name, Some("last".into()));
    assert_eq!(invocation.socket_path, Some(PathBuf::from("last")));
    assert_eq!(
        invocation.config_files,
        ["first", "second", "first"].map(PathBuf::from)
    );
    assert_eq!(invocation.terminal_features, ["RGB", "clipboard", "256"]);
    assert!(invocation.assume_256_colors);
    assert!(invocation.utf8);
}

#[test]
fn parser_projects_opaque_socket_and_config_values_exactly() {
    #[cfg(unix)]
    let opaque = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![0xff, b'q'])
    };
    #[cfg(windows)]
    let opaque = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0xd800, 0x71, 0xdc00])
    };
    #[cfg(not(any(unix, windows)))]
    let opaque = OsString::from("native");
    for attached in [false, true] {
        let mut args = Vec::new();
        for flag in ['L', 'S', 'f', 'f', 'T'] {
            let mut option = OsString::from(format!("-u{flag}"));
            if attached {
                option.push(&opaque);
                args.push(option);
            } else {
                args.extend([option, opaque.clone()]);
            }
        }
        args.push("diagnose".into());
        let invocation = parse_invocation(&args).unwrap().unwrap();
        assert_eq!(invocation.socket_name, Some(opaque.clone()));
        assert_eq!(invocation.socket_path, Some(PathBuf::from(&opaque)));
        assert_eq!(invocation.config_files, vec![PathBuf::from(&opaque); 2]);
        #[cfg(any(unix, windows))]
        assert!(invocation.terminal_features.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn default_config_paths_characterizes_non_utf8_home() {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn value_json(value: &Option<OsString>) -> serde_json::Value {
        serde_json::json!({
            "present": value.is_some(),
            "hex": value.as_ref().map(|value| hex(value.as_bytes())),
        })
    }

    // Keep the real environment lock through guard Drop and restoration checks.
    let _lock = ENV_LOCK.lock().expect("env lock");
    if run_non_utf8_home_in_clean_child(
        "cli::diagnose::tests::default_config_paths_characterizes_non_utf8_home",
    ) {
        return;
    }
    assert!(
        std::env::var_os("HOME").is_none(),
        "requires clean child HOME"
    );
    assert!(
        std::env::var_os("XDG_CONFIG_HOME").is_none(),
        "requires clean child XDG_CONFIG_HOME"
    );
    let fixture_root = PathBuf::from(
        std::env::var_os("RMUX_L073_FIXTURE_ROOT").expect("owned characterization fixture root"),
    );
    assert!(fixture_root.is_absolute());
    assert!(fixture_root.as_os_str().as_bytes().is_ascii());

    struct Case {
        id: &'static str,
        home: Option<&'static [u8]>,
        xdg: bool,
        expected: &'static [&'static [u8]],
    }
    // Frozen R2 cases: ordered raw paths, including the existing non-UTF8 split.
    // Relative byte paths are rooted in this case's owned ASCII directory.
    let cases: [Case; 8] = [
        Case {
            id: "HOME-absent-XDG-absent",
            home: None,
            xdg: false,
            expected: &[b"/etc/rmux.conf"],
        },
        Case {
            id: "HOME-absent-XDG-utf8",
            home: None,
            xdg: true,
            expected: &[b"/etc/rmux.conf", b"xdg/rmux/rmux.conf"],
        },
        Case {
            id: "HOME-empty-XDG-absent",
            home: Some(b""),
            xdg: false,
            expected: &[b"/etc/rmux.conf"],
        },
        Case {
            id: "HOME-empty-XDG-utf8",
            home: Some(b""),
            xdg: true,
            expected: &[b"/etc/rmux.conf", b"xdg/rmux/rmux.conf"],
        },
        Case {
            id: "HOME-utf8-XDG-absent",
            home: Some(b"home"),
            xdg: false,
            expected: &[
                b"/etc/rmux.conf",
                b"home/.rmux.conf",
                b"home/.config/rmux/rmux.conf",
            ],
        },
        Case {
            id: "HOME-utf8-XDG-utf8",
            home: Some(b"home"),
            xdg: true,
            expected: &[
                b"/etc/rmux.conf",
                b"home/.rmux.conf",
                b"xdg/rmux/rmux.conf",
                b"home/.config/rmux/rmux.conf",
            ],
        },
        Case {
            id: "HOME-non_utf8-XDG-absent",
            home: Some(b"home-\xff"),
            xdg: false,
            expected: &[
                b"/etc/rmux.conf",
                b"home-\xff/.rmux.conf",
                b"home-\xff/.config/rmux/rmux.conf",
            ],
        },
        Case {
            id: "HOME-non_utf8-XDG-utf8",
            home: Some(b"home-\xff"),
            xdg: true,
            expected: &[
                b"/etc/rmux.conf",
                b"home-\xff/.rmux.conf",
                b"xdg/rmux/rmux.conf",
                b"home-\xff/.config/rmux/rmux.conf",
            ],
        },
    ];
    let mut observed = BTreeSet::new();
    for case in cases {
        assert!(observed.insert(case.id), "duplicate characterization case");
        assert!(std::env::var_os("HOME").is_none());
        assert!(std::env::var_os("XDG_CONFIG_HOME").is_none());
        // Capture absence before installing any non-UTF8 bytes. Both existing
        // guards stay live across every fallible operation and resolver call.
        let home_guard = EnvVarGuard::capture("HOME");
        let xdg_guard = EnvVarGuard::capture("XDG_CONFIG_HOME");
        let case_root = fixture_root.join(case.id);
        std::fs::create_dir_all(&case_root).expect("create ASCII fixture root");
        let rooted = |relative: &[u8]| {
            let mut bytes = case_root.as_os_str().as_bytes().to_vec();
            bytes.push(b'/');
            bytes.extend_from_slice(relative);
            bytes
        };
        let home = case.home.map(|relative| {
            OsString::from_vec(if relative.is_empty() {
                Vec::new()
            } else {
                rooted(relative)
            })
        });
        let xdg = case.xdg.then(|| OsString::from_vec(rooted(b"xdg")));
        for (name, value) in [("HOME", &home), ("XDG_CONFIG_HOME", &xdg)] {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
            assert_eq!(std::env::var_os(name), *value);
        }
        let home_observed = std::env::var_os("HOME");
        let xdg_observed = std::env::var_os("XDG_CONFIG_HOME");
        let expected: Vec<Vec<u8>> = case
            .expected
            .iter()
            .map(|path| {
                if path.starts_with(b"/") {
                    path.to_vec()
                } else {
                    rooted(path)
                }
            })
            .collect();
        let mut owned_candidates = Vec::new();
        if let Some(value) = home.as_ref().filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            owned_candidates.push(path.join(".rmux.conf"));
            owned_candidates.push(path.join(".config/rmux/rmux.conf"));
        }
        if let Some(value) = &xdg {
            owned_candidates.push(PathBuf::from(value).join("rmux/rmux.conf"));
        }
        let assert_no_config_files = || {
            assert!(
                std::fs::read_dir(&case_root)
                    .expect("read owned fixture root")
                    .next()
                    .is_none(),
                "fixture root must stay empty; never create the non-UTF8 HOME"
            );
            for candidate in &owned_candidates {
                let error = std::fs::symlink_metadata(candidate)
                    .expect_err("owned config candidate must not exist");
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
        };
        assert_no_config_files();
        // Exactly one call to the real resolver for this frozen case.
        let actual: Vec<Vec<u8>> = default_config_paths()
            .iter()
            .map(|path| path.as_os_str().as_bytes().to_vec())
            .collect();
        assert_eq!(actual, expected, "case {}", case.id);
        assert_no_config_files();
        drop(xdg_guard);
        drop(home_guard);
        assert!(std::env::var_os("HOME").is_none());
        assert!(std::env::var_os("XDG_CONFIG_HOME").is_none());
        println!(
            "RMUX_L073_OBSERVATION {}",
            serde_json::json!({
                "case_id": case.id,
                "resolver": "diagnose",
                "pid": std::process::id(),
                "HOME": value_json(&home_observed),
                "XDG_CONFIG_HOME": value_json(&xdg_observed),
                "actual_paths_hex": actual.iter().map(|path| hex(path)).collect::<Vec<_>>(),
                "expected_paths_hex": expected.iter().map(|path| hex(path)).collect::<Vec<_>>(),
                "restored_absent": true,
            })
        );
    }
    assert_eq!(observed.len(), 8);
}

#[cfg(unix)]
#[test]
fn diagnose_shared_model_redacts_environment_and_does_not_import_config() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let names = [
        "HOME",
        "USERPROFILE",
        "SHELL",
        "TERM",
        "TERM_PROGRAM",
        "WT_SESSION",
        "RMUX_DIAGNOSE_TEST_SECRET",
    ];
    let _guards: Vec<_> = names
        .iter()
        .map(|name| EnvVarGuard::capture(name))
        .collect();
    let root = std::env::temp_dir().join(format!("rmux-diagnose-private-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let marker = root.join("executed");
    let config = root.join("import.conf");
    std::fs::write(&config, format!("run-shell 'touch {}'\n", marker.display())).unwrap();
    std::env::set_var("HOME", &root);
    std::env::set_var("RMUX_DIAGNOSE_TEST_SECRET", "must-not-be-disclosed");
    std::env::remove_var("USERPROFILE");
    std::env::remove_var("WT_SESSION");
    std::env::set_var("SHELL", root.join("bin/bash"));
    std::env::set_var("TERM", "xterm-256color");
    std::env::set_var("TERM_PROGRAM", "iTerm.app");
    let invocation = parse_invocation(&[
        "-S".into(),
        root.join("absent.sock").into_os_string(),
        "-f".into(),
        config.into_os_string(),
        "diagnose".into(),
    ])
    .unwrap()
    .unwrap();
    let (mut report, _) = DiagnoseReport::collect(&invocation).unwrap();
    assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(report.os_name, std::env::consts::OS);
    assert_eq!(report.os_arch, std::env::consts::ARCH);
    // Only host/build identity varies; every other field is a frozen golden.
    report.version = "<version>".into();
    report.os_name = "<os>".into();
    report.os_arch = "<arch>".into();
    report.os_version = "<os-version>".into();
    let human = report.render_human();
    let json = report.render_json();
    assert!(
        !human.contains(root.to_str().unwrap()),
        "human leaks private HOME"
    );
    assert!(
        !json.contains(root.to_str().unwrap()),
        "JSON leaks private HOME"
    );
    assert!(
        !marker.exists(),
        "diagnose must never execute imported configuration"
    );
    assert!(human.contains("shell: ~/bin/bash\n"));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["shell"], "~/bin/bash");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(
        value["compatibility"]["registry"],
        "tests/reference/tmux_compat/divergences.toml"
    );
    assert!(human.contains("tests/reference/tmux_compat/divergences.toml"));
    assert!(!human.contains("must-not-be-disclosed"));
    assert!(!json.contains("must-not-be-disclosed"));
    assert_eq!(
        human,
        include_str!("../../tests/reference/diagnose/human.txt")
    );
    assert_eq!(
        json,
        include_str!("../../tests/reference/diagnose/report.json")
    );
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(value["compatibility"]["registry"].as_str().unwrap())
        .is_file());
}

#[cfg(unix)]
#[test]
fn non_utf8_home_characterization_isolates_parent_environment() {
    use std::os::unix::ffi::OsStringExt;

    let parent_home = std::ffi::OsString::from_vec(b"/parent-home-\xff".to_vec());
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "cli::diagnose::tests::default_config_paths_characterizes_non_utf8_home",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("HOME", &parent_home)
        .env("XDG_CONFIG_HOME", "/parent-xdg")
        .env_remove("RMUX_L073_FIXTURE_ROOT")
        .output()
        .expect("run characterization with inherited parent environment");
    assert!(
        output.status.success(),
        "characterization must own its clean child: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("RMUX_L073_OBSERVATION ").count(), 8);
}

#[cfg(unix)]
fn run_non_utf8_home_in_clean_child(test_name: &str) -> bool {
    use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt};
    use std::path::PathBuf;

    let names = ["HOME", "XDG_CONFIG_HOME", "RMUX_L073_FIXTURE_ROOT"];
    let parent = names.map(std::env::var_os);
    // The existing externally isolated recipe already owns a fixture.
    if parent[0].is_none() && parent[1].is_none() && parent[2].is_some() {
        return false;
    }
    let temporary = std::env::temp_dir();
    let base = if temporary.is_absolute() && temporary.as_os_str().as_bytes().is_ascii() {
        temporary
    } else {
        PathBuf::from("/tmp")
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("fixture clock")
        .as_nanos();
    let root = base.join(format!("rmux-non-utf8-home-{}-{nonce}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("create private ASCII characterization fixture");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([test_name, "--exact", "--test-threads=1", "--nocapture"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("RMUX_L073_FIXTURE_ROOT", &root)
        .output()
        .expect("run clean characterization child");
    std::fs::remove_dir_all(&root).expect("remove owned characterization fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        names.map(std::env::var_os),
        parent,
        "parent environment changed"
    );
    assert!(
        output.status.success(),
        "clean characterization child failed"
    );
    assert_eq!(stdout.matches("RMUX_L073_OBSERVATION ").count(), 8);
    true
}

// R1197-F1: frozen first-stop precedence, including native opaque argv.
fn diagnose_precedence_stops() -> Vec<(OsString, i32, &'static str)> {
    let mut stops = vec![
        ("--help".into(), 0, "usage: rmux diagnose [--human|--json]  --json: Emit JSON results on stdout; command errors use text on stderr with a nonzero exit and empty stdout. Supported unsuccessful results remain JSON"),
        ("--bad".into(), 1, "rmux diagnose: unknown argument '--bad'"),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        stops.push((
            OsString::from_vec(vec![0xff]),
            1,
            "rmux diagnose: arguments must be valid UTF-8",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        stops.push((
            OsString::from_wide(&[0xd800]),
            1,
            "rmux diagnose: arguments must be valid UTF-8",
        ));
    }
    stops
}

fn assert_diagnose_precedence(arguments: &[OsString], code: i32, message: &str) {
    let failure = parse_diagnose_format(arguments).expect_err("frozen stop");
    assert_eq!(failure.exit_code(), code, "{arguments:?}");
    assert_eq!(failure.message(), message, "{arguments:?}");
    assert_eq!(failure.use_stderr(), code != 0, "{arguments:?}");
    assert_eq!(
        failure.message_termination(),
        super::super::error::ExitMessageTermination::Line
    );
    match crate::cli_args::parse_diagnose_args(arguments) {
        Err(crate::cli_args::DiagnoseArgumentError::Help) => assert_eq!(code, 0),
        Err(crate::cli_args::DiagnoseArgumentError::Invalid(actual)) => {
            assert_eq!(code, 1);
            assert_eq!(actual, message, "{arguments:?}");
        }
        Ok(_) => panic!("accepted stopped argv: {arguments:?}"),
    }
}

#[test]
fn diagnose_format_conflicts_precede_later_help_and_invalid() {
    for formats in [["--human", "--json"], ["--json", "--human"]] {
        for (stop, _, _) in diagnose_precedence_stops() {
            let arguments = [formats[0].into(), formats[1].into(), stop];
            assert_diagnose_precedence(
                &arguments,
                1,
                "rmux diagnose: choose only one of --human or --json",
            );
        }
    }
}

#[test]
fn diagnose_format_early_help_and_invalid_precede_later_conflicts() {
    for formats in [["--human", "--json"], ["--json", "--human"]] {
        for (stop, code, message) in diagnose_precedence_stops() {
            for arguments in [
                [stop.clone(), formats[0].into(), formats[1].into()],
                [formats[0].into(), stop.clone(), formats[1].into()],
            ] {
                assert_diagnose_precedence(&arguments, code, message);
            }
        }
    }
}

#[test]
fn diagnose_format_identical_flags_remain_repeatable() {
    for (flag, expected) in [
        ("--human", DiagnoseFormat::Human),
        ("--json", DiagnoseFormat::Json),
    ] {
        let arguments = [flag.into(), flag.into(), flag.into()];
        assert_eq!(parse_diagnose_format(&arguments).unwrap(), expected);
        let parsed = crate::cli_args::parse_diagnose_args(&arguments);
        assert_eq!(parsed.ok().expect("repeated format").json, flag == "--json");
        for (stop, code, message) in diagnose_precedence_stops() {
            let arguments = [flag.into(), flag.into(), stop];
            assert_diagnose_precedence(&arguments, code, message);
        }
    }
}

#[test]
fn closing_brace_diagnostic_is_visible_and_private() {
    let error = rmux_core::command_parser::CommandParser::new()
        .parse_source_file("\n}")
        .expect_err("closing brace");
    let raw = format!(
        "123: /config/test.conf:{}: {}",
        error.line(),
        error.message()
    );
    assert_eq!(raw, "123: /config/test.conf:2: syntax error");
    let filtered = config_message_from_show_messages_line(&raw).expect("syntax diagnostic");
    assert_eq!(private_config_message(&filtered), raw);
    assert_eq!(
        private_config_message("123: config error: /config/test.conf:2: syntax error SECRET"),
        "123: config error: [redacted]"
    );
    for line in [
        "123: /config/test.conf:2: syntax error SECRET",
        "123: /config/test.conf:x: syntax error",
        "123: user text syntax error",
    ] {
        assert_eq!(config_message_from_show_messages_line(line), None);
    }
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _home = EnvVarGuard::capture("HOME");
    std::env::set_var("HOME", "/private/example-home");
    let filtered = config_message_from_show_messages_line(
        "123: /private/example-home/test.conf:2: syntax error",
    )
    .expect("home diagnostic");
    assert_eq!(
        private_config_message(&filtered),
        "123: ~/test.conf:2: syntax error"
    );
}
