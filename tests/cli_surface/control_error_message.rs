//! Control-mode failed-command text, against tmux 3.7b.
//!
//! Oracle measured 2026-11-14 on Linux with `tmux -f /dev/null -S <private>`:
//! a command that fails inside a control client prints tmux's own message
//! between `%begin` and `%error`, for the command-line command and for
//! commands read from stdin.
use super::*;

/// Keeps the command frames and the exit record; asynchronous notifications
/// such as the pane's `%output` may interleave anywhere.
fn command_records(rendered: &str) -> Vec<&str> {
    rendered
        .lines()
        .filter(|record| {
            !record.starts_with('%')
                || ["%begin ", "%end ", "%error ", "%session-changed "]
                    .iter()
                    .any(|prefix| record.starts_with(prefix))
                || *record == "%exit"
        })
        .collect()
}

fn guard_fields(record: &str) -> Vec<&str> {
    record.split_whitespace().collect()
}

fn assert_failed_frame(records: &[&str], offset: usize, message: &str, flags: &str) {
    let begin = guard_fields(records[offset]);
    let error = guard_fields(records[offset + 2]);
    assert_eq!(begin.first(), Some(&"%begin"), "{records:?}");
    assert_eq!(records[offset + 1], message, "{records:?}");
    assert_eq!(error.first(), Some(&"%error"), "{records:?}");
    assert_eq!(begin.get(1..), error.get(1..), "{records:?}");
    assert_eq!(begin.last(), Some(&flags), "{records:?}");
}

fn run_control_with_stdin(
    harness: &CliHarness,
    args: &[&str],
    input: &[u8],
) -> Result<Output, Box<dyn Error>> {
    let mut child = harness
        .base_command()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(input)?;
    drop(stdin);
    Ok(child.wait_with_output()?)
}

#[test]
fn control_attach_to_missing_session_prints_tmux_message() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-attach-missing-message")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let failed = harness.run(&["-C", "attach-session", "-t", "missing"])?;

    let rendered = stdout(&failed);
    assert!(stderr(&failed).is_empty(), "stderr={:?}", stderr(&failed));
    let records = command_records(&rendered);
    assert_eq!(records.len(), 4, "{rendered:?}");
    assert_failed_frame(&records, 0, "can't find session: missing", "0");
    assert_eq!(records[3], "%exit", "{rendered:?}");
    Ok(())
}

#[test]
fn control_stdin_command_error_prints_tmux_message() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-stdin-error-message")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = run_control_with_stdin(
        &harness,
        &["-C", "attach-session", "-t", "alpha"],
        b"has-session -t nope\ndisplay-message -p ok\n",
    )?;

    let rendered = stdout(&output);
    assert!(stderr(&output).is_empty(), "stderr={:?}", stderr(&output));
    let records = command_records(&rendered);
    assert_eq!(records.len(), 10, "{rendered:?}");
    assert_failed_frame(&records, 3, "can't find session: nope", "1");
    assert_eq!(records[7], "ok", "{rendered:?}");
    assert_eq!(records[9], "%exit", "{rendered:?}");
    Ok(())
}

#[test]
fn control_stdin_argument_errors_are_tmux_parse_errors() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b, measured 2026-11-14 on macOS with a private socket: an
    // argument error rejects the whole line as "parse error: <cause>" before
    // any command of it runs, brace blocks included; the client exits 0.
    let harness = CliHarness::new("control-stdin-argument-parse-error")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = run_control_with_stdin(
        &harness,
        &["-C", "attach-session", "-t", "alpha"],
        b"display-message -p FIRST ; display-message -p -:\n\
          if-shell -F 1 { display-message -Q }\n\
          wait-for\n\
          display-message -p ok\n",
    )?;

    let rendered = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "{rendered:?}");
    assert!(stderr(&output).is_empty(), "stderr={:?}", stderr(&output));
    let records = command_records(&rendered);
    assert_eq!(records.len(), 16, "{rendered:?}");
    assert_failed_frame(
        &records,
        3,
        "parse error: command display-message: invalid flag -:",
        "1",
    );
    assert_failed_frame(
        &records,
        6,
        "parse error: command display-message: unknown flag -Q",
        "1",
    );
    assert_failed_frame(
        &records,
        9,
        "parse error: command wait-for: too few arguments (need at least 1)",
        "1",
    );
    assert_eq!(records[13], "ok", "{rendered:?}");
    assert!(!rendered.contains("FIRST"), "{rendered:?}");
    assert_eq!(records[15], "%exit", "{rendered:?}");
    Ok(())
}

#[test]
fn wait_for_empty_channel_is_an_ordinary_channel() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b accepts '' as a channel name for -S, -L, -U and a plain wait.
    let harness = CliHarness::new("wait-for-empty-channel")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    assert_success(&harness.run(&["wait-for", "-L", ""])?);
    assert_success(&harness.run(&["wait-for", "-U", ""])?);
    assert_success(&harness.run(&["wait-for", "-S", ""])?);
    // The signal above found no waiter, so this wait consumes it at once.
    assert_success(&harness.run(&["wait-for", ""])?);
    let unlocked = harness.run(&["wait-for", "-U", ""])?;
    assert_eq!(unlocked.status.code(), Some(1), "{unlocked:?}");
    assert_eq!(stderr(&unlocked), "channel  not locked\n");
    Ok(())
}
