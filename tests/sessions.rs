#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use common::acquire_empty_socket_path_lock;
use common::{
    assert_success, is_transient_daemon_stall, read_until_contains,
    run_success_with_transient_retry, stderr, stdout, AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const NONBLOCKING_ATTACH_TIMEOUT: Duration = Duration::from_millis(500);

fn assert_missing_has_session(output: &std::process::Output, session_name: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(output).is_empty());
    assert_eq!(
        stderr(output),
        format!("can't find session: {session_name}\n")
    );
}

#[cfg(target_os = "linux")]
fn tree_contains_control_empty_socket_label(root: &Path) -> Result<bool, Box<dyn Error>> {
    const NEEDLE: &[u8] = b"\x1fempty-S";

    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path
            .as_os_str()
            .as_bytes()
            .windows(NEEDLE.len())
            .any(|window| window == NEEDLE)
        {
            return Ok(true);
        }

        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path
                .as_os_str()
                .as_bytes()
                .windows(NEEDLE.len())
                .any(|window| window == NEEDLE)
            {
                return Ok(true);
            }
            if entry.file_type()?.is_dir() {
                stack.push(entry_path);
            }
        }
    }

    Ok(false)
}

#[test]
fn list_sessions_prints_sorted_server_rendered_stdout() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-sessions-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-x", "120", "-y", "40"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);

    let listed = harness.run(&[
        "list-sessions",
        "-F",
        "#{session_name}:#{session_windows}:#{session_attached}:#{session_width}x#{session_height}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:2:0:x\nbeta:1:0:x\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn new_session_accepts_attached_short_value_flags_at_runtime() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-attached-short-values")?;
    let _daemon = harness.start_hidden_daemon()?;

    let created = harness.run(&["new-session", "-P", "-F#{pane_id}", "-sfoo", "-d"])?;

    assert_eq!(created.status.code(), Some(0));
    assert_eq!(stdout(&created), "%0\n");
    assert!(stderr(&created).is_empty());
    assert_success(&harness.run(&["has-session", "-t", "foo"])?);
    Ok(())
}

#[test]
fn new_session_print_format_uses_spawned_command_metadata() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-print-command-metadata")?;
    let _daemon = harness.start_hidden_daemon()?;

    for index in 0..5 {
        let session = format!("printed{index}");
        let created = harness.run(&[
            "new-session",
            "-dP",
            "-F",
            "#{pane_current_command}:#{pane_start_command}",
            "-s",
            &session,
            "sleep 60",
        ])?;

        assert_eq!(created.status.code(), Some(0));
        assert_eq!(stdout(&created), "sleep:\"sleep 60\"\n");
        assert!(stderr(&created).is_empty());
        assert_success(&harness.run(&["kill-session", "-t", &session])?);
    }
    Ok(())
}

#[test]
fn startup_exit_empty_with_implicit_config() -> Result<(), Box<dyn Error>> {
    assert_startup_exit_empty(None)
}

#[test]
fn startup_exit_empty_with_explicit_config() -> Result<(), Box<dyn Error>> {
    assert_startup_exit_empty(Some("explicit.conf"))
}

fn assert_startup_exit_empty(explicit_config: Option<&str>) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("startup-exit-empty")?;
    let config = harness
        .tmpdir()
        .join(explicit_config.unwrap_or("home/.rmux.conf"));
    fs::write(&config, "set-option -s exit-empty on\n")?;
    let mut create = harness.base_command();
    create.arg("-S").arg(harness.socket_path());
    if explicit_config.is_some() {
        create.arg("-f").arg(&config);
    }
    assert_success(
        &create
            .args(["new-session", "-d", "-s", "temoin", "sleep 4"])
            .output()?,
    );
    let listed = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", stderr(&listed));
    assert_eq!(stdout(&listed), "temoin\n");
    common::wait_for_socket_directory_empty(harness.socket_path(), Duration::from_secs(10))
}

/// Killing the last session makes an `exit-empty` daemon stop accepting while
/// its endpoint stays bound until the shutdown drain ends. A `new-session` in
/// that window used to fail on the queued, never-served connection; a slow
/// `session-closed` hook widens the window load produced in `sessions.rs:147`.
#[test]
fn new_session_after_last_kill_session_waits_for_the_exiting_daemon() -> Result<(), Box<dyn Error>>
{
    const HOOK_HOLD: Duration = Duration::from_secs(3);

    let harness = CliHarness::new("new-session-after-exit-empty")?;
    let _daemon = harness.start_hidden_daemon()?;
    let hook_started = harness.tmpdir().join("session-closed-hook-started");
    let hook = format!(
        "run-shell \"touch '{}'; sleep {}\"",
        hook_started.display(),
        HOOK_HOLD.as_secs()
    );

    assert_success(&harness.run(&["new-session", "-d", "-s", "last", "sleep 60"])?);
    assert_success(&harness.run(&["set-hook", "-g", "session-closed", &hook])?);
    assert_success(&harness.run(&["kill-session", "-t", "last"])?);

    let deadline = Instant::now() + HOOK_HOLD;
    while !hook_started.exists() {
        assert!(Instant::now() < deadline, "session-closed hook never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The hook runs while the daemon drains; let exit-empty close admission.
    std::thread::sleep(Duration::from_millis(300));

    let created = harness.run(&["new-session", "-d", "-s", "next", "sleep 60"])?;
    assert_success(&created);
    assert_success(&harness.run(&["has-session", "-t", "next"])?);
    Ok(())
}

#[test]
fn new_session_attach_if_exists_does_not_silently_succeed_when_detached(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-attach-if-exists-detached")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let existing = harness.run(&["new-session", "-Ad", "-s", "alpha"])?;

    assert_eq!(existing.status.code(), Some(1));
    assert!(stdout(&existing).is_empty());
    assert_eq!(stderr(&existing), "open terminal failed: not a terminal\n");
    Ok(())
}

#[test]
fn non_terminal_new_session_rejects_before_creating_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-notty-preflight")?;
    let _daemon = harness.start_hidden_daemon()?;

    let attached = harness.run(&["new-session", "-s", "alpha"])?;

    assert_eq!(attached.status.code(), Some(1));
    assert!(stdout(&attached).is_empty());
    assert_eq!(stderr(&attached), "open terminal failed: not a terminal\n");

    let detached = harness.run(&["new-session", "-d", "-s", "alpha"])?;
    assert_success(&detached);
    Ok(())
}

#[test]
fn non_terminal_new_session_preserves_duplicate_error_order() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-notty-duplicate")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let duplicate = harness.run(&["new-session", "-s", "alpha"])?;

    assert_eq!(duplicate.status.code(), Some(1));
    assert!(stdout(&duplicate).is_empty());
    assert_eq!(stderr(&duplicate), "duplicate session: alpha\n");
    Ok(())
}

#[test]
fn new_session_rejects_zero_dimensions_before_creating_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-zero-dimensions")?;

    let zero_width = harness.run(&["new-session", "-d", "-s", "zero-width", "-x", "0"])?;
    assert_eq!(zero_width.status.code(), Some(1));
    assert!(stdout(&zero_width).is_empty());
    assert_eq!(stderr(&zero_width), "width too small\n");

    let zero_height = harness.run(&["new-session", "-d", "-s", "zero-height", "-y", "0"])?;
    assert_eq!(zero_height.status.code(), Some(1));
    assert!(stdout(&zero_height).is_empty());
    assert_eq!(stderr(&zero_height), "height too small\n");

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn empty_socket_path_uses_abstract_endpoint_without_sentinel_file() -> Result<(), Box<dyn Error>> {
    let _empty_socket_lock = acquire_empty_socket_path_lock()?;
    let harness = CliHarness::new("empty-socket-abstract")?;
    let _ = harness.run(&["-S", "", "kill-server"])?;

    let created = harness.run(&[
        "-S",
        "",
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "empty",
    ])?;
    assert_success(&created);

    let listed = harness.run(&["-S", "", "list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "empty\n");
    assert!(stderr(&listed).is_empty());

    assert!(
        !tree_contains_control_empty_socket_label(harness.tmpdir())?,
        "-S '' must not create the legacy control-character sentinel socket"
    );

    let killed = harness.run(&["-S", "", "kill-server"])?;
    assert_success(&killed);
    Ok(())
}

#[test]
fn hidden_environment_without_target_uses_current_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("setenv-hidden-default-session")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let set = harness.run(&["set-environment", "-h", "SECRET", "classified"])?;
    assert_success(&set);

    let normal = harness.run(&["show-environment", "SECRET"])?;
    assert_eq!(normal.status.code(), Some(0));
    assert!(stdout(&normal).is_empty());
    assert!(stderr(&normal).is_empty());

    let hidden = harness.run(&["show-environment", "-h", "SECRET"])?;
    assert_eq!(hidden.status.code(), Some(0));
    assert_eq!(stdout(&hidden), "SECRET=classified\n");
    assert!(stderr(&hidden).is_empty());

    Ok(())
}

#[test]
fn set_environment_accepts_hyphen_prefixed_values_at_runtime() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("setenv-hyphen-value")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let set = harness.run(&["set-environment", "TERM", "-screen"])?;
    assert_success(&set);

    let shown = harness.run(&["show-environment", "TERM"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), "TERM=-screen\n");
    assert!(stderr(&shown).is_empty());

    Ok(())
}

#[test]
fn nested_default_invocation_refuses_without_creating_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("nested-default-invocation")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "outer"])?);
    let nested_env = format!("{},123,0", harness.socket_path().display());

    let refused = harness.run_with(&[], |command| {
        command.env("TMUX", &nested_env);
    })?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(stdout(&refused).is_empty());
    assert_eq!(
        stderr(&refused),
        "sessions should be nested with care, unset $TMUX to force\n"
    );

    let sessions = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(sessions.status.code(), Some(0));
    assert_eq!(stdout(&sessions), "outer\n");

    let detached = harness.run_with(&["new-session", "-d", "-s", "detached"], |command| {
        command.env("TMUX", &nested_env);
    })?;
    assert_success(&detached);
    let sessions = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(sessions.status.code(), Some(0));
    assert_eq!(stdout(&sessions), "detached\nouter\n");

    Ok(())
}

#[test]
fn session_window_and_pane_id_targets_resolve_through_cli() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("id-targets-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "foo"])?);
    let ids = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "foo",
        "#{session_id} #{window_id} #{pane_id}",
    ])?;
    assert_eq!(ids.status.code(), Some(0));
    assert!(stderr(&ids).is_empty());
    let ids_stdout = stdout(&ids);
    let mut parts = ids_stdout.split_whitespace();
    let session_id = parts.next().expect("session id");
    let window_id = parts.next().expect("window id");
    let pane_id = parts.next().expect("pane id");

    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-t",
        session_id,
        "-n",
        "via-session-id",
    ])?);
    assert_success(&harness.run(&["rename-window", "-t", window_id, "via-window-id"])?);
    assert_success(&harness.run(&["send-keys", "-t", pane_id, "x"])?);

    let windows = harness.run(&["list-windows", "-t", "foo", "-F", "#{window_name}"])?;
    assert_eq!(windows.status.code(), Some(0));
    let rendered = stdout(&windows);
    assert!(rendered.contains("via-session-id\n"), "{rendered:?}");
    assert!(rendered.contains("via-window-id\n"), "{rendered:?}");
    assert!(stderr(&windows).is_empty());
    Ok(())
}

#[test]
fn repeated_target_flags_use_the_last_tmux_target() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("repeated-target-last-wins")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    let alpha_id = harness.run(&["display-message", "-p", "-t", "alpha", "#{session_id}"])?;
    let beta_id = harness.run(&["display-message", "-p", "-t", "beta", "#{session_id}"])?;
    assert_eq!(alpha_id.status.code(), Some(0));
    assert!(stderr(&alpha_id).is_empty());
    assert_eq!(beta_id.status.code(), Some(0));
    assert!(stderr(&beta_id).is_empty());
    let alpha_target = format!("{}:", stdout(&alpha_id).trim());
    let beta_target = format!("{}:", stdout(&beta_id).trim());

    let created = harness.run(&[
        "new-window",
        "-d",
        "-t",
        &alpha_target,
        "-P",
        "-F",
        "#{session_name}:#{window_name}",
        "-t",
        &beta_target,
        "-n",
        "chosen",
    ])?;

    assert_eq!(created.status.code(), Some(0));
    assert_eq!(stdout(&created), "beta:chosen\n");
    assert!(stderr(&created).is_empty());

    let malformed_first = harness.run(&[
        "new-window",
        "-d",
        "-t",
        "bad:xyz.???",
        "-t",
        &alpha_target,
        "-n",
        "separated-recovered",
        "-P",
        "-F",
        "#{session_name}:#{window_name}",
    ])?;
    assert_eq!(malformed_first.status.code(), Some(0));
    assert_eq!(stdout(&malformed_first), "alpha:separated-recovered\n");
    assert!(stderr(&malformed_first).is_empty());

    let compact_invalid = "-tbad:xyz.???";
    let compact_beta = format!("-t{beta_target}");
    let compact_first = harness.run(&[
        "new-window",
        "-d",
        compact_invalid,
        &compact_beta,
        "-n",
        "compact-recovered",
        "-P",
        "-F",
        "#{session_name}:#{window_name}",
    ])?;
    assert_eq!(compact_first.status.code(), Some(0));
    assert_eq!(stdout(&compact_first), "beta:compact-recovered\n");
    assert!(stderr(&compact_first).is_empty());

    let malformed_last = harness.run(&[
        "new-window",
        "-d",
        "-t",
        &alpha_target,
        "-t",
        "bad:xyz.???",
        "-n",
        "must-not-exist",
    ])?;
    assert_eq!(malformed_last.status.code(), Some(1));
    assert!(stdout(&malformed_last).is_empty());
    assert!(!stderr(&malformed_last).is_empty());

    assert_success(&harness.run(&[
        "swap-window",
        "-s",
        "bad:xyz.???",
        "-s",
        "alpha:0",
        "-t",
        "beta:0",
    ])?);
    Ok(())
}

#[test]
fn environment_commands_resolve_session_id_targets() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("environment-session-id-target")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let session_id = harness.run(&["display-message", "-p", "-t", "alpha", "#{session_id}"])?;
    assert_eq!(session_id.status.code(), Some(0));
    assert!(stderr(&session_id).is_empty());
    let session_id = stdout(&session_id).trim().to_owned();

    assert_success(&harness.run(&["set-environment", "-t", &session_id, "FOO", "BAR"])?);
    let shown = harness.run(&["show-environment", "-t", &session_id, "FOO"])?;

    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), "FOO=BAR\n");
    assert!(stderr(&shown).is_empty());
    Ok(())
}

#[test]
fn new_session_environment_option_persists_to_session_environment() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-environment-option")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-e", "FOO_CHILD=bar"])?);
    let shown = harness.run(&["show-environment", "-t", "alpha", "FOO_CHILD"])?;

    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), "FOO_CHILD=bar\n");
    assert!(stderr(&shown).is_empty());
    Ok(())
}

#[test]
fn list_sessions_supports_filter_sort_order_surface_and_reverse() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-sessions-filter-sort")?;
    let _daemon = harness.start_hidden_daemon()?;

    for name in ["alpha", "beta", "gamma"] {
        assert_success(&harness.run(&["new-session", "-d", "-s", name])?);
    }

    let listed = harness.run(&[
        "list-sessions",
        "-f",
        "#{||:#{==:#{session_name},alpha},#{==:#{session_name},gamma}}",
        "-F",
        "#{session_name}",
    ])?;

    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha\ngamma\n");
    assert!(stderr(&listed).is_empty());

    let sort = harness.run(&["list-sessions", "-O"])?;
    assert_eq!(sort.status.code(), Some(1));
    assert_eq!(
        stderr(&sort),
        "command list-sessions: -O expects an argument\n"
    );

    let bare_reverse = harness.run(&["list-sessions", "-r", "-F", "#{session_name}"])?;
    assert_eq!(bare_reverse.status.code(), Some(0));
    assert_eq!(stdout(&bare_reverse), "alpha\nbeta\ngamma\n");
    assert!(stderr(&bare_reverse).is_empty());

    let explicit_reverse =
        harness.run(&["list-sessions", "-O", "name", "-r", "-F", "#{session_name}"])?;
    assert_eq!(explicit_reverse.status.code(), Some(0));
    assert_eq!(stdout(&explicit_reverse), "gamma\nbeta\nalpha\n");
    assert!(stderr(&explicit_reverse).is_empty());

    Ok(())
}

#[test]
fn next_session_id_targets_the_next_named_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("next-session-id")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "first"])?);

    let predicted = harness.run(&["display-message", "-p", "#{next_session_id}"])?;
    assert!(predicted.status.success(), "{}", stderr(&predicted));
    let created = harness.run(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id}",
        "-s",
        "second",
    ])?;
    assert!(created.status.success(), "{}", stderr(&created));
    assert_eq!(stdout(&created), "$1\n");
    assert_eq!(stdout(&predicted), stdout(&created));
    assert_success(&harness.run(&["has-session", "-t", stdout(&predicted).trim_end()])?);
    Ok(())
}

#[test]
fn new_session_prints_formatted_session_info() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-print-info")?;
    let _daemon = harness.start_hidden_daemon()?;

    let created = harness.run(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_name}:#{session_width}x#{session_height}",
        "-s",
        "alpha",
        "-x",
        "120",
        "-y",
        "40",
    ])?;

    assert_eq!(created.status.code(), Some(0));
    assert_eq!(stdout(&created), "alpha:x\n");
    assert!(stderr(&created).is_empty());
    assert_success(&harness.run(&["has-session", "-t", "alpha"])?);
    Ok(())
}

#[test]
fn auto_named_sessions_follow_tmux_global_session_id_sequence() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-auto-id-shape")?;
    let _daemon = harness.start_hidden_daemon()?;

    for name in ["0", "1", "bob"] {
        assert_success(&harness.run(&["new-session", "-d", "-s", name])?);
    }

    let created = harness.run(&["new-session", "-d", "-P", "-F", "#{session_name}"])?;
    assert_eq!(created.status.code(), Some(0));
    assert_eq!(stdout(&created), "3\n");
    assert!(stderr(&created).is_empty());
    Ok(())
}

#[test]
fn grouped_sessions_without_explicit_name_follow_tmux_global_suffix_shape(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("grouped-session-auto-id-shape")?;
    let _daemon = harness.start_hidden_daemon()?;

    for name in ["0", "1", "bob"] {
        assert_success(&harness.run(&["new-session", "-d", "-s", name])?);
    }

    let created = harness.run(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_name}|#{session_group}|#{session_windows}",
        "-t",
        "stacy",
    ])?;
    assert_eq!(created.status.code(), Some(0));
    assert_eq!(stdout(&created), "stacy-3|stacy|1\n");
    assert!(stderr(&created).is_empty());

    let listed = harness.run(&[
        "list-sessions",
        "-F",
        "#{session_name}|#{session_group}|#{session_windows}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "0||1\n1||1\nbob||1\nstacy-3|stacy|1\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

#[test]
fn rename_session_updates_attached_client_tracking_and_session_local_state(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("rename-session-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    assert_success(&harness.run(&["set-environment", "-t", "alpha", "TERM", "screen"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(NONBLOCKING_ATTACH_TIMEOUT)?;

    assert_success(&harness.run(&["rename-session", "-t", "alpha", "gamma"])?);

    let show_environment = harness.run(&["show-environment", "-t", "gamma", "TERM"])?;
    assert_eq!(show_environment.status.code(), Some(0));
    assert_eq!(stdout(&show_environment), "TERM=screen\n");
    assert!(stderr(&show_environment).is_empty());

    let missing_old = harness.run(&["has-session", "-t", "alpha"])?;
    assert_missing_has_session(&missing_old, "alpha");

    assert_success(&harness.run(&["has-session", "-t", "gamma"])?);

    let listed = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "beta\ngamma\n");
    assert!(stderr(&listed).is_empty());

    let tmux_env = format!("{},1,0", harness.socket_path().display());
    let switched = harness.run_with(&["switch-client", "-t", "beta"], |command| {
        command.env("TMUX", &tmux_env);
    })?;
    assert_success(&switched);

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "beta:0.0",
        "printf beta-output",
        "Enter",
    ])?);
    let beta_output = read_until_contains(attach.master_mut(), "beta-output", ATTACH_TIMEOUT)?;
    assert!(beta_output.contains("beta-output"));

    assert_success(&harness.run(&["detach-client"])?);
    let status = attach.wait_for_exit(ATTACH_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    Ok(())
}

#[test]
fn grouped_sessions_share_windows_and_report_group_visibility() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("grouped-session-visibility")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "alpha", "-s", "beta"])?);
    assert_success(&harness.run(&["new-window", "-t", "beta", "-d", "-n", "shared"])?);

    let listed = harness.run(&[
        "list-sessions",
        "-F",
        "#{session_name}:#{session_group}:#{session_grouped}:#{session_group_size}:#{session_windows}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed), "alpha:alpha:1:2:2\nbeta:alpha:1:2:2\n");
    assert!(stderr(&listed).is_empty());

    let alpha_windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    let beta_windows = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(alpha_windows.status.code(), Some(0));
    assert_eq!(beta_windows.status.code(), Some(0));
    assert_eq!(stdout(&alpha_windows), stdout(&beta_windows));
    assert!(stdout(&alpha_windows).contains("1:shared"));
    Ok(())
}

#[test]
fn ungrouped_session_group_formats_are_empty() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("ungrouped-session-formats")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha",
        "#{session_grouped}|#{session_group}|#{session_group_list}|#{session_group_size}|#{session_group_attached}|#{session_group_many_attached}",
    ])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "0|||||\n");
    assert!(stderr(&output).is_empty());

    Ok(())
}

#[test]
fn grouped_sessions_copy_current_and_last_window_state_on_creation() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("grouped-session-current-window")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "logs"])?);
    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "shell"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:2"])?);
    assert_success(&harness.run(&["select-window", "-t", "alpha:1"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "alpha", "-s", "beta"])?);

    let alpha_windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_active}:#{window_last_flag}",
    ])?;
    let beta_windows = harness.run(&[
        "list-windows",
        "-t",
        "beta",
        "-F",
        "#{window_index}:#{window_active}:#{window_last_flag}",
    ])?;

    assert_eq!(alpha_windows.status.code(), Some(0));
    assert_eq!(beta_windows.status.code(), Some(0));
    assert_eq!(stdout(&alpha_windows), "0:0:0\n1:1:0\n2:0:1\n");
    assert_eq!(stdout(&beta_windows), "0:1:0\n1:0:0\n2:0:0\n");
    assert!(stderr(&alpha_windows).is_empty());
    assert!(stderr(&beta_windows).is_empty());
    Ok(())
}

#[test]
fn kill_session_only_removes_the_targeted_group_member() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("grouped-session-kill-member")?;
    let _daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("grouped-session-survivor.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-t", "alpha", "-s", "beta"])?);
    assert_success(&harness.run(&["new-window", "-t", "beta", "-d", "-n", "shared"])?);
    assert_success(&harness.run(&["kill-session", "-t", "alp"])?);

    let missing = harness.run(&["has-session", "-t", "alpha"])?;
    assert_missing_has_session(&missing, "alpha");
    assert_success(&harness.run(&["has-session", "-t", "beta"])?);

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "beta:1.0",
        &format!("printf survivor > {}", shell_quote(&output_path)),
        "Enter",
    ])?);
    wait_for_file_contents(&output_path, "survivor", ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn session_targeting_resolves_unique_prefixes_for_session_commands() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("session-command-prefix-targets")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["has-session", "-t", "alp"])?);
    assert_success(&harness.run(&["rename-session", "-t", "alp", "gamma"])?);
    assert_success(&harness.run(&["has-session", "-t", "gam"])?);
    assert_success(&harness.run(&["kill-session", "-t", "gam"])?);

    let missing = harness.run(&["has-session", "-t", "gamma"])?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(stdout(&missing).is_empty());
    assert!(
        stderr(&missing).contains("no server running on "),
        "has-session after the last session is killed should report absent server, got: {}",
        stderr(&missing)
    );
    Ok(())
}

#[test]
fn kill_session_all_except_target_preserves_only_the_named_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("kill-session-all-except")?;
    let _daemon = harness.start_hidden_daemon()?;

    for name in ["alpha", "beta", "gamma"] {
        run_success_with_transient_retry(&harness, &["new-session", "-d", "-s", name])?;
    }
    run_success_with_transient_retry(&harness, &["kill-session", "-a", "-t", "beta"])?;

    for (target, status) in [("alpha", 1), ("beta", 0), ("gamma", 1)] {
        let output = harness.run(&["has-session", "-t", target])?;
        if status == 0 {
            assert_eq!(output.status.code(), Some(0));
            assert!(stdout(&output).is_empty());
            assert!(stderr(&output).is_empty());
        } else {
            assert_missing_has_session(&output, target);
        }
    }
    Ok(())
}

/// Holds a process stopped with `SIGSTOP` and resumes it when dropped.
struct StoppedProcess {
    pid: libc::pid_t,
}

impl StoppedProcess {
    fn stop(pid: u32) -> Result<Self, Box<dyn Error>> {
        let pid = libc::pid_t::try_from(pid)?;
        // SAFETY: `pid` names a daemon child this test spawned and still owns.
        if unsafe { libc::kill(pid, libc::SIGSTOP) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { pid })
    }
}

impl Drop for StoppedProcess {
    fn drop(&mut self) {
        // SAFETY: resumes the same owned daemon child stopped in `stop`.
        unsafe {
            libc::kill(self.pid, libc::SIGCONT);
        }
    }
}

#[test]
fn transient_retry_recognizes_a_daemon_stalled_past_the_rpc_deadline() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("stalled-daemon-retry")?;
    let daemon = harness.start_hidden_daemon()?;
    run_success_with_transient_retry(&harness, &["new-session", "-d", "-s", "alpha"])?;

    let stopped = StoppedProcess::stop(daemon.pid())?;
    let stalled = harness.run(&["has-session", "-t", "alpha"])?;
    drop(stopped);

    assert_eq!(stalled.status.code(), Some(1));
    assert!(
        is_transient_daemon_stall(&stderr(&stalled)),
        "an expired RPC deadline must stay retryable, got stderr: {}",
        stderr(&stalled)
    );
    run_success_with_transient_retry(&harness, &["has-session", "-t", "alpha"])?;
    Ok(())
}

fn wait_for_file_contents(
    path: &Path,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }

    Err(format!("timed out waiting for '{}'", path.display()).into())
}

#[test]
fn destroy_unattached_on_removes_detached_sessions_immediately() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("destroy-unattached-immediate")?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "s", "sleep 30"])?);
    assert_success(&harness.run(&["set-option", "-g", "destroy-unattached", "on"])?);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let output = harness.run(&["has-session", "-t", "s"])?;
        if output.status.code() == Some(1) {
            assert!(stdout(&output).is_empty());
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "session still exists after destroy-unattached; stdout={:?} stderr={:?}",
                stdout(&output),
                stderr(&output)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
