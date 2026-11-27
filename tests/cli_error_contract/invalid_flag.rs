use std::error::Error;

use super::common_cross::CrossPlatformHarness;

#[test]
fn invalid_flag_diagnostic_matches_tmux_without_changing_literal_parsing(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("invalid-flag-diagnostic")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;

    // D321/D322: freeze the exact tmux diagnostic independently of the
    // temporary invalid-option class in the inherited S2272 fixture.
    for literal in ["-:1: literal output", "-Q"] {
        let flag = if literal.starts_with("-:") {
            "-:"
        } else {
            "-Q"
        };
        for queued in [false, true] {
            let mut args = vec!["display-message", "-p", literal];
            if queued {
                args.extend([
                    ";",
                    "set-option",
                    "-g",
                    "@invalid-flag-canary",
                    "unexpected",
                ]);
            }
            let output = harness.run(args)?;
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(
                output.stderr,
                format!(
                    "command display-message: {} flag {flag}\n",
                    if flag == "-Q" { "unknown" } else { "invalid" }
                )
                .as_bytes(),
                "{output:?}"
            );
        }
        assert_eq!(
            harness.stdout(["show-options", "-gqv", "@invalid-flag-canary"])?,
            ""
        );

        let separated = harness.run(["display-message", "-p", "--", literal])?;
        assert_eq!(separated.status.code(), Some(0), "{separated:?}");
        assert_eq!(separated.stdout, format!("{literal}\n").as_bytes());
        assert!(separated.stderr.is_empty(), "{separated:?}");
    }
    Ok(())
}

#[test]
fn unknown_flags_and_extra_arguments_print_tmux_stderr_and_exit_code() -> Result<(), Box<dyn Error>>
{
    let harness = CrossPlatformHarness::new("unknown-flag-extra-argument")?;
    harness.success(["new-session", "-d", "-s", "errors"])?;

    // tmux 3.7b with a server on a private socket: rc=1, empty stdout and
    // this exact stderr, instead of clap's error and usage block.
    let cases: &[(&[&str], &str)] = &[
        (&["ls", "-Z"], "command list-sessions: unknown flag -Z\n"),
        (&["has", "-Z"], "command has-session: unknown flag -Z\n"),
        (&["lsw", "-Q"], "command list-windows: unknown flag -Q\n"),
        (
            &["capture-pane", "-p", "-Q"],
            "command capture-pane: unknown flag -Q\n",
        ),
        (
            &["ls", "extra"],
            "command list-sessions: too many arguments (need at most 0)\n",
        ),
        (
            &["kill-server", "extra"],
            "command kill-server: too many arguments (need at most 0)\n",
        ),
        (
            &["ls", "--", "extra"],
            "command list-sessions: too many arguments (need at most 0)\n",
        ),
        (
            &["ls", "--bogus"],
            "command list-sessions: invalid flag --\n",
        ),
        (&["ls", "-."], "command list-sessions: invalid flag -.\n"),
        (&["neww", "-."], "command new-window: invalid flag -.\n"),
        (
            &["ls", "-?"],
            "usage: list-sessions [-r] [-F format] [-f filter] [-O order]\n",
        ),
        (&["kill-server", "-?"], "usage: kill-server \n"),
    ];
    for (args, expected) in cases {
        for queued in [false, true] {
            let mut argv = args.to_vec();
            if queued {
                argv.extend([";", "set-option", "-g", "@unknown-flag-canary", "ran"]);
            }
            let output = harness.run(argv.clone())?;
            assert_eq!(output.status.code(), Some(1), "{argv:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{argv:?}: {output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                *expected,
                "{argv:?}"
            );
        }
    }
    assert_eq!(
        harness.stdout(["show-options", "-gqv", "@unknown-flag-canary"])?,
        ""
    );
    // kill-server with an extra argument must not stop the server.
    assert_eq!(harness.stdout(["has-session", "-t", "errors"])?, "");
    Ok(())
}
