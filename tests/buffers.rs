#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, terminate_child, CliHarness};

#[test]
fn set_buffer_and_show_buffer_round_trip() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-set-show")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "hello world"])?);

    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "hello world");
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_buffer_named_and_show_by_name() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-named")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "mybuf", "named data"])?);

    let show = harness.run(&["show-buffer", "-b", "mybuf"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "named data");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_buffer_accepts_ignored_target() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-set-target")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "-t", "alpha", "target-tolerated"])?);

    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "target-tolerated");
    assert!(stderr(&show).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn list_buffers_shows_entries() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-list")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "first"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "named", "second"])?);

    let list = harness.run(&["list-buffers"])?;
    assert_eq!(list.status.code(), Some(0));
    let out = stdout(&list);
    assert!(out.contains("named:"), "should contain named buffer");
    assert!(out.contains("buffer0:"), "should contain unnamed buffer");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn list_buffers_bare_reverse_does_not_reverse_default_order() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-list-bare-reverse")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "gamma", "g"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "alpha", "a"])?);

    let default = harness.run(&["list-buffers", "-F", "#{buffer_name}"])?;
    assert_eq!(default.status.code(), Some(0));
    assert_eq!(stdout(&default), "alpha\ngamma\n");

    let bare_reverse = harness.run(&["list-buffers", "-r", "-F", "#{buffer_name}"])?;
    assert_eq!(bare_reverse.status.code(), Some(0));
    assert_eq!(stdout(&bare_reverse), stdout(&default));

    let explicit_reverse =
        harness.run(&["list-buffers", "-O", "name", "-r", "-F", "#{buffer_name}"])?;
    assert_eq!(explicit_reverse.status.code(), Some(0));
    assert_eq!(stdout(&explicit_reverse), "gamma\nalpha\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn show_buffer_without_name_uses_latest_automatic_buffer() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-show-top-auto")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "auto"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "named", "manual"])?);

    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "auto");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn show_buffer_without_name_rejects_named_only_store() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-show-named-only")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "named", "manual"])?);

    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("no buffers"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn list_buffers_empty_returns_no_output() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-list-empty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let list = harness.run(&["list-buffers"])?;
    assert_eq!(list.status.code(), Some(0));
    assert!(stdout(&list).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn delete_buffer_removes_stack_head() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-delete")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "a"])?);
    assert_success(&harness.run(&["set-buffer", "b"])?);
    assert_success(&harness.run(&["delete-buffer"])?);

    // Remaining buffer should contain "a"
    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "a");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn delete_buffer_without_name_uses_latest_automatic_buffer() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-delete-top-auto")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "auto"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "named", "manual"])?);
    assert_success(&harness.run(&["delete-buffer"])?);

    let auto = harness.run(&["show-buffer", "-b", "buffer0"])?;
    assert_eq!(auto.status.code(), Some(1));
    assert!(stderr(&auto).contains("no buffer buffer0"));

    let named = harness.run(&["show-buffer", "-b", "named"])?;
    assert_eq!(named.status.code(), Some(0));
    assert_eq!(stdout(&named), "manual");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn delete_buffer_nonexistent_returns_error() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-del-miss")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["delete-buffer", "-b", "missing"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "unknown buffer: missing\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn show_buffer_empty_store_returns_error() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-show-empty")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["show-buffer"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no buffers"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn paste_buffer_to_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-paste")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "paste-me"])?);
    assert_success(&harness.run(&["paste-buffer", "-t", "alpha:0.0"])?);

    // Buffer should still exist
    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(0));
    assert_eq!(stdout(&show), "paste-me");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn paste_buffer_without_name_uses_latest_automatic_buffer() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-paste-top-auto")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "auto-paste"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "named", "manual-paste"])?);
    assert_success(&harness.run(&["paste-buffer", "-t", "alpha:0.0"])?);

    let capture = wait_for_capture_contains(&harness, "alpha:0.0", "auto-paste")?;
    assert!(
        stdout(&capture).contains("auto-paste"),
        "paste-buffer should use the automatic buffer by default"
    );
    assert!(
        !stdout(&capture).contains("manual-paste"),
        "named buffer must not be pasted without -b"
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn paste_buffer_with_delete_flag() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-paste-d")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "temp-data"])?);
    assert_success(&harness.run(&["paste-buffer", "-d", "-t", "alpha:0.0"])?);

    // Buffer should be gone
    let show = harness.run(&["show-buffer"])?;
    assert_eq!(show.status.code(), Some(1));
    assert!(stderr(&show).contains("no buffers"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_buffer_append_empty_payload_is_a_noop() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-append-empty-noop")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "named", "original"])?);
    assert_success(&harness.run(&["set-buffer", "head"])?);
    assert_success(&harness.run(&["set-buffer", "-a", "-b", "named", ""])?);

    let named = harness.run(&["show-buffer", "-b", "named"])?;
    assert_eq!(named.status.code(), Some(0));
    assert_eq!(stdout(&named), "original");

    let head = harness.run(&["show-buffer"])?;
    assert_eq!(head.status.code(), Some(0));
    assert_eq!(stdout(&head), "head");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_buffer_rename_without_buffer_name_prefers_latest_unnamed_buffer(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-rename-top-unnamed")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "auto"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "named", "manual"])?);
    assert_success(&harness.run(&["set-buffer", "-n", "renamed"])?);

    let renamed = harness.run(&["show-buffer", "-b", "renamed"])?;
    assert_eq!(renamed.status.code(), Some(0));
    assert_eq!(stdout(&renamed), "auto");

    let named = harness.run(&["show-buffer", "-b", "named"])?;
    assert_eq!(named.status.code(), Some(0));
    assert_eq!(stdout(&named), "manual");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_buffer_rename_ignores_trailing_content() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-rename-ignore-trailing")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "src", "original"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "src", "-n", "dst", "ignored"])?);

    let renamed = harness.run(&["show-buffer", "-b", "dst"])?;
    assert_eq!(renamed.status.code(), Some(0));
    assert_eq!(stdout(&renamed), "original");
    assert!(stderr(&renamed).is_empty());

    let old = harness.run(&["show-buffer", "-b", "src"])?;
    assert_eq!(old.status.code(), Some(1));
    assert!(stderr(&old).contains("no buffer src"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_buffer_rename_without_buffer_name_rejects_named_only_store() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-rename-named-only")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-buffer", "-b", "named", "manual"])?);

    let rename = harness.run(&["set-buffer", "-n", "renamed"])?;
    assert_eq!(rename.status.code(), Some(1));
    assert!(stderr(&rename).contains("no buffer"));

    let named = harness.run(&["show-buffer", "-b", "named"])?;
    assert_eq!(named.status.code(), Some(0));
    assert_eq!(stdout(&named), "manual");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn buffer_commands_report_absent_server_on_stderr() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("buf-absent")?;

    for &(command, args) in &[
        ("set-buffer", &["hello"] as &[&str]),
        ("show-buffer", &[]),
        ("list-buffers", &[]),
        ("delete-buffer", &[]),
    ] {
        let mut full_args = vec![command];
        full_args.extend_from_slice(args);
        let output = harness.run(&full_args)?;

        assert_eq!(
            output.status.code(),
            Some(1),
            "{command} should exit 1 on absent server"
        );
        assert!(
            stderr(&output).contains(&format!(
                "no server running on {}",
                harness.socket_path().display()
            )),
            "{command} stderr should report absent server, got: {}",
            stderr(&output)
        );
        assert!(
            stdout(&output).is_empty(),
            "{command} should produce no stdout"
        );
    }

    Ok(())
}

fn wait_for_capture_contains(
    harness: &CliHarness,
    target: &str,
    marker: &str,
) -> Result<std::process::Output, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last = harness.run(&["capture-pane", "-p", "-t", target])?;
    while Instant::now() < deadline {
        if last.status.code() == Some(0) && stdout(&last).contains(marker) {
            return Ok(last);
        }
        std::thread::sleep(Duration::from_millis(25));
        last = harness.run(&["capture-pane", "-p", "-t", target])?;
    }
    Ok(last)
}
