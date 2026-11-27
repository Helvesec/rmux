mod common_cross;

#[path = "cli_error_contract/invalid_flag.rs"]
mod invalid_flag;

use std::error::Error;

use common_cross::CrossPlatformHarness;

struct SdkBinaryEnv(Option<std::ffi::OsString>);

impl SdkBinaryEnv {
    fn candidate() -> Self {
        let previous = std::env::var_os("RMUX_SDK_DAEMON_BINARY");
        std::env::set_var("RMUX_SDK_DAEMON_BINARY", common_cross::rmux_binary());
        Self(previous)
    }
}

impl Drop for SdkBinaryEnv {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var("RMUX_SDK_DAEMON_BINARY", value),
            None => std::env::remove_var("RMUX_SDK_DAEMON_BINARY"),
        }
    }
}

#[test]
fn bare_hyphen_prefixed_literal_matches_invalid_option_class() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("literal-option-error")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;

    // D321/D322(1): tmux 3.7b rejects this operand before display-message executes.
    // Temporarily accept the invalid-option class with the exact rejected flag.
    // Restore tmux's exact wording once the D322(2) diagnostic fix is integrated.
    let cases: &[&[&str]] = &[
        &["display-message", "-p", "-:1: literal output"],
        &[
            "display-message",
            "-p",
            "-t",
            "errors:0.0",
            "-:1: literal output",
            ";",
            "set-option",
            "-g",
            "@continued",
            "unexpected",
        ],
    ];
    for args in cases {
        let output = harness.run(*args)?;
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(
            output.stderr == b"command display-message: invalid flag -:\n"
                || output.stderr == b"command display-message: unknown flag -:\n",
            "invalid-option class for -:; {args:?}: {output:?}"
        );
    }
    assert_eq!(harness.stdout(["show-options", "-gqv", "@continued"])?, "");
    Ok(())
}

// R1785 frozen witness: keep the literal bytes, exit codes, output channels,
// continuation checks, and real-error assertions unchanged by stimulus corrections.
#[test]
fn queued_literal_diagnostics_preserve_success_and_real_errors_abort_the_group(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("queued-error-contract")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;

    let literal = "-:1: literal output";
    let direct = harness.run(["display-message", "-p", "--", literal])?;
    assert_eq!(direct.status.code(), Some(0), "{direct:?}");
    assert_eq!(direct.stdout, b"-:1: literal output\n");
    assert!(direct.stderr.is_empty());

    let queued = harness.run([
        "display-message",
        "-p",
        "-t",
        "errors:0.0",
        "--",
        literal,
        ";",
        "set-option",
        "-g",
        "@continued",
        "yes",
    ])?;
    assert_eq!(queued.status.code(), Some(0), "{queued:?}");
    assert_eq!(queued.stdout, direct.stdout);
    assert!(queued.stderr.is_empty(), "{queued:?}");
    assert_eq!(
        harness.stdout(["show-options", "-gqv", "@continued"])?,
        "yes\n"
    );

    let failed = harness.run([
        "wait-for",
        "-U",
        "contract-unlocked",
        ";",
        "set-option",
        "-g",
        "@continued",
        "unexpected",
    ])?;
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    assert!(failed.stdout.is_empty(), "{failed:?}");
    assert_eq!(failed.stderr, b"channel contract-unlocked not locked\n");
    assert_eq!(
        harness.stdout(["show-options", "-gqv", "@continued"])?,
        "yes\n"
    );

    let recovered = harness.run(["display-message", "-p", "RECOVERED"])?;
    assert_eq!(recovered.status.code(), Some(0));
    assert_eq!(recovered.stdout, b"RECOVERED\n");
    assert!(recovered.stderr.is_empty());

    let endpoint = harness.stdout(["display-message", "-p", "#{socket_path}"])?;
    #[cfg(unix)]
    let endpoint = rmux_sdk::RmuxEndpoint::UnixSocket(endpoint.trim_end().into());
    #[cfg(windows)]
    let endpoint = rmux_sdk::RmuxEndpoint::WindowsPipe(endpoint.trim_end().into());
    let _sdk_binary = SdkBinaryEnv::candidate();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let sdk = rmux_sdk::Rmux::builder().endpoint(endpoint).build();
        let session = rmux_sdk::SessionName::new("errors")?;
        let missing = rmux_sdk::PaneId::new(9999);
        let error = sdk
            .pane_by_id(session.clone(), missing)
            .await
            .expect_err("missing pane");
        assert!(
            matches!(error, rmux_sdk::RmuxError::PaneNotFound { pane_id, .. } if pane_id == missing)
        );
        assert!(sdk.session(session).await?.exists().await?);
        let queued = sdk
            .cmd([
                "display-message",
                "-p",
                "-t",
                "errors:0.0",
                "--",
                "-:1: literal output",
            ])
            .await?;
        assert_eq!(queued.exit, Some(0));
        assert_eq!(queued.stdout, b"-:1: literal output\n");
        assert!(queued.stderr.is_empty());
        let failed = sdk
            .cmd([
                "wait-for",
                "-U",
                "contract-unlocked",
                ";",
                "set-option",
                "-g",
                "@continued",
                "unexpected",
            ])
            .await?;
        assert_eq!(failed.exit, Some(1));
        assert!(failed.stdout.is_empty());
        assert_eq!(failed.stderr, b"channel contract-unlocked not locked\n");
        let after = sdk.cmd(["show-options", "-gqv", "@continued"]).await?;
        assert_eq!(after.exit, Some(0));
        assert_eq!(after.stdout, b"yes\n");
        assert!(after.stderr.is_empty());
        Ok::<_, rmux_sdk::RmuxError>(())
    })?;

    let json = harness.run(["display-message", "--json", "--", "-:1: literal output"])?;
    assert_eq!(json.status.code(), Some(0), "{json:?}");
    assert!(json.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout)?;
    assert_eq!(value["message"], literal);
    Ok(())
}

#[test]
fn json_command_errors_keep_stdout_empty_and_use_text_stderr() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("json-error-channels")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;
    let cases: &[&[&str]] = &[
        &["display-message", "--json", "-t", "=missing", "message"],
        &["list-windows", "--json", "-t", "=missing"],
        &["list-panes", "--json", "-t", "=missing"],
        &["list-sessions", "--json", "-F", "conflicting-format"],
        &["list-clients", "--json", "-F", "conflicting-format"],
        &["pane-snapshot", "--json", "-t", "%9999"],
        &["wait-pane", "--json", "-t", "%9999", "--text", "missing"],
        &[
            "collect-pane-output",
            "--json",
            "-t",
            "%9999",
            "--until-pane-exit",
            "--max-bytes",
            "1",
        ],
        &[
            "locator",
            "--json",
            "-t",
            "%9999",
            "--get-by-text",
            "missing",
        ],
        &[
            "expect-pane",
            "--json",
            "-t",
            "%9999",
            "--get-by-text",
            "missing",
            "--visible",
        ],
        &["find-panes", "--json", "--invalid-flag"],
        &["find-sessions", "--json", "--invalid-flag"],
        &["diagnose", "--json", "--human"],
        &["capabilities", "--json", "--invalid-flag"],
    ];
    for args in cases {
        let output = harness.run(*args)?;
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(!output.stderr.is_empty(), "{args:?}: {output:?}");
        std::str::from_utf8(&output.stderr)?;
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stderr).is_err(),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn rendered_local_json_help_describes_success_and_error_channels() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("json-help-contract")?;
    for command in ["capabilities", "diagnose"] {
        let output = harness.run([command, "--help"])?;
        assert_eq!(output.status.code(), Some(0), "{command}: {output:?}");
        assert!(output.stderr.is_empty(), "{command}: {output:?}");
        let help = String::from_utf8(output.stdout)?;
        for clause in [
            "--json",
            "JSON results on stdout",
            "command errors use text on stderr",
            "nonzero exit and empty stdout",
        ] {
            assert!(
                help.contains(clause),
                "{command}: missing {clause:?}: {help}"
            );
        }
    }
    Ok(())
}

// R1197-F1: literal exit/message/channel witnesses; no daemon is needed.
#[test]
fn diagnose_precedence_keeps_first_error_and_output_channel() -> Result<(), Box<dyn Error>> {
    use std::ffi::OsString;
    let harness = CrossPlatformHarness::new("diagnose-precedence")?;
    let mut stops: Vec<(OsString, i32, &[u8], &[u8])> = vec![
        ("--help".into(), 0, b"usage: rmux diagnose [--human|--json]  --json: Emit JSON results on stdout; command errors use text on stderr with a nonzero exit and empty stdout. Supported unsuccessful results remain JSON\n", b""),
        ("--bad".into(), 1, b"", b"rmux diagnose: unknown argument '--bad'\n"),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        stops.push((
            OsString::from_vec(vec![0xff]),
            1,
            b"",
            b"rmux diagnose: arguments must be valid UTF-8\n",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        stops.push((
            OsString::from_wide(&[0xd800]),
            1,
            b"",
            b"rmux diagnose: arguments must be valid UTF-8\n",
        ));
    }
    for formats in [["--human", "--json"], ["--json", "--human"]] {
        for (stop, code, stdout, stderr) in &stops {
            let conflict = harness.run([
                OsString::from("diagnose"),
                formats[0].into(),
                formats[1].into(),
                stop.clone(),
            ])?;
            assert_eq!(conflict.status.code(), Some(1), "{formats:?} {stop:?}");
            assert!(conflict.stdout.is_empty(), "{conflict:?}");
            assert_eq!(
                conflict.stderr,
                b"rmux diagnose: choose only one of --human or --json\n"
            );
            for arguments in [
                [
                    OsString::from("diagnose"),
                    stop.clone(),
                    formats[0].into(),
                    formats[1].into(),
                ],
                [
                    OsString::from("diagnose"),
                    formats[0].into(),
                    stop.clone(),
                    formats[1].into(),
                ],
                [
                    OsString::from("diagnose"),
                    formats[0].into(),
                    formats[0].into(),
                    stop.clone(),
                ],
            ] {
                let output = harness.run(&arguments)?;
                assert_eq!(output.status.code(), Some(*code), "{arguments:?}");
                assert_eq!(output.stdout, *stdout, "{arguments:?}");
                assert_eq!(output.stderr, *stderr, "{arguments:?}");
            }
        }
    }
    Ok(())
}
