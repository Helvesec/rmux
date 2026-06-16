#![cfg(unix)]

mod common;

use std::{error::Error, fs, time::Duration};

use common::{assert_success, read_until_contains, stderr, stdout, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;
use serde_json::Value;

#[test]
fn capabilities_json_advertises_binary_contract() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripting-capabilities")?;
    let output = harness.run(&["capabilities", "--json"])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    let value: Value = serde_json::from_str(&stdout(&output))?;
    assert_eq!(value["binary_contract_version"], 1);
    assert_eq!(
        json_string_array(&value["json_commands"]),
        fixture_lines("conformance/json_commands.txt")
    );
    assert_eq!(
        json_string_array(&value["control_notifications"]),
        fixture_lines("conformance/control_notifications.txt")
    );
    assert_eq!(
        json_string_array(&value["format_tokens"]),
        fixture_lines("conformance/format_tokens.txt")
    );
    assert_eq!(value["control_mode"]["entrypoint"], "rmux -C");
    assert_eq!(value["control_mode"]["line_ending"], "\\n");
    assert_eq!(value["control_mode"]["unknown_percent_lines"], "ignore");
    assert_eq!(
        value["control_mode"]["output_escape"]["encoding"],
        "tmux-octal"
    );
    assert_eq!(
        value["control_mode"]["line_shapes"]["%output"][0],
        "%output"
    );
    Ok(())
}

#[test]
fn capabilities_is_discoverable_as_rmux_extension() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripting-capabilities-discovery")?;

    let explicit = harness.run(&["list-commands", "capabilities"])?;
    assert_eq!(explicit.status.code(), Some(0));
    assert!(stderr(&explicit).is_empty());
    assert_eq!(stdout(&explicit), "capabilities [--human|--json]\n");

    let bare = harness.run(&["list-commands", "-F", "#{command_list_name}"])?;
    assert_eq!(bare.status.code(), Some(0));
    assert!(stderr(&bare).is_empty());
    assert!(
        !stdout(&bare).lines().any(|line| line == "capabilities"),
        "bare list-commands stays tmux-compatible"
    );
    Ok(())
}

#[test]
fn list_commands_emit_machine_readable_json() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripting-json-lists")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let sessions = run_json(&harness, &["list-sessions", "--json"])?;
    let sessions = sessions.as_array().expect("sessions is an array");
    assert_eq!(sessions.len(), 1);
    assert_object_keys(&sessions[0], "conformance/list_sessions_keys.txt");
    assert_eq!(sessions[0]["session_name"], "alpha");
    assert_eq!(sessions[0]["session_windows"], 2);
    assert_eq!(sessions[0]["session_attached"], false);

    let windows = run_json(&harness, &["list-windows", "-t", "alpha", "--json"])?;
    let windows = windows.as_array().expect("windows is an array");
    assert_eq!(windows.len(), 2);
    assert_object_keys(&windows[0], "conformance/list_windows_keys.txt");
    assert_eq!(windows[0]["session_name"], "alpha");
    assert_eq!(windows[0]["window_index"], 0);
    assert_eq!(windows[1]["window_name"], "logs");

    let panes = run_json(&harness, &["list-panes", "-a", "--json"])?;
    let panes = panes.as_array().expect("panes is an array");
    assert_eq!(panes.len(), 3);
    assert_object_keys(&panes[0], "conformance/list_panes_keys.txt");
    assert!(panes.iter().all(|pane| pane["session_name"] == "alpha"));
    assert!(panes.iter().any(|pane| pane["pane_active"] == true));

    let clients = run_json(&harness, &["list-clients", "--json"])?;
    let clients = clients.as_array().expect("clients is an array");
    assert_eq!(clients.len(), 0);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(Duration::from_secs(5))?;
    let _ = read_until_contains(
        attach.master_mut(),
        "tester@RMUXHOST",
        Duration::from_secs(5),
    )?;

    let clients = run_json(&harness, &["list-clients", "--json"])?;
    let clients = clients.as_array().expect("clients is an array");
    assert_eq!(clients.len(), 1);
    assert_object_keys(&clients[0], "conformance/list_clients_keys.txt");
    assert_eq!(clients[0]["client_session"], "alpha");
    assert_eq!(clients[0]["client_control_mode"], false);

    assert_success(&harness.run(&["detach-client"])?);
    let status = attach.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "attach-session exited with {status}");
    Ok(())
}

#[test]
fn display_message_emits_machine_readable_json() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripting-json-display-message")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let message = run_json(
        &harness,
        &[
            "display-message",
            "--json",
            "-t",
            "alpha:0.0",
            "#{session_name}",
        ],
    )?;

    assert_object_keys(&message, "conformance/display_message_keys.txt");
    assert_eq!(message["message"], "alpha");
    Ok(())
}

#[test]
fn list_panes_json_preserves_newlines_in_string_fields() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripting-json-list-panes-newline")?;
    let _daemon = harness.start_hidden_daemon()?;
    let cwd = harness.tmpdir().join("path-with\nnewline");
    fs::create_dir_all(&cwd)?;

    let output = harness.run_with(&["new-session", "-d", "-s", "newline"], |command| {
        command.current_dir(&cwd);
    })?;
    assert_success(&output);

    let panes = run_json(&harness, &["list-panes", "-t", "newline", "--json"])?;
    let panes = panes.as_array().expect("panes is an array");
    let expected_cwd = fs::canonicalize(&cwd)?.to_string_lossy().into_owned();
    assert_eq!(panes.len(), 1);
    assert_eq!(
        panes[0]["pane_current_path"].as_str(),
        Some(expected_cwd.as_str())
    );
    Ok(())
}

#[test]
fn json_conflicts_with_format_strings() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("scripting-json-conflict")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["list-sessions", "--json", "-F", "#{session_name}"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("cannot be used with"),
        "unexpected stderr: {}",
        stderr(&output)
    );

    let output = harness.run(&["display-message", "--json", "-p", "#{session_name}"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("cannot be used with"),
        "unexpected stderr: {}",
        stderr(&output)
    );
    Ok(())
}

#[test]
fn control_mode_conformance_transcript_uses_advertised_prefixes() {
    let notifications = fixture_lines("conformance/control_notifications.txt");
    let transcript = include_str!("conformance/control_transcript.txt");

    for line in transcript.lines().filter(|line| !line.is_empty()) {
        let prefix = line
            .split_whitespace()
            .next()
            .expect("transcript line has a prefix");
        assert!(
            notifications
                .iter()
                .any(|notification| notification == prefix),
            "unadvertised control-mode line in conformance transcript: {line}"
        );
    }
}

fn run_json(harness: &CliHarness, args: &[&str]) -> Result<Value, Box<dyn Error>> {
    let output = harness.run(args)?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty());
    Ok(serde_json::from_str(&stdout(&output))?)
}

fn fixture_lines(path: &str) -> Vec<String> {
    let content = match path {
        "conformance/control_notifications.txt" => {
            include_str!("conformance/control_notifications.txt")
        }
        "conformance/display_message_keys.txt" => {
            include_str!("conformance/display_message_keys.txt")
        }
        "conformance/format_tokens.txt" => include_str!("conformance/format_tokens.txt"),
        "conformance/json_commands.txt" => include_str!("conformance/json_commands.txt"),
        "conformance/list_clients_keys.txt" => include_str!("conformance/list_clients_keys.txt"),
        "conformance/list_panes_keys.txt" => include_str!("conformance/list_panes_keys.txt"),
        "conformance/list_sessions_keys.txt" => include_str!("conformance/list_sessions_keys.txt"),
        "conformance/list_windows_keys.txt" => include_str!("conformance/list_windows_keys.txt"),
        _ => panic!("unknown conformance fixture {path}"),
    };
    content
        .lines()
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn json_string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("value is an array")
        .iter()
        .map(|value| value.as_str().expect("array entry is a string").to_owned())
        .collect()
}

fn assert_object_keys(value: &Value, fixture: &str) {
    let object = value.as_object().expect("value is an object");
    let mut actual = object.keys().cloned().collect::<Vec<_>>();
    let mut expected = fixture_lines(fixture);
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}
