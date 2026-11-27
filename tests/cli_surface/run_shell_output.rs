use super::*;

#[test]
fn run_shell_command_output_survives_execution_error() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-command-output")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "base", "/bin/sh"])?);
    for command in ["new-session", "resize-window", "resize-pane"] {
        for flag in ["-x", "-y"] {
            for value in [
                "bad",
                "-1",
                "65536",
                "9223372036854775808",
                "-9223372036854775809",
            ] {
                if command == "resize-pane" && value == "65536" {
                    continue;
                }
                let body = format!(
                    "display-message -p before ; {command} {flag} {value} {} ; display-message -p after",
                    if command == "new-session" { "-d /bin/sh" } else { "" }
                );
                for prefix in [
                    vec!["run-shell", "-C"],
                    vec!["run", "-C"],
                    vec!["run-shell", "-d", "0.001", "-C"],
                    vec!["run-shell", "-t", "base:0.0", "-C"],
                ] {
                    let mut args = prefix;
                    args.push(&body);
                    let output = harness.run(&args)?;
                    assert_eq!(output.status.code(), Some(1), "{args:?}");
                    assert_eq!(stdout(&output), "before\n", "{args:?}");
                    assert!(!stderr(&output).is_empty(), "{args:?}");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn run_shell_command_output_keeps_parse_errors_eager() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-command-parse")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "base", "/bin/sh"])?);
    for body in [
        "display-message -p before ; resize-window -x",
        "display-message -p before ; resize-window -Q",
        "display-message -p before ; not-a-command",
        "display-message -p before ; if-shell -F 1 { display-message -Q }",
    ] {
        let body = format!("set-buffer -b syntax-effect no ; {body}");
        let output = harness.run(&["run-shell", "-C", &body])?;
        assert_eq!(output.status.code(), Some(1), "{body}");
        assert_eq!(stdout(&output), "", "{body}");
        assert!(!harness
            .run(&["show-buffer", "-b", "syntax-effect"])?
            .status
            .success());
        assert!(!stderr(&output).is_empty(), "{body}");
    }
    Ok(())
}

#[test]
fn run_shell_command_output_keeps_nested_and_shell_neighbors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-command-neighbors")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "base", "/bin/sh"])?);
    let direct_error = harness.run(&["run-shell", "-C", "resize-pane -x bad"])?;
    for body in [
        "display-message -p before ; resize-pane -x bad ; display-message -p after",
        "run-shell -C 'display-message -p before ; resize-pane -x bad ; display-message -p after'",
        "if-shell -F 1 'display-message -p before ; resize-pane -x bad ; display-message -p after'",
    ] {
        let output = harness.run(&["run-shell", "-C", body])?;
        assert_eq!(output.status.code(), Some(1), "{body}");
        assert_eq!(stdout(&output), "before\n", "{body}");
        assert_eq!(stderr(&output), stderr(&direct_error), "{body}");
    }
    let success = harness.run(&[
        "run-shell",
        "-C",
        "display-message -p before ; display-message -p after",
    ])?;
    assert_eq!(success.status.code(), Some(0));
    assert_eq!(stderr(&success), "");
    assert_eq!(stdout(&success), "before\nafter\n");
    let shell = harness.run(&["run-shell", "printf shell; exit 7"])?;
    assert_eq!(shell.status.code(), Some(7));
    assert_eq!(stdout(&shell), "shell\n'printf shell; exit 7' returned 7\n");
    assert_eq!(stderr(&shell), "");
    // Existing product validation, unlike tmux's accepted negative delay.
    let invalid_delay =
        harness.run(&["run-shell", "-d", "-1", "-C", "display-message -p skipped"])?;
    assert_eq!(invalid_delay.status.code(), Some(1));
    assert_eq!(stdout(&invalid_delay), "");
    assert_eq!(
        stderr(&invalid_delay),
        "run-shell -d expects a non-negative finite delay\n"
    );
    Ok(())
}

#[test]
fn run_shell_command_output_error_keeps_parent_tail() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-command-parent-tail")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "base", "/bin/sh"])?);
    for (body, expected) in [
        (
            "display-message -p before ; resize-pane -x bad ; display-message -p skipped",
            "before\nouter-after\n",
        ),
        (
            "display-message -p before ; display-message -Q",
            "outer-after\n",
        ),
    ] {
        let output = harness.run(&[
            "run-shell",
            "-C",
            body,
            ";",
            "display-message",
            "-p",
            "outer-after",
        ])?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(stdout(&output), expected);
        assert!(!stderr(&output).is_empty());
    }
    Ok(())
}
