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

/// Replaces the time and command number of `%begin`, `%end` and `%error`
/// guard lines with `T N`, after checking that each block closes with the
/// numbers it opened with, as tmux does. Also hides the session creation date.
/// A -CC stream keeps its DCS envelope byte for byte.
#[cfg(unix)]
fn normalized_control_stream(stdout: &[u8]) -> Result<String, Box<dyn Error>> {
    const CONTROL_CONTROL_START: &str = "\x1bP1000p";
    let stdout = std::str::from_utf8(stdout)?;
    let (envelope, stream) = stdout
        .strip_prefix(CONTROL_CONTROL_START)
        .map_or(("", stdout), |stream| (CONTROL_CONTROL_START, stream));
    let mut open_block = None;
    let mut normalized = String::with_capacity(stdout.len());
    normalized.push_str(envelope);
    for line in stream.split_inclusive('\n') {
        let guard = ["%begin ", "%end ", "%error "]
            .into_iter()
            .find(|prefix| line.starts_with(prefix));
        let Some(guard) = guard else {
            match (line.find("(created "), line.rfind(')')) {
                (Some(start), Some(end)) if start < end => {
                    normalized.push_str(&line[..start]);
                    normalized.push_str("(created X");
                    normalized.push_str(&line[end..]);
                }
                _ => normalized.push_str(line),
            }
            continue;
        };
        let fields = line[guard.len()..]
            .trim_end()
            .split(' ')
            .collect::<Vec<_>>();
        let [time, number, flags] = fields[..] else {
            return Err(format!("malformed guard line {line:?}").into());
        };
        if guard == "%begin " {
            open_block = Some((time.to_owned(), number.to_owned()));
        } else if open_block.take() != Some((time.to_owned(), number.to_owned())) {
            return Err(format!("{line:?} does not close the open block in {stdout:?}").into());
        }
        normalized.push_str(&format!("{guard}T N {flags}\n"));
    }
    Ok(normalized)
}

#[cfg(unix)]
#[test]
fn control_mode_option_terminator_ends_flags_like_tmux() -> Result<(), Box<dyn Error>> {
    // Bytes measured on tmux 3.7b (Linux and Darwin), private socket, a
    // session "alpha" and a buffer "x": a final `--` ends the options and
    // the command runs, under -C and -CC, alone or in a ; queue, through
    // the list-* fast paths and the commands that take no positional.
    let harness = CrossPlatformHarness::new("control-option-terminator")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success(["set-buffer", "x"])?;

    let empty_block = "%begin T N 0\n%end T N 0\n%exit\n";
    for (arguments, expected) in [
        (
            &["-C", "list-sessions", "--"][..],
            "%begin T N 0\nalpha: 1 windows (created X)\n%end T N 0\n%exit\n",
        ),
        (
            &["-CC", "list-sessions", "--"][..],
            "\x1bP1000p%begin T N 0\nalpha: 1 windows (created X)\n%end T N 0\n%exit\n\x1b\\",
        ),
        (
            &["-C", "list-sessions", "-F", "#{session_name}", "--"][..],
            "%begin T N 0\nalpha\n%end T N 0\n%exit\n",
        ),
        (
            &["-C", "list-sessions", "-rF", "#{session_name}", "--"][..],
            "%begin T N 0\nalpha\n%end T N 0\n%exit\n",
        ),
        (
            &["-C", "list-sessions", "-F", "--", "--"][..],
            "%begin T N 0\n--\n%end T N 0\n%exit\n",
        ),
        (
            &[
                "-C",
                "list-windows",
                "-a",
                "-F",
                "#{session_name}:#{window_index}",
                "--",
            ][..],
            "%begin T N 0\nalpha:0\n%end T N 0\n%exit\n",
        ),
        (
            &[
                "-C",
                "list-panes",
                "-a",
                "-F",
                "#{session_name}:#{pane_index}",
                "--",
            ][..],
            "%begin T N 0\nalpha:0\n%end T N 0\n%exit\n",
        ),
        (
            &[
                "-C",
                "list-panes",
                "-s",
                "-t",
                "alpha",
                "-F",
                "#{pane_index}",
                "--",
            ][..],
            "%begin T N 0\n0\n%end T N 0\n%exit\n",
        ),
        (
            &["-C", "list-buffers", "-F", "#{buffer_sample}", "--"][..],
            "%begin T N 0\nx\n%end T N 0\n%exit\n",
        ),
        (
            &["-C", "show-buffer", "--"][..],
            "%begin T N 0\nx\n%end T N 0\n%exit\n",
        ),
        (&["-C", "has-session", "-t", "alpha", "--"][..], empty_block),
        (
            &["-C", "clear-history", "-t", "alpha", "--"][..],
            empty_block,
        ),
        (&["-C", "start-server", "--"][..], empty_block),
        (
            &[
                "-C",
                "list-sessions",
                "-F",
                "#{session_name}",
                "--",
                ";",
                "display-message",
                "-p",
                "after",
            ][..],
            "%begin T N 0\nalpha\n%end T N 0\n%begin T N 0\nafter\n%end T N 0\n%exit\n",
        ),
        (
            &["-C", "list-sessions", "--", "x"][..],
            "command list-sessions: too many arguments (need at most 0)\n%exit\n",
        ),
    ] {
        let output = harness.run(arguments)?;
        let code = if expected.contains("%begin") { 0 } else { 1 };
        assert_eq!(
            output.status.code(),
            Some(code),
            "{arguments:?}: {output:?}"
        );
        assert_eq!(
            normalized_control_stream(&output.stdout)?,
            expected,
            "{arguments:?}"
        );
        assert!(output.stderr.is_empty(), "{arguments:?}: {output:?}");
    }

    let mut attached = harness
        .command(["-C", "attach-session", "-t", "alpha"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write as _;
        let mut stdin = attached.stdin.take().ok_or("control client stdin")?;
        stdin.write_all(b"list-sessions -F '#{session_name}' --\n")?;
    }
    let attached = attached.wait_with_output()?;
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");
    assert_eq!(
        normalized_control_stream(&attached.stdout)?,
        "%begin T N 0\n%end T N 0\n%session-changed $0 alpha\n\
         %begin T N 1\nalpha\n%end T N 1\n%exit\n"
    );
    assert!(attached.stderr.is_empty(), "{attached:?}");

    let killed = harness.run(["-C", "kill-server", "--"])?;
    assert_eq!(killed.status.code(), Some(0), "{killed:?}");
    assert_eq!(
        normalized_control_stream(&killed.stdout)?,
        "%begin T N 0\n%end T N 0\n%sessions-changed\n%exit\n"
    );
    assert!(killed.stderr.is_empty(), "{killed:?}");
    Ok(())
}

#[test]
fn source_file_option_terminator_ends_flags_like_tmux() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b runs both commands and prints "alpha\nafter\n" with rc 0.
    let harness = CrossPlatformHarness::new("source-option-terminator")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    let source = harness.tmpdir().join("option-terminator.conf");
    let source_path = source.to_str().ok_or("non-UTF-8 source path")?;
    fs::write(
        &source,
        "list-sessions -F '#{session_name}' -- ; display-message -p after\n",
    )?;

    let output = harness.run(["source-file", source_path])?;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"alpha\nafter\n", "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    Ok(())
}
