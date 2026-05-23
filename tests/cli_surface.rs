#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use common::{
    assert_clap_failure, assert_socket_directory_empty, assert_success, read_until_contains,
    stderr, stdout, terminate_child, wait_for_socket, AttachedSession, CliHarness,
    BINARY_OVERRIDE_ENV, BINARY_OVERRIDE_TEST_OPT_IN_ENV,
};
use rmux_core::command_parser::COMMAND_TABLE;
use rmux_proto::{CONTROL_CONTROL_END, CONTROL_CONTROL_START};
use rmux_pty::TerminalSize;

const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const NONBLOCKING_ATTACH_TIMEOUT: Duration = Duration::from_millis(500);
const WORKFLOW_TRUECOLOR_FEATURES: &str =
    ",xterm-256color:RGB,tmux-256color:RGB,screen-256color:RGB,screen:RGB";
type SharedPipeBuffer = Arc<Mutex<Vec<u8>>>;
type PipeCollector = JoinHandle<io::Result<Vec<u8>>>;
const TOP_LEVEL_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";
const LONG_OPTION_USAGE: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";

fn assert_nested_switch_client_error(output: &Output) {
    let stderr = stderr(output);
    assert!(
        stderr.contains("switch-client requires an attached client")
            || stderr.contains("can't find client: 1")
            || stderr.contains("no current client"),
        "stderr={stderr:?}"
    );
}

fn list_command_names(rendered: &str) -> Vec<String> {
    rendered
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(ToOwned::to_owned))
        .collect()
}

fn assert_absent_server_error(output: &Output, harness: &CliHarness, command_name: &str) {
    assert!(
        stderr(output).contains(&format!(
            "no server running on {}",
            harness.socket_path().display()
        )),
        "{command_name} stderr should report absent server, got: {}",
        stderr(output)
    );
}

#[test]
fn named_socket_absent_server_keeps_connect_error_surface() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("named-socket-no-server")?;
    let output = harness.run(&["-L", "named", "list-sessions"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("error connecting to "),
        "named sockets should keep connect errors, got: {}",
        stderr(&output)
    );
    assert!(
        !stderr(&output).contains("(os error "),
        "named socket absent errors should match tmux's strerror-only shape, got: {}",
        stderr(&output)
    );
    assert!(
        !stderr(&output).contains("no server running on "),
        "named sockets should not use the default-socket absent server wording, got: {}",
        stderr(&output)
    );
    Ok(())
}

#[test]
fn version_flag_reports_rmux_version_without_server_contact() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("version-flag")?;
    let output = harness.run(&["-V"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output).trim(),
        format!("rmux {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(stderr(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn top_level_long_options_match_tmux_usage_errors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("top-level-long-usage-errors")?;

    for args in [&["--help"][..], &["--version"][..], &["--vesion"][..]] {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(1));
        assert!(stdout(&output).is_empty());
        assert_eq!(stderr(&output), LONG_OPTION_USAGE);
        assert!(!harness.socket_path().exists());
    }

    Ok(())
}

#[test]
fn single_dash_help_exits_zero_with_usage() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("single-dash-help")?;
    let output = harness.run(&["-h"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), TOP_LEVEL_USAGE);
    assert!(stderr(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_commands_is_client_local_and_supports_formatting() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-commands-client-local")?;

    let all_commands = harness.run(&["list-commands"])?;
    assert_eq!(all_commands.status.code(), Some(0));
    assert!(stdout(&all_commands).contains("list-commands (lscm) [-F format] [command]"));
    assert!(stdout(&all_commands).contains("choose-tree"));
    assert!(stdout(&all_commands).contains("link-window"));
    assert!(stdout(&all_commands).contains("unlink-window"));
    assert!(stdout(&all_commands).contains("set-window-option (setw)"));
    assert!(stdout(&all_commands).contains("show-window-options (showw)"));
    assert!(stdout(&all_commands).contains("display-menu (menu)"));
    assert!(stdout(&all_commands).contains("display-popup (popup)"));
    assert!(stdout(&all_commands).contains("clear-prompt-history (clearphist)"));
    assert!(stdout(&all_commands).contains("show-prompt-history (showphist)"));
    assert!(stderr(&all_commands).is_empty());

    let filtered = harness.run(&[
        "list-commands",
        "-F",
        "#{command_name}=#{command_alias}",
        "lscm",
    ])?;
    assert_eq!(filtered.status.code(), Some(0));
    assert_eq!(stdout(&filtered).trim(), "list-commands=lscm");
    assert!(stderr(&filtered).is_empty());

    let choose_alias = harness.run(&["list-commands", "-F", "#{command_name}", "choose-window"])?;
    assert_eq!(choose_alias.status.code(), Some(0));
    assert_eq!(stdout(&choose_alias).trim(), "choose-tree");
    assert!(stderr(&choose_alias).is_empty());

    let window_alias = harness.run(&[
        "list-commands",
        "-F",
        "#{command_name}=#{command_alias}",
        "showw",
    ])?;
    assert_eq!(window_alias.status.code(), Some(0));
    assert_eq!(stdout(&window_alias).trim(), "show-window-options=showw");
    assert!(stderr(&window_alias).is_empty());

    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_keys_uses_default_table_without_server() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-keys-defaults-without-server")?;
    let output = harness.run(&["list-keys", "-T", "prefix"])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("bind-key    -T prefix Space   next-layout"));
    assert!(stdout(&output).contains("bind-key    -T prefix q       display-panes"));
    assert!(stdout(&output).contains("bind-key    -T prefix M-5     select-layout tiled"));
    assert!(!stdout(&output).contains("bind-key    -T prefix M-6"));
    assert!(!stdout(&output).contains("bind-key    -T prefix M-7"));
    assert!(stderr(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn help_and_list_commands_cover_the_full_tmux_command_table() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("full-command-surface")?;
    let list = harness.run(&["list-commands"])?;
    let expected = COMMAND_TABLE
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect::<Vec<_>>();

    assert_eq!(list.status.code(), Some(0));
    assert_eq!(list_command_names(&stdout(&list)), expected);
    assert!(stderr(&list).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_commands_rejects_unsupported_and_ambiguous_filters_locally() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-commands-filter-errors")?;

    let ambiguous = harness.run(&["list-commands", "list"])?;
    assert_eq!(ambiguous.status.code(), Some(1));
    assert!(stdout(&ambiguous).is_empty());
    assert!(stderr(&ambiguous).contains("ambiguous command: list, could be:"));

    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn command_help_uses_double_dash_while_short_h_keeps_tmux_semantics() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("command-double-dash-help")?;

    for command in [
        ["command-prompt", "--help"].as_slice(),
        ["choose-tree", "--help"].as_slice(),
        ["set-window-option", "--help"].as_slice(),
        ["show-window-options", "--help"].as_slice(),
    ] {
        let output = harness.run(command)?;
        let rendered = format!("{}{}", stdout(&output), stderr(&output));
        assert_eq!(output.status.code(), Some(0));
        assert!(rendered.contains("Usage:"));
        assert!(!harness.socket_path().exists());
    }

    let split_help = harness.run(&["split-window", "--help"])?;
    let split_rendered = format!("{}{}", stdout(&split_help), stderr(&split_help));
    assert_eq!(split_help.status.code(), Some(0));
    assert!(split_rendered.contains("-h"));
    assert!(split_rendered.contains("-v"));

    let split_horizontal = harness.run(&["split-window", "-h", "-t", "alpha"])?;
    assert_eq!(split_horizontal.status.code(), Some(1));
    assert_absent_server_error(&split_horizontal, &harness, "split-window");
    assert!(stdout(&split_horizontal).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn invalid_top_level_cluster_with_h_does_not_exit_successfully() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("invalid-top-level-h-cluster")?;

    let output = harness.run(&["-xh"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!("rmux: unknown option -- x\n{TOP_LEVEL_USAGE}")
    );
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn long_top_level_flag_with_h_does_not_exit_successfully() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("invalid-top-level-long-h")?;

    let output = harness.run(&["--not-a-tmux-flag", "-h"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), LONG_OPTION_USAGE);
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn no_start_server_suppresses_new_session_auto_start() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("no-start-server")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["-N", "new-session", "-d", "-s", "alpha"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "new-session");
    assert!(stdout(&output).is_empty());
    assert!(
        !harness.pid_path().exists(),
        "-N must not launch the daemon"
    );
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn no_start_server_suppresses_attach_session_auto_start() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("no-start-server-attach")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["-N", "attach-session", "-t", "alpha"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "attach-session");
    assert!(stdout(&output).is_empty());
    assert!(
        !harness.pid_path().exists(),
        "-N must not launch the daemon"
    );
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn no_start_server_suppresses_start_server_auto_start() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("no-start-server-start")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["-N", "start-server"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "start-server");
    assert!(stdout(&output).is_empty());
    assert!(
        !harness.pid_path().exists(),
        "-N must not launch the daemon"
    );
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn start_server_is_a_start_server_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("start-server-command")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["start-server"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_success(&output);
    assert!(harness.pid_path().exists());
    assert!(harness.socket_path().exists());
    Ok(())
}

#[test]
fn hidden_daemon_binary_override_is_ignored_without_test_opt_in() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("start-server-ignore-override")?;
    let marker_path = harness.tmpdir().join("override-marker");
    let script_path = harness.tmpdir().join("override.sh");
    write_marker_script(&script_path, &marker_path)?;

    let output = harness.run_with(&["start-server"], |command| {
        command.env(BINARY_OVERRIDE_ENV, &script_path);
        command.env_remove(BINARY_OVERRIDE_TEST_OPT_IN_ENV);
    })?;

    assert_success(&output);
    assert!(
        harness.socket_path().exists(),
        "rmux should still auto-start its own daemon"
    );
    assert!(
        !marker_path.exists(),
        "the undocumented override must be ignored without the test-only opt-in"
    );
    assert_success(&harness.run(&["kill-server"])?);
    Ok(())
}

#[test]
fn kill_server_shuts_down_daemon_and_cleans_socket() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("kill-server-cleanup")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["kill-server"])?;
    assert_success(&output);
    wait_for_socket_cleanup(harness.socket_path())?;

    let _ = daemon.child_mut().wait();
    Ok(())
}

#[test]
fn server_access_list_succeeds_against_running_server() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("server-access-list")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["server-access", "-l", "ignored-user"])?;

    assert_eq!(output.status.code(), Some(0));
    if !stdout(&output).is_empty() {
        assert!(stdout(&output).contains(" (W)"));
    }
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn server_access_missing_user_is_reported_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("server-access-missing-user")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["server-access", "-r"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "missing user argument\n");
    Ok(())
}

#[test]
fn server_access_target_flag_reports_tmux_unknown_flag() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("server-access-target-flag")?;

    let output = harness.run(&["server-access", "-t", "%0", "-l"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "command server-access: unknown flag -t\n");
    Ok(())
}

#[test]
fn current_target_commands_accept_tmux_style_implicit_defaults() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("implicit-current-cli")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for args in [
        &["select-pane"][..],
        &["resize-pane"][..],
        &["select-layout"][..],
    ] {
        let output = harness.run(args)?;
        assert_success(&output);
    }
    for args in [
        &["show-options"][..],
        &["show-window-options"][..],
        &["show-environment"][..],
        &["show-hooks"][..],
    ] {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
    }

    assert_success(&harness.run(&["break-pane"])?);
    let windows = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_index}"])?;
    assert_eq!(windows.status.code(), Some(0));
    assert!(stderr(&windows).is_empty());
    assert_eq!(stdout(&windows).lines().count(), 1);
    Ok(())
}

#[test]
fn attach_session_is_a_start_server_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-start-server")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["attach-session", "-t", "alpha"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output).trim(), "no sessions");
    assert!(harness.pid_path().exists());
    wait_for_socket_cleanup(harness.socket_path())?;
    Ok(())
}

#[test]
fn non_start_server_command_does_not_auto_start() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-sessions-no-start")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["list-sessions"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "list-sessions");
    assert!(stdout(&output).is_empty());
    assert!(
        !harness.pid_path().exists(),
        "list-sessions must not launch the daemon"
    );
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn no_fork_without_command_runs_server_in_the_foreground() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("no-fork-foreground")?;
    let mut child = harness
        .base_command()
        .arg("-D")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    wait_for_socket(harness.socket_path(), &mut child)?;
    assert!(
        child.try_wait()?.is_none(),
        "-D server should remain foreground"
    );
    terminate_child(&mut child)?;
    Ok(())
}

#[test]
fn no_fork_rejects_an_explicit_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("no-fork-with-command")?;
    let output = harness.run(&["-D", "new-session", "-d"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("usage: rmux"));
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn shell_command_rejects_an_explicit_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("shell-command-conflict")?;
    let output = harness.run(&["-c", "echo hi", "list-sessions"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("usage: rmux"));
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn shell_command_starts_the_server_and_returns_the_shell_exit_status() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("shell-command-startup")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["-c", "printf startup-shell; exit 23"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(stdout(&output), "startup-shell");
    assert!(stderr(&output).is_empty());
    assert!(
        harness.pid_path().exists(),
        "-c shell-command startup must launch the hidden daemon when the server is absent"
    );
    assert!(
        harness.socket_path().exists(),
        "-c shell-command startup must leave the auto-started server socket behind"
    );
    Ok(())
}

#[test]
fn control_mode_uses_tmux_text_protocol() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-mode-protocol")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut child = harness
        .base_command()
        .arg("-CC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("control stdin");
    let stdout = child.stdout.take().expect("control stdout");
    let stderr = child.stderr.take().expect("control stderr");
    let (stdout_buffer, stdout_thread) = spawn_pipe_collector(stdout);
    let (_stderr_buffer, stderr_thread) = spawn_pipe_collector(stderr);

    stdin.write_all(b"list-sessions\nbad-command\nattach-session -t alpha\n")?;
    stdin.flush()?;
    wait_for_output_condition(
        &stdout_buffer,
        ATTACH_TIMEOUT,
        "two %end guards and one %error guard",
        |rendered| {
            rendered.matches("%end ").count() >= 2 && rendered.matches("%error ").count() >= 1
        },
    )?;

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf control-mode-output",
        "Enter",
    ])?);

    wait_for_output_condition(
        &stdout_buffer,
        ATTACH_TIMEOUT,
        "framed pane output",
        |rendered| rendered.contains("%output %") && rendered.contains("control-mode-output"),
    )?;
    stdin.write_all(b"\n")?;
    drop(stdin);

    let status = child.wait()?;
    let rendered = String::from_utf8(read_pipe_output(stdout_thread, "stdout")?)?;
    let stderr = String::from_utf8(read_pipe_output(stderr_thread, "stderr")?)?;

    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());

    assert!(rendered.starts_with(CONTROL_CONTROL_START));
    assert!(rendered.contains("%begin "));
    assert!(rendered.contains("%end "));
    assert!(rendered.contains("%error "));
    assert!(rendered.contains("parse error:"));
    assert!(rendered.contains("bad-command"));
    assert!(rendered.contains("alpha"));
    assert!(rendered.contains("%output %"));
    assert!(rendered.contains("control-mode-output"));
    assert!(rendered.contains("%exit"));
    assert!(rendered.ends_with(CONTROL_CONTROL_END));
    Ok(())
}

#[test]
fn unsupported_subcommands_exit_one() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("unsupported-subcommand")?;
    let output = harness.run(&["bogus-command"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "bogus-command");
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn sanitized_session_names_allow_new_session_auto_start() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sanitized-session-name")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let output = harness.run_with(&["new-session", "-d", "-s", "bad:name"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_success(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    assert!(harness.pid_path().exists(), "auto-start must run");
    assert!(
        harness.socket_path().exists(),
        "sanitized names create a socket"
    );
    assert_success(&harness.run(&["has-session", "-t", "bad_name"])?);
    Ok(())
}

#[test]
fn new_session_detached_auto_starts_and_then_has_session_succeeds() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-auto-start")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let create = harness.run_with(&["new-session", "-d", "-s", "alpha"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;
    assert_success(&create);

    let has = harness.run(&["has-session", "-t", "alpha"])?;
    assert_success(&has);
    assert!(harness.socket_path().exists());
    Ok(())
}

#[test]
fn new_session_start_directory_sets_initial_pane_path() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-start-directory")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let start_dir = harness.tmpdir().join("start-dir");
    fs::create_dir_all(&start_dir)?;
    let start_dir_text = start_dir.to_string_lossy().to_string();

    let create = harness.run_with(
        &["new-session", "-d", "-s", "alpha", "-c", &start_dir_text],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&create);

    let cwd = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{pane_current_path}",
    ])?;
    assert_eq!(
        cwd.status.code(),
        Some(0),
        "display-message should succeed, stderr={}",
        stderr(&cwd)
    );
    assert!(stderr(&cwd).is_empty());
    let expected_start_dir = fs::canonicalize(&start_dir)?.to_string_lossy().to_string();
    assert_eq!(stdout(&cwd).trim(), expected_start_dir);
    Ok(())
}

#[test]
fn new_session_trailing_shell_command_spawns_initial_pane_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-shell-command")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let create = harness.run_with(
        &["new-session", "-d", "-s", "alpha", "sleep 30"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&create);

    let current = harness.run(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.0",
        "#{pane_current_command}",
    ])?;
    assert_eq!(current.status.code(), Some(0));
    assert_eq!(stdout(&current), "sleep\n");
    assert!(stderr(&current).is_empty());

    Ok(())
}

#[test]
fn new_session_uses_shell_env_when_default_shell_is_unset() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-shell-env")?;
    let _daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("shell.txt");
    let expected_shell = "/bin/sh";
    let shell_env = format!("SHELL={expected_shell}");
    let shell_command = format!("printf '%s' \"$SHELL\" > {}", shell_quote(&output_path));

    let clear_default_shell = harness.run(&["set-option", "-g", "default-shell", ""])?;
    assert_success(&clear_default_shell);

    let create = harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-e",
        &shell_env,
        &shell_command,
    ])?;
    assert_success(&create);

    wait_for_file_contents(&output_path, expected_shell, ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn has_session_reports_absent_server_when_the_server_is_absent() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("has-session-absent")?;
    let output = harness.run(&["has-session", "-t", "alpha"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_absent_server_error(&output, &harness, "has-session");
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn kill_session_reports_absent_server_when_the_server_is_absent() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("kill-session-absent")?;
    let output = harness.run(&["kill-session", "-t", "alpha"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_absent_server_error(&output, &harness, "kill-session");
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn queued_prompt_history_commands_use_source_file_dispatch_and_preserve_cli_contract(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("queued-prompt-history-dispatch")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let shown = harness.run(&["show-prompt-history"])?;
    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(
        stdout(&shown),
        "History for command:\n\n\nHistory for search:\n\n\nHistory for target:\n\n\nHistory for window-target:\n\n\n"
    );
    assert!(stderr(&shown).is_empty());

    let cleared = harness.run(&["clear-prompt-history", "-T", "search"])?;
    assert_eq!(cleared.status.code(), Some(0));
    assert!(stdout(&cleared).is_empty());
    assert!(stderr(&cleared).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn rmux_environment_default_socket_is_used_when_no_socket_flag_is_given(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("rmux-env-default-socket")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let rmux_env = format!("{},1,0", harness.socket_path().display());

    let output = harness.run_with(&["has-session", "-t", "alpha"], |command| {
        command.env("RMUX", &rmux_env);
    })?;

    assert_success(&output);
    Ok(())
}

#[test]
fn rmux_environment_socket_is_used_when_no_socket_flag_is_given() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("rmux-env-socket")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let rmux_socket = harness.tmpdir().join("rmux-1000").join("absent.sock");
    let rmux_env = format!("{},1,0", rmux_socket.display());

    let output = harness.run_with(&["has-session", "-t", "alpha"], |command| {
        command.env("RMUX", &rmux_env);
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("error connecting to "),
        "RMUX socket environment should keep explicit-socket connect diagnostics, got: {}",
        stderr(&output)
    );
    Ok(())
}

#[test]
fn socket_path_flag_overrides_socket_name_and_rmux_environment() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-path-override")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let rmux_env = format!("{},1,0", harness.tmpdir().join("rmux-env.sock").display());

    let output = harness.run_with(
        &[
            "-L",
            "ignored-name",
            "-S",
            harness
                .socket_path()
                .to_str()
                .expect("utf-8 harness socket path"),
            "has-session",
            "-t",
            "alpha",
        ],
        |command| {
            command.env("RMUX", &rmux_env);
        },
    )?;

    assert_success(&output);
    Ok(())
}

#[test]
fn socket_name_flag_uses_named_socket_under_tmux_uid_directory() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-name")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let named_socket = harness
        .socket_path()
        .parent()
        .expect("default socket parent")
        .join("named");

    let created = harness.run_with(
        &["-L", "named", "new-session", "-d", "-s", "alpha"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&created);
    assert!(named_socket.exists());

    let default_socket = harness.run(&["has-session", "-t", "alpha"])?;
    assert_eq!(default_socket.status.code(), Some(1));

    let named_socket_output = harness.run(&["-L", "named", "has-session", "-t", "alpha"])?;
    assert_success(&named_socket_output);
    let _ = fs::remove_file(named_socket);
    Ok(())
}

#[test]
fn switch_client_reports_absent_server_without_autostart() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("switch-client-outside")?;
    let output = harness.run(&["switch-client", "-t", "alpha"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "switch-client");
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn attach_session_inside_tmux_uses_switch_client_semantics() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-session-nested")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let rmux_env = format!("{},1,0", harness.socket_path().display());
    let output = harness.run_with(&["attach-session", "-t", "alpha"], |command| {
        command.env("RMUX", &rmux_env);
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_nested_switch_client_error(&output);
    assert!(!stderr(&output).contains("attach error"));
    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn attach_session_inside_tmux_rejects_unavailable_attach_only_flags_before_connecting(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-session-nested-validation")?;
    let rmux_env = format!("{},1,0", harness.socket_path().display());

    for (args, expected) in [
        (
            &[
                "attach-session",
                "-c",
                "/tmp",
                "-d",
                "-f",
                "active-pane",
                "-r",
                "-x",
                "-t",
                "alpha",
            ][..],
            "unsupported: -c, -d, -f, -r, -x",
        ),
        (&["attach-session"][..], "requires -t"),
    ] {
        let output = harness.run_with(args, |command| {
            command.env("RMUX", &rmux_env);
        })?;

        assert_eq!(output.status.code(), Some(1));
        assert!(stderr(&output).contains("attach-session inside an attached client"));
        assert!(stderr(&output).contains(expected));
        assert!(stdout(&output).is_empty());
        assert!(!harness.socket_path().exists());
    }

    Ok(())
}

#[test]
fn switch_client_can_control_the_sole_active_attach_from_another_process(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("switch-client-cross-process")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(NONBLOCKING_ATTACH_TIMEOUT)?;

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf alpha-output",
        "Enter",
    ])?);
    let alpha_output = read_until_contains(attach.master_mut(), "alpha-output", ATTACH_TIMEOUT)?;
    assert!(alpha_output.contains("alpha-output"));

    let rmux_env = format!("{},1,0", harness.socket_path().display());
    let switched = harness.run_with(&["switch-client", "-t", "beta"], |command| {
        command.env("RMUX", &rmux_env);
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
fn detach_client_can_control_the_sole_active_attach_from_another_process(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("detach-client-cross-process")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(NONBLOCKING_ATTACH_TIMEOUT)?;

    let detached = harness.run(&["detach-client"])?;
    assert_success(&detached);

    let status = attach.wait_for_exit(ATTACH_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    Ok(())
}

#[test]
fn new_session_without_detach_creates_then_attempts_nested_switch() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-nested-switch")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let rmux_env = format!("{},1,0", harness.socket_path().display());
    let output = harness.run_with(&["new-session", "-s", "alpha"], |command| {
        command.env("RMUX", &rmux_env);
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_nested_switch_client_error(&output);

    let has = harness.run(&["has-session", "-t", "alpha"])?;
    assert_success(&has);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_option_append_without_server_matches_tmux_connect_surface() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-option-append-validation")?;
    let output = harness.run(&["set-option", "-a", "-g", "status", "off"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, "set-option");
    assert!(stdout(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn quiet_option_commands_suppress_unknown_option_errors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("quiet-option-unknown")?;
    let _daemon = harness.start_hidden_daemon()?;

    let set = harness.run(&[
        "set-option",
        "-q",
        "-g",
        "definitely-not-real-option",
        "foo",
    ])?;
    assert_success(&set);
    assert!(stdout(&set).is_empty());
    assert!(stderr(&set).is_empty());

    let show = harness.run(&["show-options", "-q", "-g", "definitely-not-real-option"])?;
    assert_success(&show);
    assert!(stdout(&show).is_empty());
    assert!(stderr(&show).is_empty());

    let target_error = harness.run(&["show-options", "-q", "-t", "missing", "status"])?;
    assert_eq!(target_error.status.code(), Some(1));
    let target_stderr = stderr(&target_error);
    assert!(
        target_stderr.contains("can't find session: missing")
            || target_stderr.contains("session not found: missing"),
        "quiet option lookup should not suppress target errors, got: {}",
        target_stderr
    );
    Ok(())
}

#[test]
fn default_terminal_target_shape_sets_term_for_future_panes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("default-terminal-target-shape")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("pane-term.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-option", "-s", "default-terminal", "tmux-256color"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        &format!("printf \"$TERM\" > {}", shell_quote(&output_path)),
        "Enter",
    ])?);

    wait_for_file_contents(&output_path, "tmux-256color", ATTACH_TIMEOUT)?;
    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn terminal_features_append_short_flag_shape_succeeds() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("terminal-features-append-shape")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-option",
        "-as",
        "terminal-features",
        WORKFLOW_TRUECOLOR_FEATURES,
    ])?);

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn self_unsetting_hook_payload_runs_once_across_repeated_attaches() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("self-unsetting-hook-payload")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let hook_path = harness.tmpdir().join("client-attached.txt");
    let hook_command = format!(
        "mkdir -p {} && printf 'attached\\n' > {}",
        shell_quote(hook_path.parent().expect("hook path parent")),
        shell_quote(&hook_path),
    );
    let payload = format!(
        "run-shell {}; set-hook -u -t alpha client-attached",
        shell_quote_str(&hook_command)
    );

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-t",
        "alpha",
        "client-attached",
        payload.as_str(),
    ])?);

    attach_then_detach(&harness, "alpha")?;
    wait_for_file_contents(&hook_path, "attached\n", ATTACH_TIMEOUT)?;

    attach_then_detach(&harness, "alpha")?;
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::read_to_string(&hook_path)?, "attached\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_session_without_session_name_uses_default_numeric_name() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("default-session-name")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(&["new-session", "-d"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;

    assert_success(&output);
    assert_success(&harness.run(&["has-session", "-t", "0"])?);
    Ok(())
}

#[test]
fn command_free_invocation_routes_to_default_new_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("command-free-default")?;
    let _daemon = harness.start_hidden_daemon()?;
    let rmux_env = format!("{},1,0", harness.socket_path().display());

    let output = harness.run_with(&[], |command| {
        command.env("RMUX", &rmux_env);
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_nested_switch_client_error(&output);
    assert_success(&harness.run(&["has-session", "-t", "0"])?);
    Ok(())
}

#[test]
fn command_free_invocation_auto_starts_default_new_session() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("command-free-auto-start")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let rmux_env = format!("{},1,0", harness.socket_path().display());

    let output = harness.run_with(&[], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        command.env("RMUX", &rmux_env);
    })?;

    assert_eq!(output.status.code(), Some(1));
    assert_nested_switch_client_error(&output);
    assert!(harness.pid_path().exists());
    assert!(harness.socket_path().exists());
    assert_success(&harness.run(&["has-session", "-t", "0"])?);
    Ok(())
}

#[test]
fn has_session_sanitizes_dot_names_before_lookup() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sanitized-dot-session")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "bad_name"])?);

    let output = harness.run(&["has-session", "-t", "bad.name"])?;
    assert_success(&output);

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn send_keys_without_keys_succeeds() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("send-keys-no-keys")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["send-keys", "-t", "alpha:0.0"])?;
    assert_success(&output);

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_option_scope_conflict_exits_one() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-option-scope-conflict")?;
    let output = harness.run(&["set-option", "-s", "-w", "status", "off"])?;

    assert_clap_failure(&output);
    Ok(())
}

#[test]
fn window_option_commands_round_trip_with_explicit_window_targets() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("window-option-command-surface")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let toggled = harness.run(&["set-option", "-w", "-t", "alpha", "synchronize-panes"])?;
    assert_success(&toggled);

    let show_toggled = harness.run(&["show-options", "-wv", "-t", "alpha", "synchronize-panes"])?;
    assert_eq!(show_toggled.status.code(), Some(0));
    assert_eq!(stdout(&show_toggled), "on\n");
    assert!(stderr(&show_toggled).is_empty());

    let set_window = harness.run(&[
        "set-window-option",
        "-t",
        "alpha",
        "pane-border-style",
        "fg=colour1",
    ])?;
    assert_success(&set_window);

    let show_window = harness.run(&[
        "show-window-options",
        "-v",
        "-t",
        "alpha",
        "pane-border-style",
    ])?;
    assert_eq!(show_window.status.code(), Some(0));
    assert_eq!(stdout(&show_window), "fg=colour1\n");
    assert!(stderr(&show_window).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn set_option_without_target_uses_current_scope_not_global() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-option-current-scope")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);

    assert_success(&harness.run(&["set-option", "status", "off"])?);
    let alpha_status = harness.run(&["show-options", "-v", "-t", "alpha", "status"])?;
    assert_eq!(stdout(&alpha_status), "on\n");
    let beta_status = harness.run(&["show-options", "-v", "-t", "beta", "status"])?;
    assert_eq!(stdout(&beta_status), "off\n");

    assert_success(&harness.run(&["set-option", "mode-keys", "vi"])?);
    let alpha_mode = harness.run(&["show-options", "-wv", "-t", "alpha", "mode-keys"])?;
    assert_eq!(stdout(&alpha_mode), "emacs\n");
    let beta_mode = harness.run(&["show-options", "-wv", "-t", "beta", "mode-keys"])?;
    assert_eq!(stdout(&beta_mode), "vi\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn show_option_global_compatibility_shapes_ignore_targets_like_tmux() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("show-option-global-compat-shapes")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-option", "-s", "message-limit", "77"])?);
    let show_server = harness.run(&["show-options", "-gsv", "-t", "missing", "message-limit"])?;
    assert_eq!(show_server.status.code(), Some(0));
    assert_eq!(stdout(&show_server), "77\n");
    assert!(stderr(&show_server).is_empty());

    assert_success(&harness.run(&[
        "set-window-option",
        "-g",
        "pane-border-style",
        "fg=colour3",
    ])?);
    let show_window = harness.run(&[
        "show-window-options",
        "-g",
        "-t",
        "missing",
        "-v",
        "pane-border-style",
    ])?;
    assert_eq!(show_window.status.code(), Some(0));
    assert_eq!(stdout(&show_window), "fg=colour3\n");
    assert!(stderr(&show_window).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn window_option_commands_surface_command_name_in_scope_errors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("window-option-scope-error-names")?;

    let show_no_scope = harness.run(&["show-window-options"])?;
    assert_eq!(show_no_scope.status.code(), Some(1));
    assert!(stdout(&show_no_scope).is_empty());
    assert_absent_server_error(&show_no_scope, &harness, "show-window-options");
    assert!(!harness.socket_path().exists());

    let show_options_no_scope = harness.run(&["show-options"])?;
    assert_eq!(show_options_no_scope.status.code(), Some(1));
    assert_absent_server_error(&show_options_no_scope, &harness, "show-options");
    assert!(!harness.socket_path().exists());

    let show_options_w_no_target = harness.run(&["show-options", "-w"])?;
    assert_eq!(show_options_w_no_target.status.code(), Some(1));
    assert!(
        stderr(&show_options_w_no_target).contains("show-options -w requires a target"),
        "show-options -w without target should prompt for a target, got: {}",
        stderr(&show_options_w_no_target)
    );
    assert!(!harness.socket_path().exists());

    let show_options_p_without_pane = harness.run(&["show-options", "-p", "-t", "alpha"])?;
    assert_eq!(show_options_p_without_pane.status.code(), Some(1));
    assert_absent_server_error(&show_options_p_without_pane, &harness, "show-options");
    assert!(!harness.socket_path().exists());

    Ok(())
}

#[test]
fn simple_commands_report_absent_server_on_stderr() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("absent-server-stderr")?;

    for &(command, args) in &[
        ("rename-session", &["-t", "alpha", "beta"] as &[&str]),
        ("new-window", &["-t", "alpha"] as &[&str]),
        ("kill-window", &["-t", "alpha:0"]),
        ("select-window", &["-t", "alpha:0"]),
        ("rename-window", &["-t", "alpha:0", "renamed"]),
        ("next-window", &["-t", "alpha"]),
        ("previous-window", &["-t", "alpha"]),
        ("last-window", &["-t", "alpha"]),
        ("has-session", &[]),
        ("kill-session", &[]),
        ("list-sessions", &[]),
        ("list-windows", &["-t", "alpha"]),
        ("move-window", &["-s", "alpha:0", "-t", "alpha:1"]),
        ("swap-window", &["-s", "alpha:0", "-t", "alpha:1"]),
        ("rotate-window", &["-t", "alpha:0"]),
        ("split-window", &["-v", "-t", "alpha"] as &[&str]),
        ("select-layout", &["-t", "alpha:0", "main-vertical"]),
        ("next-layout", &["-t", "alpha:0"]),
        ("previous-layout", &["-t", "alpha:0"]),
        ("resize-pane", &["-t", "alpha:0.0", "-x", "34"]),
        ("resize-pane", &["-x", "notnum"]),
        ("display-message", &["-t", "alpha", "hello"]),
        ("list-panes", &["-t", "alpha"]),
        ("select-pane", &["-t", "alpha:0.0"]),
        ("send-keys", &["-t", "alpha:0.0", "echo"]),
        ("server-access", &["-l"]),
        ("lock-server", &[]),
        ("lock-session", &["-t", "alpha"]),
        ("lock-client", &["-t", "="]),
        ("kill-server", &[]),
        ("set-option", &["-g", "status", "off"]),
        (
            "set-window-option",
            &["-t", "alpha:0", "pane-border-style", "fg=colour1"],
        ),
        ("set-environment", &["-g", "TERM", "screen"]),
        ("set-hook", &["-g", "client-attached", "true"]),
        ("show-window-options", &["-t", "alpha:0"]),
    ] {
        let mut full_args = vec![command];
        full_args.extend_from_slice(args);
        let output = harness.run(&full_args)?;

        assert_eq!(
            output.status.code(),
            Some(1),
            "{command} should exit 1 on absent server"
        );
        assert_absent_server_error(&output, &harness, command);
        assert!(
            stdout(&output).is_empty(),
            "{command} should produce no stdout"
        );
    }

    Ok(())
}

#[test]
fn detach_client_rejects_unexpected_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("detach-client-extra-args")?;
    let output = harness.run(&["detach-client", "something"])?;

    assert_clap_failure(&output);
    assert!(stderr(&output).contains("unexpected"));
    Ok(())
}

#[test]
fn kill_session_reports_missing_sessions_on_running_server() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("kill-nonexistent")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["kill-session", "-t", "never-created"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("session not found: never-created"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn has_session_is_silent_for_nonexistent_session_on_running_server() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("has-nonexistent")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["has-session", "-t", "never-created"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("can't find session: never-created"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn new_session_with_partial_terminal_size() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("partial-term-size")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run_with(
        &["new-session", "-d", "-s", "alpha", "-x", "200"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&output);

    assert_success(&harness.run(&["has-session", "-t", "alpha"])?);
    Ok(())
}

#[test]
fn help_exits_one_with_tmux_usage() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("help-exit-code")?;
    let output = harness.run(&["--help"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), LONG_OPTION_USAGE);
    Ok(())
}

fn attach_then_detach(harness: &CliHarness, session: &str) -> Result<(), Box<dyn Error>> {
    let mut attach = AttachedSession::spawn(harness, session, TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(NONBLOCKING_ATTACH_TIMEOUT)?;
    assert_success(&harness.run(&["detach-client"])?);
    let status = attach.wait_for_exit(ATTACH_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    Ok(())
}

fn wait_for_socket_cleanup(socket_path: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;

    while Instant::now() < deadline {
        if !socket_path.exists() {
            assert_socket_directory_empty(socket_path)?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }

    Err(format!(
        "timed out waiting for '{}' to be removed",
        socket_path.display()
    )
    .into())
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

    Err(format!(
        "timed out waiting for '{}' to contain '{}'",
        path.display(),
        expected
    )
    .into())
}

fn spawn_pipe_collector<R>(mut reader: R) -> (SharedPipeBuffer, PipeCollector)
where
    R: Read + Send + 'static,
{
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mirror = Arc::clone(&shared);
    let handle = thread::spawn(move || -> io::Result<Vec<u8>> {
        let mut collected = Vec::new();
        let mut chunk = [0_u8; 4096];

        loop {
            match reader.read(&mut chunk) {
                Ok(0) => return Ok(collected),
                Ok(count) => {
                    collected.extend_from_slice(&chunk[..count]);
                    mirror
                        .lock()
                        .expect("control output mirror lock")
                        .extend_from_slice(&chunk[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    });

    (shared, handle)
}

fn read_pipe_output(handle: PipeCollector, label: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = handle
        .join()
        .map_err(|_| format!("{label} collector thread panicked"))??;
    Ok(output)
}

fn wait_for_output_condition<F>(
    buffer: &SharedPipeBuffer,
    timeout: Duration,
    description: &str,
    predicate: F,
) -> Result<(), Box<dyn Error>>
where
    F: Fn(&str) -> bool,
{
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        let snapshot = {
            let bytes = buffer.lock().expect("control output lock");
            String::from_utf8_lossy(&bytes).into_owned()
        };
        if predicate(&snapshot) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }

    let snapshot = {
        let bytes = buffer.lock().expect("control output lock");
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Err(format!("timed out waiting for {description} in control output: {snapshot:?}").into())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn shell_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn write_marker_script(path: &Path, marker_path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf redirected > '{}'\nexit 0\n",
            marker_path.display()
        ),
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}
