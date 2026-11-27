#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, AttachedSession, CliHarness, DaemonGuard};
use rmux_pty::TerminalSize;

const FORMAT: &str = "#{session_name}:#{pane_id}";

/// The `-d` delay every delayed-command fixture queues.
const DELAYED_COMMAND_DELAY: Duration = Duration::from_millis(500);

/// How many times a replacement fixture may re-run after losing the race to
/// mutate its target before the queued command fires.
const REPLACEMENT_ATTEMPTS: usize = 5;

#[test]
fn detached_cli_run_shell_uses_the_canonical_current_target() -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("run-shell-detached-current")?;
    let expected = display_format(&harness, None)?;
    assert!(
        expected.starts_with("beta:%"),
        "the detached-current fixture should select beta, got {expected:?}"
    );

    let if_shell = harness.run(&[
        "if-shell",
        "-F",
        "1",
        &format!("display-message -p '{FORMAT}'"),
    ])?;
    assert_output_success(&if_shell);
    assert_eq!(stdout(&if_shell), format!("{expected}\n"));
    assert!(stderr(&if_shell).is_empty());

    let run_shell = harness.run(&["run-shell", &format_print_command()])?;
    assert_output_success(&run_shell);
    assert_eq!(stdout(&run_shell), format!("{expected}\n"));
    assert!(stderr(&run_shell).is_empty());

    let explicit_expected = display_format(&harness, Some("alpha:0.0"))?;
    let explicit_output = harness.tmpdir().join("explicit-context.txt");
    let explicit = harness.run(&[
        "run-shell",
        "-t",
        "alpha:0.0",
        &format_write_command(&explicit_output),
    ])?;
    assert_success(&explicit);
    wait_for_file_text(&explicit_output, &format!("{explicit_expected}\n"))?;
    Ok(())
}

#[test]
fn sourced_bound_and_hook_run_shell_keep_their_entry_target() -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("run-shell-entry-targets")?;
    let expected = display_format(&harness, None)?;

    let source_file = harness.tmpdir().join("run-shell-context.conf");
    fs::write(
        &source_file,
        format!("run-shell {}\n", shell_quote_str(&format_print_command())),
    )?;
    let sourced = harness.run(&[
        "source-file",
        source_file.to_str().expect("utf-8 source path"),
    ])?;
    assert_output_success(&sourced);
    assert_eq!(stdout(&sourced), format!("{expected}\n"));
    assert!(stderr(&sourced).is_empty());

    let binding_output = harness.tmpdir().join("binding-context.txt");
    let binding_command = format_write_command(&binding_output);
    assert_success(&harness.run(&[
        "bind-key",
        "-T",
        "prefix",
        "X",
        "run-shell",
        &binding_command,
    ])?);
    let mut attach = AttachedSession::spawn(&harness, "beta", TerminalSize::new(80, 12))?;
    attach.wait_for_raw_mode(Duration::from_secs(5))?;
    attach.send_bytes(b"\x02X")?;
    wait_for_file_text(&binding_output, &format!("{expected}\n"))?;
    attach.send_bytes(b"\x02d")?;
    assert!(attach.wait_for_exit(Duration::from_secs(5))?.success());

    let hook_output = harness.tmpdir().join("hook-context.txt");
    let hook_command = format!(
        "run-shell {}",
        shell_quote_str(&format_write_command(&hook_output))
    );
    assert_success(&harness.run(&["set-hook", "-g", "after-new-window", &hook_command])?);
    let created = harness.run(&["new-window", "-dP", "-F", FORMAT, "-t", "beta", "sleep 30"])?;
    assert_output_success(&created);
    let created_target = stdout(&created);
    wait_for_file_text(&hook_output, &created_target)?;
    Ok(())
}

#[test]
fn delayed_run_shell_keeps_the_dispatch_context_after_session_rename() -> Result<(), Box<dyn Error>>
{
    let (harness, _daemon) = current_target_harness("run-shell-context-rename")?;
    let expected = display_format(&harness, None)?;
    let mut run_shell = spawn_delayed_run_shell(&harness)?;
    wait_until_delayed(&mut run_shell)?;

    assert_success(&harness.run(&["rename-session", "-t", "beta", "renamed"])?);
    let output = run_shell.wait_with_output()?;

    assert_output_success(&output);
    assert_eq!(stdout(&output), format!("{expected}\n"));
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn delayed_run_shell_does_not_rebind_to_a_replacement_session() -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("run-shell-context-replacement")?;
    let expected = display_format(&harness, None)?;
    let mut run_shell = spawn_delayed_run_shell(&harness)?;
    wait_until_delayed(&mut run_shell)?;

    assert_success(&harness.run(&["kill-session", "-t", "beta"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);
    let replacement = display_format(&harness, Some("beta:0.0"))?;
    assert_ne!(
        replacement, expected,
        "the replacement must have a distinct stable pane identity"
    );

    let output = run_shell.wait_with_output()?;
    assert_output_success(&output);
    assert_eq!(stdout(&output), format!("{expected}\n"));
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn delayed_command_run_shell_follows_stable_session_identity_after_rename(
) -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("run-shell-command-context-rename")?;
    let mut run_shell = spawn_delayed_command_run_shell(&harness, "new-window -d -n delayed")?;
    wait_until_delayed(&mut run_shell)?;

    assert_success(&harness.run(&["rename-session", "-t", "beta", "renamed"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "gamma", "sleep 30"])?);
    let output = run_shell.wait_with_output()?;

    assert_output_success(&output);
    assert_eq!(window_names(&harness, "alpha")?, vec!["sleep"]);
    assert_eq!(window_names(&harness, "gamma")?, vec!["sleep"]);
    assert_eq!(window_names(&harness, "renamed")?, vec!["sleep", "delayed"]);
    Ok(())
}

#[test]
fn command_run_shell_continues_after_renaming_its_pinned_session() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b oracle, measured 2026-07-27: the rename and the following
    // admitted mutation both run against the same session lifetime.
    let (harness, _daemon) = current_target_harness("run-shell-command-queued-rename")?;
    let output = harness.run(&[
        "run-shell",
        "-C",
        "-t",
        "beta:0.0",
        "rename-session -t beta renamed ; new-window -d -n queued-after-rename sleep 30",
    ])?;

    let old_target = harness.run(&["has-session", "-t", "beta"])?;
    assert_eq!(
        old_target.status.code(),
        Some(1),
        "the first admitted command must rename beta: {old_target:?}"
    );
    assert_success(&harness.run(&["has-session", "-t", "renamed"])?);
    assert_eq!(
        window_names(&harness, "renamed")?,
        vec!["sleep", "queued-after-rename"],
        "the renamed session exists, but the second admitted mutation was not dispatched: \
         stdout={:?} stderr={:?}",
        stdout(&output),
        stderr(&output)
    );
    assert_output_success(&output);
    Ok(())
}

#[test]
fn command_run_shell_keeps_the_renamed_identity_when_the_old_name_is_reused(
) -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("run-shell-command-rename-reuse")?;
    let output = harness.run(&[
        "run-shell",
        "-C",
        "-t",
        "beta:0.0",
        "rename-session -t beta renamed ; \
         new-session -d -s beta sleep 30 ; \
         new-window -d -n queued-on-renamed sleep 30",
    ])?;

    assert_output_success(&output);
    assert_eq!(window_names(&harness, "beta")?, vec!["sleep"]);
    assert_eq!(
        window_names(&harness, "renamed")?,
        vec!["sleep", "queued-on-renamed"]
    );
    Ok(())
}

#[test]
fn delayed_command_run_shell_rejects_replacement_session_product_divergence(
) -> Result<(), Box<dyn Error>> {
    // tmux 3.7b oracle, measured 2026-07-26: after the original target is
    // killed, the delayed command resolves a replacement with the same name
    // and creates the requested window there. RMUX pins the stable session
    // identity and fails closed instead of mutating the replacement.
    for _ in 0..REPLACEMENT_ATTEMPTS {
        let (harness, _daemon) = current_target_harness("run-shell-command-context-replacement")?;
        let queued = Instant::now();
        let mut run_shell = spawn_delayed_command_run_shell(&harness, "new-window -d -n rebound")?;
        wait_until_delayed(&mut run_shell)?;

        assert_success(&harness.run(&["kill-session", "-t", "beta"])?);
        assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);
        // `-d` is wall clock, and it cannot start before the spawn: replacing
        // the target within it proves the command was still queued. Past it
        // the run is undecided, because a command that already ran mutated the
        // original target and exits 0 whatever the pinning rule does.
        let still_queued = queued.elapsed() < DELAYED_COMMAND_DELAY;
        let output = run_shell.wait_with_output()?;
        if !still_queued && output.status.code() == Some(0) {
            continue;
        }

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stdout(&output).is_empty());
        assert_eq!(
            stderr(&output),
            "queued pinned target was replaced before execution\n"
        );
        assert_eq!(window_names(&harness, "alpha")?, vec!["sleep"]);
        assert_eq!(window_names(&harness, "beta")?, vec!["sleep"]);
        return Ok(());
    }
    Err(format!(
        "the replacement never landed inside the {DELAYED_COMMAND_DELAY:?} delay in \
         {REPLACEMENT_ATTEMPTS} attempts"
    )
    .into())
}

fn current_target_harness(label: &str) -> Result<(CliHarness, DaemonGuard), Box<dyn Error>> {
    let harness = CliHarness::new(label)?;
    let daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);
    Ok((harness, daemon))
}

fn display_format(harness: &CliHarness, target: Option<&str>) -> Result<String, Box<dyn Error>> {
    let mut arguments = vec!["display-message", "-p"];
    if let Some(target) = target {
        arguments.extend(["-t", target]);
    }
    arguments.push(FORMAT);
    let output = harness.run(&arguments)?;
    assert_output_success(&output);
    assert!(stderr(&output).is_empty());
    Ok(stdout(&output).trim_end().to_owned())
}

fn spawn_delayed_run_shell(harness: &CliHarness) -> Result<Child, Box<dyn Error>> {
    let mut command = harness.base_command();
    command
        .args(["run-shell", "-d", "0.5", &format_print_command()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command.spawn()?)
}

fn spawn_delayed_command_run_shell(
    harness: &CliHarness,
    nested_command: &str,
) -> Result<Child, Box<dyn Error>> {
    let delay = format!("{}", DELAYED_COMMAND_DELAY.as_secs_f64());
    let mut command = harness.base_command();
    command
        .args(["run-shell", "-C", "-d", &delay, nested_command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command.spawn()?)
}

fn window_names(harness: &CliHarness, session: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let output = harness.run(&["list-windows", "-t", session, "-F", "#{window_name}"])?;
    assert_output_success(&output);
    Ok(stdout(&output).lines().map(ToOwned::to_owned).collect())
}

fn wait_until_delayed(child: &mut Child) -> Result<(), Box<dyn Error>> {
    std::thread::sleep(Duration::from_millis(100));
    if let Some(status) = child.try_wait()? {
        return Err(format!("delayed run-shell exited before mutation: {status}").into());
    }
    Ok(())
}

fn assert_output_success(output: &std::process::Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "command failed: stdout={:?} stderr={:?}",
        stdout(output),
        stderr(output)
    );
    assert!(stderr(output).is_empty(), "stderr={:?}", stderr(output));
}

fn wait_for_file_text(path: &std::path::Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(contents) => last = Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!(
        "timed out waiting for {} to contain {expected:?}; last={last:?}",
        path.display()
    )
    .into())
}

fn format_print_command() -> String {
    format!("printf '%s\\n' '{FORMAT}'")
}

fn format_write_command(path: &std::path::Path) -> String {
    format!("{} > {}", format_print_command(), shell_quote(path))
}

fn shell_quote(path: &std::path::Path) -> String {
    shell_quote_str(&path.display().to_string())
}

fn shell_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[test]
fn callback_error_and_identical_shell_text_keep_distinct_control_status(
) -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("run-shell-callback-status")?;
    for mode in ["-C", "-CC"] {
        for (body, callback, expected) in [
            ("not-a-command", true, 1),
            ("printf 'unknown command: not-a-command\\n'", false, 0),
            (
                "printf 'unknown command: not-a-command\\n'; exit 7",
                false,
                7,
            ),
        ] {
            for delay in [None, Some("0"), Some("0.001")] {
                let mut args = vec![mode, "run-shell"];
                if callback {
                    args.push("-C");
                }
                if let Some(delay) = delay {
                    args.extend(["-d", delay]);
                }
                args.push(body);
                let output = harness.run(&args)?;
                assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
                let text = stdout(&output);
                assert!(text.contains("%end "), "{text}");
                assert!(!text.contains("%error "), "{text}");
                assert!(text.find("%end ") < text.find("unknown command:"), "{text}");
                assert!(stderr(&output).is_empty(), "{output:?}");
            }
        }
    }
    let output = harness.run(&[
        "display-message",
        "-p",
        "BEFORE",
        ";",
        "run-shell",
        "-C",
        "not-a-command",
        ";",
        "display-message",
        "-p",
        "AFTER",
    ])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "BEFORE\nAFTER\n");
    assert_eq!(stderr(&output), "unknown command: not-a-command\n");
    let file = harness.tmpdir().join("callback.conf");
    fs::write(
        &file,
        "run-shell -C not-a-command\ndisplay-message -p AFTER\n",
    )?;
    for mode in [vec![], vec!["-C"]] {
        let mut args = mode.clone();
        args.extend(["source-file", file.to_str().unwrap()]);
        let output = harness.run(&args)?;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let diagnostic = format!("{}:1: unknown command: not-a-command", file.display());
        if mode.is_empty() {
            assert_eq!(stdout(&output), "AFTER\n");
            assert_eq!(stderr(&output), format!("{diagnostic}\n"));
        } else {
            let text = stdout(&output);
            assert!(text.contains(&diagnostic), "{text}");
            assert!(!text.contains("%error "), "{text}");
            assert!(text.contains("AFTER\n"), "{text}");
        }
    }
    Ok(())
}

#[test]
fn sourced_callback_errors_retain_file_and_line() -> Result<(), Box<dyn Error>> {
    let (harness, _daemon) = current_target_harness("source-callback-location")?;
    let file = harness.tmpdir().join("callback.conf");
    for delay in ["", "-d 0", "-d 0.001"] {
        let body = format!("run-shell -C {delay} not-a-command");
        let output = harness.run(&["if-shell", "-F", "1", &body])?;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(stdout(&output), "");
        assert_eq!(stderr(&output), "unknown command: not-a-command\n");
        fs::write(
            &file,
            format!(
                "# line one\n\nrun-shell -C {delay} not-a-command\n\
                 display-message -p AFTER\n\
                 run-shell -C {delay} another-missing-command\n"
            ),
        )?;
        for mode in [vec![], vec!["-C"], vec!["-CC"]] {
            let mut args = mode.clone();
            args.extend(["source-file", file.to_str().unwrap()]);
            let output = harness.run(&args)?;
            let first = format!("{}:3: unknown command: not-a-command\n", file.display());
            let second = format!(
                "{}:5: unknown command: another-missing-command\n",
                file.display()
            );
            assert_eq!(
                output.status.code(),
                Some(1),
                "{delay} {mode:?}: {output:?}"
            );
            if mode.is_empty() {
                assert_eq!(stdout(&output), "AFTER\n");
                assert_eq!(stderr(&output), format!("{first}{second}"), "{delay}");
            } else {
                let text = stdout(&output);
                assert!(text.contains(&first) && text.contains(&second), "{text}");
                assert!(!text.contains("%error "), "{text}");
                assert!(stderr(&output).is_empty());
            }
        }
    }
    Ok(())
}
