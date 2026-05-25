#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use common::{assert_success, stderr, stdout, CliHarness};

const COMMAND_SURFACE: [&str; 91] = [
    "new-session",
    "start-server",
    "kill-server",
    "has-session",
    "kill-session",
    "server-access",
    "lock-server",
    "lock-session",
    "lock-client",
    "new-window",
    "kill-window",
    "select-window",
    "rename-window",
    "next-window",
    "previous-window",
    "last-window",
    "list-windows",
    "move-window",
    "swap-window",
    "rotate-window",
    "resize-window",
    "respawn-window",
    "split-window",
    "swap-pane",
    "last-pane",
    "join-pane",
    "move-pane",
    "break-pane",
    "pipe-pane",
    "respawn-pane",
    "kill-pane",
    "select-layout",
    "next-layout",
    "previous-layout",
    "resize-pane",
    "display-panes",
    "select-pane",
    "find-window",
    "link-window",
    "unlink-window",
    "choose-tree",
    "choose-buffer",
    "choose-client",
    "customize-mode",
    "clock-mode",
    "send-keys",
    "bind-key",
    "unbind-key",
    "list-commands",
    "list-keys",
    "send-prefix",
    "command-prompt",
    "confirm-before",
    "attach-session",
    "refresh-client",
    "list-clients",
    "switch-client",
    "detach-client",
    "suspend-client",
    "set-option",
    "set-window-option",
    "set-environment",
    "show-options",
    "show-window-options",
    "show-environment",
    "show-hooks",
    "source-file",
    "set-hook",
    "set-buffer",
    "show-buffer",
    "paste-buffer",
    "list-buffers",
    "delete-buffer",
    "load-buffer",
    "save-buffer",
    "capture-pane",
    "clear-history",
    "copy-mode",
    "display-message",
    "show-messages",
    "run-shell",
    "if-shell",
    "wait-for",
    "web-share",
    "rename-session",
    "list-sessions",
    "list-panes",
    "display-menu",
    "display-popup",
    "clear-prompt-history",
    "show-prompt-history",
];
const ATTACH_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_FOR_BLOCK_TIMEOUT: Duration = Duration::from_millis(150);

fn assert_missing_has_session(output: &std::process::Output, session_name: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(output).is_empty());
    assert_eq!(
        stderr(output),
        format!("can't find session: {session_name}\n")
    );
}

#[test]
fn cli_command_surface_matches_public_help_enum_and_dispatch() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected = expected_command_surface();
    let harness = CliHarness::new("request-cli-command-surface")?;
    let list_output = harness.run(&["list-commands"])?;
    let listed_commands = extract_list_commands(&stdout(&list_output));
    let enum_commands = extract_enum_variant_commands(
        &repo_root.join("src/cli_args.rs"),
        "pub(crate) enum Command",
    )?;
    let dispatch_commands = extract_match_variant_commands(
        &repo_root.join("src/cli/dispatch.rs"),
        "match command",
        "Command::",
    )?;

    assert_eq!(expected.len(), COMMAND_SURFACE.len());
    assert_eq!(list_output.status.code(), Some(0));
    assert_eq!(listed_commands, expected);
    assert_eq!(enum_commands, expected);
    assert_eq!(dispatch_commands, expected);
    Ok(())
}

#[test]
fn buffer_capture_and_scripting_commands_round_trip_end_to_end() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("request-buffers-script")?;
    let _daemon = harness.start_hidden_daemon()?;
    let save_path = harness.tmpdir().join("saved-buffer.txt");
    let load_path = harness.tmpdir().join("loaded-buffer.txt");
    let save_path_arg = save_path.to_string_lossy().into_owned();
    let load_path_arg = load_path.to_string_lossy().into_owned();
    let paste_buffer_name = "pastecmd";
    let paste_marker = "cli_request_pasted_marker";
    let paste_command = format!("printf {paste_marker}");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    assert_success(&harness.run(&["set-buffer", "-b", "empty", ""])?);
    let empty = harness.run(&["show-buffer", "-b", "empty"])?;
    assert_eq!(empty.status.code(), Some(1));
    assert!(stdout(&empty).is_empty());

    assert_success(&harness.run(&["set-buffer", "-b", "delete-me", "x"])?);

    assert_success(&harness.run(&[
        "set-buffer",
        "-b",
        paste_buffer_name,
        paste_command.as_str(),
    ])?);
    let show_named = harness.run(&["show-buffer", "-b", paste_buffer_name])?;
    assert_eq!(show_named.status.code(), Some(0));
    assert_eq!(stdout(&show_named), paste_command);
    assert!(stderr(&show_named).is_empty());

    let listed = harness.run(&["list-buffers"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert!(!stdout(&listed).contains("empty:"));
    assert!(stdout(&listed).contains("delete-me:"));
    assert!(stdout(&listed).contains("pastecmd:"));
    assert!(stderr(&listed).is_empty());

    assert_success(&harness.run(&[
        "save-buffer",
        "-b",
        paste_buffer_name,
        save_path_arg.as_str(),
    ])?);
    assert_eq!(fs::read_to_string(&save_path)?, paste_command);

    fs::write(&load_path, "loaded-from-file")?;
    assert_success(&harness.run(&["load-buffer", "-b", "loaded", load_path_arg.as_str()])?);
    let loaded = harness.run(&["show-buffer", "-b", "loaded"])?;
    assert_eq!(loaded.status.code(), Some(0));
    assert_eq!(stdout(&loaded), "loaded-from-file");
    assert!(stderr(&loaded).is_empty());

    assert_success(&harness.run(&["paste-buffer", "-b", paste_buffer_name, "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["send-keys", "-t", "alpha:0.0", "Enter"])?);
    let capture = wait_for_capture(&harness, paste_marker)?;
    assert!(stdout(&capture).contains(paste_marker));
    assert!(stderr(&capture).is_empty());

    assert_success(&harness.run(&["capture-pane", "-t", "alpha:0.0", "-b", "captured"])?);
    let captured = harness.run(&["show-buffer", "-b", "captured"])?;
    assert_eq!(captured.status.code(), Some(0));
    assert!(
        stdout(&captured).contains(paste_marker),
        "capture-pane should persist transcript output into a buffer"
    );
    assert!(stderr(&captured).is_empty());

    assert_success(&harness.run(&["delete-buffer", "-b", "delete-me"])?);
    let listed_after_delete = harness.run(&["list-buffers"])?;
    assert_eq!(listed_after_delete.status.code(), Some(0));
    assert!(!stdout(&listed_after_delete).contains("delete-me:"));
    assert!(stderr(&listed_after_delete).is_empty());

    let display = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{session_name}:#{pane_index}:#{missing}",
    ])?;
    assert_eq!(display.status.code(), Some(0));
    assert_eq!(stdout(&display), "alpha:0:\n");
    assert!(stderr(&display).is_empty());

    let shell = harness.run(&["run-shell", "printf cli-run-shell-output"])?;
    assert_eq!(shell.status.code(), Some(0));
    assert_eq!(stdout(&shell), "cli-run-shell-output");
    assert!(stderr(&shell).is_empty());

    assert_success(&harness.run(&[
        "if-shell",
        "-F",
        "-t",
        "alpha:0.0",
        "#{pane_active}",
        "set-buffer -b branch selected",
        "set-buffer -b branch skipped",
    ])?);
    let branch = harness.run(&["show-buffer", "-b", "branch"])?;
    assert_eq!(branch.status.code(), Some(0));
    assert_eq!(stdout(&branch), "selected");
    assert!(stderr(&branch).is_empty());

    Ok(())
}

#[test]
fn issue_8_split_and_new_window_pane_output_round_trip() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("request-issue-8-pane-output")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "120", "-y", "40"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf ISSUE8_FIRST",
        "Enter",
    ])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        "printf ISSUE8_SECOND",
        "Enter",
    ])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:1.0",
        "printf ISSUE8_WINDOW",
        "Enter",
    ])?);

    assert!(stdout(&wait_for_capture_target(
        &harness,
        "alpha:0.0",
        "ISSUE8_FIRST"
    )?)
    .contains("ISSUE8_FIRST"));
    assert!(stdout(&wait_for_capture_target(
        &harness,
        "alpha:0.1",
        "ISSUE8_SECOND"
    )?)
    .contains("ISSUE8_SECOND"));
    assert!(stdout(&wait_for_capture_target(
        &harness,
        "alpha:1.0",
        "ISSUE8_WINDOW"
    )?)
    .contains("ISSUE8_WINDOW"));

    Ok(())
}

#[test]
fn rename_listing_and_wait_for_commands_round_trip_end_to_end() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("request-rename-list-wait")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "120", "-y", "40"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let panes_before_rename = harness.run(&[
        "list-panes",
        "-a",
        "-t",
        "alpha",
        "-F",
        "#{session_name}:#{window_index}:#{pane_index}",
    ])?;
    assert_eq!(panes_before_rename.status.code(), Some(0));
    let panes_before_stdout = stdout(&panes_before_rename);
    let pane_lines = nonempty_lines(&panes_before_stdout);
    assert_eq!(pane_lines.len(), 3);
    assert!(pane_lines.iter().all(|line| line.starts_with("alpha:")));
    assert!(stderr(&panes_before_rename).is_empty());

    assert_success(&harness.run(&["rename-session", "-t", "alpha", "gamma"])?);

    let missing_old = harness.run(&["has-session", "-t", "alpha"])?;
    assert_missing_has_session(&missing_old, "alpha");
    assert_success(&harness.run(&["has-session", "-t", "gamma"])?);

    let sessions = harness.run(&[
        "list-sessions",
        "-F",
        "#{session_name}:#{session_windows}:#{session_width}x#{session_height}",
    ])?;
    assert_eq!(sessions.status.code(), Some(0));
    assert_eq!(stdout(&sessions), "gamma:2:x\n");
    assert!(stderr(&sessions).is_empty());

    let panes_after_rename = harness.run(&[
        "list-panes",
        "-a",
        "-t",
        "gamma",
        "-F",
        "#{session_name}:#{window_index}:#{pane_index}",
    ])?;
    assert_eq!(panes_after_rename.status.code(), Some(0));
    let panes_after_stdout = stdout(&panes_after_rename);
    let renamed_lines = nonempty_lines(&panes_after_stdout);
    assert_eq!(renamed_lines.len(), 3);
    assert!(renamed_lines.iter().all(|line| line.starts_with("gamma:")));
    assert!(renamed_lines.contains(&"gamma:1:0"));
    assert!(stderr(&panes_after_rename).is_empty());

    let mut signal_waiter = spawn_cli_process(&harness, &["wait-for", "ready"])?;
    assert_process_blocks(&mut signal_waiter, "plain wait-for")?;
    assert_success(&harness.run(&["wait-for", "-S", "ready"])?);
    assert_eq!(signal_waiter.wait()?.code(), Some(0));

    assert_success(&harness.run(&["wait-for", "-L", "check"])?);
    let mut lock_waiter = spawn_cli_process(&harness, &["wait-for", "-L", "check"])?;
    assert_process_blocks(&mut lock_waiter, "locked wait-for -L")?;
    assert_success(&harness.run(&["wait-for", "-U", "check"])?);
    assert_eq!(lock_waiter.wait()?.code(), Some(0));

    Ok(())
}

fn expected_command_surface() -> BTreeSet<String> {
    COMMAND_SURFACE
        .iter()
        .map(|command| (*command).to_owned())
        .collect()
}

fn extract_enum_variant_commands(
    path: &Path,
    anchor: &str,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let block = extract_braced_block(&contents, anchor)?;
    let mut commands = BTreeSet::new();

    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('/') {
            continue;
        }

        let variant = extract_variant_name(trimmed);
        if let Some(variant) = variant {
            if variant == "Unsupported" {
                continue;
            }
            let command = public_command_name(&variant);
            commands.insert(command);
        }
    }

    Ok(commands)
}

fn extract_match_variant_commands(
    path: &Path,
    anchor: &str,
    prefix: &str,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let block = extract_braced_block(&contents, anchor)?;
    let mut commands = BTreeSet::new();

    for line in block.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(prefix) {
            continue;
        }

        if let Some(variant) = extract_prefixed_variant_name(trimmed, prefix) {
            if variant == "Unsupported" {
                continue;
            }
            let command = public_command_name(&variant);
            commands.insert(command);
        }
    }

    Ok(commands)
}

fn extract_list_commands(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(ToOwned::to_owned)
        .collect()
}

fn extract_braced_block<'a>(contents: &'a str, anchor: &str) -> Result<&'a str, Box<dyn Error>> {
    let anchor_offset = contents
        .find(anchor)
        .ok_or_else(|| format!("missing anchor `{anchor}`"))?;
    let brace_offset = contents[anchor_offset..]
        .find('{')
        .ok_or_else(|| format!("missing opening brace after `{anchor}`"))?;
    let body_start = anchor_offset + brace_offset + 1;
    let mut depth = 0usize;

    for (offset, character) in contents[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' if depth == 0 => return Ok(&contents[body_start..body_start + offset]),
            '}' => depth -= 1,
            _ => {}
        }
    }

    Err(format!("unterminated block after `{anchor}`").into())
}

fn extract_variant_name(line: &str) -> Option<String> {
    let variant: String = line
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .collect();
    (!variant.is_empty()).then_some(variant)
}

fn extract_prefixed_variant_name(line: &str, prefix: &str) -> Option<String> {
    let remainder = line.strip_prefix(prefix)?;
    extract_variant_name(remainder)
}

fn camel_case_to_kebab(value: &str) -> String {
    let mut output = String::new();

    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                output.push('-');
            }
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }

    output
}

fn public_command_name(variant: &str) -> String {
    match variant {
        "Prompt" => "command-prompt".to_owned(),
        _ => camel_case_to_kebab(variant),
    }
}

fn wait_for_capture(
    harness: &CliHarness,
    marker: &str,
) -> Result<std::process::Output, Box<dyn Error>> {
    wait_for_capture_target(harness, "alpha:0.0", marker)
}

fn wait_for_capture_target(
    harness: &CliHarness,
    target: &str,
    marker: &str,
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut last = None;

    for _ in 0..100 {
        let output = harness.run(&["capture-pane", "-p", "-t", target])?;
        if output.status.code() == Some(0) && stdout(&output).contains(marker) {
            return Ok(output);
        }
        last = Some(output);
        thread::sleep(Duration::from_millis(20));
    }

    let last = last.expect("capture-pane must run at least once");
    Err(format!(
        "capture-pane -p -t {target} never surfaced marker {marker}; status={:?} stdout={:?} stderr={:?}",
        last.status.code(),
        stdout(&last),
        stderr(&last)
    )
    .into())
}

fn nonempty_lines(output: &str) -> Vec<&str> {
    output.lines().filter(|line| !line.is_empty()).collect()
}

fn spawn_cli_process(harness: &CliHarness, args: &[&str]) -> Result<ChildGuard, Box<dyn Error>> {
    let mut command = harness.base_command();
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    Ok(ChildGuard(command.spawn()?))
}

fn assert_process_blocks(child: &mut ChildGuard, label: &str) -> Result<(), Box<dyn Error>> {
    thread::sleep(WAIT_FOR_BLOCK_TIMEOUT);
    assert!(
        child.try_wait()?.is_none(),
        "{label} should still be blocked"
    );
    Ok(())
}

struct ChildGuard(Child);

impl ChildGuard {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, Box<dyn Error>> {
        Ok(self.0.try_wait()?)
    }

    fn wait(&mut self) -> Result<ExitStatus, Box<dyn Error>> {
        for _ in 0..20 {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            thread::sleep(ATTACH_TIMEOUT / 20);
        }

        Ok(self.0.wait()?)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
