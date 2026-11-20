mod common_cross;

use std::error::Error;
use std::fs;

use common_cross::CrossPlatformHarness;

#[test]
fn source_file_invalid_colon_diagnostic_is_shared_by_sibling_commands() -> Result<(), Box<dyn Error>>
{
    let harness = CrossPlatformHarness::new("source-invalid-colon-siblings")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;
    let source = harness.tmpdir().join("invalid-flag.conf");
    let source_path = source.to_str().ok_or("non-UTF-8 source path")?;

    for command in [
        "display-message",
        "list-sessions",
        "list-windows",
        "list-panes",
        "new-window",
        "set-option",
        "send-keys",
        "source-file",
    ] {
        for (flag, kind) in [("-:", "invalid"), ("-Q", "unknown")] {
            fs::write(&source, format!("{command} {flag}\n"))?;
            let output = harness.run(["source-file", source_path])?;
            assert_eq!(
                output.status.code(),
                Some(1),
                "{command} {flag}: {output:?}"
            );
            assert_eq!(
                output.stdout,
                format!("{source_path}:1: command {command}: {kind} flag {flag}\n").as_bytes(),
                "{command} {flag}: {output:?}"
            );
            assert!(output.stderr.is_empty(), "{command} {flag}: {output:?}");
        }
    }
    Ok(())
}

#[test]
fn source_file_invalid_colon_preserves_unknown_flags_literals_and_queue_canaries(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-invalid-colon")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;
    let source = harness.tmpdir().join("invalid-flag.conf");
    let source_path = source.to_str().ok_or("non-UTF-8 source path")?;

    for (literal, diagnostic) in [
        ("-:", "invalid flag -:"),
        ("-:1: literal output", "invalid flag -:"),
        ("-Q", "unknown flag -Q"),
    ] {
        harness.success(["set-option", "-gu", "@source-flag-canary"])?;
        fs::write(
            &source,
            format!(
                "display-message -p '{literal}' ; set-option -g @source-flag-canary unexpected\n"
            ),
        )?;
        let rejected = harness.run(["source-file", source_path])?;
        assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
        assert_eq!(
            rejected.stdout,
            format!("{source_path}:1: command display-message: {diagnostic}\n").as_bytes(),
            "{rejected:?}"
        );
        assert!(rejected.stderr.is_empty(), "{rejected:?}");
        assert_eq!(
            harness.stdout(["show-options", "-gqv", "@source-flag-canary"])?,
            ""
        );

        fs::write(
            &source,
            format!(
                "display-message -p -- '{literal}' ; set-option -g @source-flag-canary expected\n"
            ),
        )?;
        let separated = harness.run(["source-file", source_path])?;
        assert_eq!(separated.status.code(), Some(0), "{separated:?}");
        assert_eq!(separated.stdout, format!("{literal}\n").as_bytes());
        assert!(separated.stderr.is_empty(), "{separated:?}");
        assert_eq!(
            harness.stdout(["show-options", "-gqv", "@source-flag-canary"])?,
            "expected\n"
        );
    }
    Ok(())
}
