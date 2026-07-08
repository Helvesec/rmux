#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use common::acquire_empty_socket_path_lock;
use common::{
    assert_clap_failure, assert_socket_directory_empty, assert_success, read_until_contains,
    stderr, stdout, terminate_child, wait_for_socket, AttachedSession, CliHarness,
    BINARY_OVERRIDE_ENV, BINARY_OVERRIDE_TEST_OPT_IN_ENV,
};
use rmux_client::INTERNAL_DAEMON_FLAG;
use rmux_core::command_parser::COMMAND_TABLE;
use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, Request, Response, RmuxError, CONTROL_CONTROL_END,
    CONTROL_CONTROL_START, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION,
};
use rmux_pty::TerminalSize;

const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const NONBLOCKING_ATTACH_TIMEOUT: Duration = Duration::from_millis(500);
const WORKFLOW_TRUECOLOR_FEATURES: &str =
    ",xterm-256color:RGB,tmux-256color:RGB,screen-256color:RGB,screen:RGB";
type SharedPipeBuffer = Arc<Mutex<Vec<u8>>>;
type PipeCollector = JoinHandle<io::Result<Vec<u8>>>;
const TOP_LEVEL_USAGE: &str = "usage: rmux [-2CDhlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";
const LONG_OPTION_USAGE: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n";
const LONG_OPTION_HELP: &str = "usage: rmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]\n            [-S socket-path] [-T features] [command [flags]]\n\nRMUX extensions:\n  capabilities [--human|--json]\n  claude [install-skill|claude-args...]\n  diagnose [--human|--json]\n  doctor tmux-dropin\n  setup tmux-shim\n  wait-pane [flags]\n  pane-snapshot [flags]\n  stream-pane [--raw|--lines]\n  collect-pane-output --until-pane-exit --max-bytes bytes\n  locator|expect-pane [flags]\n  find-panes|find-sessions [flags]\n  broadcast-keys -t target... -- key ...\n  with-session session-name -- command ...\n  web-share [flags]\n  web-share list|lookup|stop|disconnect|off|config\n\nUse `rmux list-commands` for the tmux-compatible command surface.\n";

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

fn spawn_incompatible_wire_server(socket_path: &Path) -> io::Result<JoinHandle<io::Result<()>>> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept()?;
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 1024];
        loop {
            match decoder.next_frame::<Request>() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(error) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, error));
                }
            }

            let bytes_read = stream.read(&mut buffer)?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "client closed before sending request",
                ));
            }
            decoder.push_bytes(&buffer[..bytes_read]);
        }

        write_legacy_incompatible_wire_response(&mut stream, 1)
    });
    Ok(handle)
}

fn write_legacy_incompatible_wire_response(
    stream: &mut impl Write,
    wire_version: u8,
) -> io::Result<()> {
    let response = Response::Error(ErrorResponse {
        error: RmuxError::UnsupportedWireVersion {
            got: RMUX_WIRE_VERSION,
            minimum: 1,
            maximum: 1,
        },
    });
    let mut frame = encode_frame(&response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if frame.get(1).copied() != Some(RMUX_WIRE_VERSION as u8) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected encoded RMUX wire envelope",
        ));
    }
    frame[1] = wire_version;
    stream.write_all(&frame)
}

fn spawn_wire_v3_kill_server(socket_path: &Path) -> io::Result<JoinHandle<io::Result<()>>> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept()?;
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 1024];
        loop {
            match decoder.next_frame::<Request>() {
                Ok(Some(Request::KillServer(_))) => break,
                Ok(Some(request)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("expected kill-server request, got {request:?}"),
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, error));
                }
            }

            let bytes_read = stream.read(&mut buffer)?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "client closed before sending first kill-server request",
                ));
            }
            decoder.push_bytes(&buffer[..bytes_read]);
        }
        write_legacy_incompatible_wire_response(&mut stream, 3)?;
        drop(stream);

        let (mut stream, _) = listener.accept()?;
        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read < 10 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "wire-v3 kill-server fallback frame was truncated",
            ));
        }
        if buffer[0] != RMUX_FRAME_MAGIC || buffer[1] != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected wire-v3 RMUX envelope, got magic={:?} version={:?}",
                    buffer.first(),
                    buffer.get(1)
                ),
            ));
        }
        let length = u32::from_le_bytes(
            buffer[2..6]
                .try_into()
                .expect("wire-v3 test frame includes fixed length"),
        );
        if length != 4 || buffer[6..10] != 72_u32.to_le_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wire-v3 fallback did not send a kill-server request",
            ));
        }
        Ok(())
    });
    Ok(handle)
}

fn spawn_alias_fallback_incompatible_wire_server(
    socket_path: &Path,
) -> io::Result<JoinHandle<io::Result<()>>> {
    spawn_incompatible_wire_server(socket_path)
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
fn incompatible_wire_error_uses_simple_default_kill_server_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("wire-incompatible-default")?;
    let server = spawn_incompatible_wire_server(harness.socket_path())?;

    let output = harness.run(&["list-sessions"])?;

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("uses an incompatible protocol"),
        "stderr should explain protocol incompatibility, got: {stderr}"
    );
    assert!(
        stderr.contains("rmux: run `rmux kill-server` to stop it, then retry."),
        "default socket should use simple kill-server command, got: {stderr}"
    );
    server
        .join()
        .expect("fake incompatible server should exit")?;
    Ok(())
}

#[test]
fn incompatible_wire_error_targets_explicit_socket_command() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("wire-incompatible-explicit")?;
    let socket_path = harness.tmpdir().join("custom.sock");
    let server = spawn_incompatible_wire_server(&socket_path)?;
    let socket_arg = socket_path.to_string_lossy().to_string();

    let output = harness.run(&["-S", &socket_arg, "list-sessions"])?;

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("uses an incompatible protocol"),
        "stderr should explain protocol incompatibility, got: {stderr}"
    );
    assert!(
        stderr.contains(&format!("rmux -S {socket_arg} kill-server")),
        "explicit socket should be preserved in command hint, got: {stderr}"
    );
    server
        .join()
        .expect("fake incompatible server should exit")?;
    Ok(())
}

#[test]
fn alias_fallback_incompatible_wire_error_keeps_socket_context() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("wire-incompatible-alias-fallback")?;
    let server = spawn_alias_fallback_incompatible_wire_server(harness.socket_path())?;

    let output = harness.run(&["not-a-command"])?;

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("uses an incompatible protocol"),
        "stderr should explain protocol incompatibility, got: {stderr}"
    );
    assert!(
        stderr.contains("rmux: run `rmux kill-server` to stop it, then retry."),
        "alias fallback should keep default socket restart guidance, got: {stderr}"
    );
    server
        .join()
        .expect("fake incompatible alias server should exit")?;
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

    let help = harness.run(&["--help"])?;
    assert_eq!(help.status.code(), Some(1));
    assert!(stdout(&help).is_empty());
    assert_eq!(stderr(&help), LONG_OPTION_HELP);
    assert!(!harness.socket_path().exists());

    for args in [&["--version"][..], &["--vesion"][..]] {
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
        "#{command_list_name}=#{command_list_alias}",
        "lscm",
    ])?;
    assert_eq!(filtered.status.code(), Some(0));
    assert_eq!(stdout(&filtered).trim(), "list-commands=lscm");
    assert!(stderr(&filtered).is_empty());

    let choose_alias = harness.run(&[
        "list-commands",
        "-F",
        "#{command_list_name}",
        "choose-window",
    ])?;
    assert_eq!(choose_alias.status.code(), Some(0));
    assert_eq!(stdout(&choose_alias).trim(), "choose-tree");
    assert!(stderr(&choose_alias).is_empty());

    let window_alias = harness.run(&[
        "list-commands",
        "-F",
        "#{command_list_name}=#{command_list_alias}",
        "showw",
    ])?;
    assert_eq!(window_alias.status.code(), Some(0));
    assert_eq!(stdout(&window_alias).trim(), "show-window-options=showw");
    assert!(stderr(&window_alias).is_empty());

    let command_fields = harness.run(&[
        "list-commands",
        "-F",
        "#{command_name}|#{command_alias}|#{command_list_name}|#{command_list_alias}|#{command_list_usage}",
        "swap-window",
    ])?;
    assert_eq!(command_fields.status.code(), Some(0));
    assert_eq!(
        stdout(&command_fields).trim(),
        "||swap-window|swapw|[-d] [-s src-window] [-t dst-window]"
    );
    assert!(stderr(&command_fields).is_empty());

    let unknown_fields = harness.run(&[
        "list-commands",
        "-F",
        "x#{bogus}y|#{command_name}|#{command_alias}|#{command_usage}|#{command_list_name}",
        "link-window",
    ])?;
    assert_eq!(unknown_fields.status.code(), Some(0));
    assert_eq!(stdout(&unknown_fields).trim(), "xy||||link-window");
    assert!(stderr(&unknown_fields).is_empty());

    let escaped_and_incomplete = harness.run(&[
        "list-commands",
        "-F",
        "##{command_list_name}|abc#{|#{command_list_name}",
        "link-window",
    ])?;
    assert_eq!(escaped_and_incomplete.status.code(), Some(0));
    assert_eq!(
        stdout(&escaped_and_incomplete).trim(),
        "#{command_list_name}|abc"
    );
    assert!(stderr(&escaped_and_incomplete).is_empty());

    let nested_incomplete = harness.run(&[
        "list-commands",
        "-F",
        "abc#{|#{command_list_name}|tail",
        "link-window",
    ])?;
    assert_eq!(nested_incomplete.status.code(), Some(0));
    assert_eq!(stdout(&nested_incomplete), "abc\n");
    assert!(stderr(&nested_incomplete).is_empty());

    let empty_unknown_field = harness.run(&[
        "list-commands",
        "-F",
        "#{definitely_unknown}",
        "link-window",
    ])?;
    assert_eq!(empty_unknown_field.status.code(), Some(0));
    assert!(stdout(&empty_unknown_field).is_empty());
    assert!(stderr(&empty_unknown_field).is_empty());

    let split_signature = harness.run(&["list-commands", "split-window"])?;
    assert_eq!(split_signature.status.code(), Some(0));
    assert_eq!(
        stdout(&split_signature),
        "split-window (splitw) [-bdefhIPvZ] [-c start-directory] [-e environment] [-F format] [-l size] [-t target-pane][shell-command]\n"
    );
    assert!(stderr(&split_signature).is_empty());

    let start_signature = harness.run(&["list-commands", "start-server"])?;
    assert_eq!(start_signature.status.code(), Some(0));
    assert_eq!(stdout(&start_signature), "start-server (start) \n");
    assert!(stderr(&start_signature).is_empty());

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
    assert!(stdout(&output)
        .contains("bind-key    -T prefix M-6     select-layout main-horizontal-mirrored"));
    assert!(stdout(&output)
        .contains("bind-key    -T prefix M-7     select-layout main-vertical-mirrored"));
    assert!(
        !stdout(&output).contains("new-pane"),
        "new-pane is deferred in the tmux-3.7 ledger and must not be advertised"
    );
    assert!(stdout(&output).contains("bind-key    -T prefix \\\"      split-window"));
    assert!(stdout(&output).contains("bind-key    -T prefix \\~      show-messages"));
    assert!(stderr(&output).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_keys_help_and_inventory_match_supported_flags() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-keys-help-inventory")?;

    let help = harness.run(&["list-keys", "--help"])?;
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).contains("-F <FORMAT>"));
    assert!(stdout(&help).contains("-O <SORT_ORDER>"));
    assert!(stdout(&help).contains("-r"));

    let inventory = harness.run(&["list-commands", "list-keys"])?;
    assert_eq!(inventory.status.code(), Some(0));
    assert!(stdout(&inventory).contains("[-1aNr]"));
    assert!(stdout(&inventory).contains("[-F format]"));
    assert!(stdout(&inventory).contains("[-O order]"));
    assert!(stderr(&inventory).is_empty());
    Ok(())
}

#[test]
fn list_keys_matches_tmux_table_errors_notes_and_first_line_alignment() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("list-keys-format-parity")?;

    let missing = harness.run(&["list-keys", "-T", "nosuch"])?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(stdout(&missing).is_empty());
    assert_eq!(stderr(&missing), "table nosuch doesn't exist\n");

    let notes = harness.run(&["list-keys", "-N"])?;
    assert_eq!(notes.status.code(), Some(0));
    assert!(stdout(&notes).contains("C-b %       Split window horizontally"));
    assert!(stdout(&notes).contains("C-b ~       Show messages"));
    assert!(!stdout(&notes).contains("C-b \\%"));
    assert!(!stdout(&notes).contains("C-b \\~"));
    assert!(stderr(&notes).is_empty());

    let first = harness.run(&["list-keys", "-1", "-T", "prefix"])?;
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(
        stdout(&first),
        "bind-key    -T prefix Space   next-layout\n"
    );
    assert!(stderr(&first).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_keys_key_filter_matches_all_tables_without_explicit_table() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-keys-key-filter")?;

    let all_tables = harness.run(&["list-keys", "C-b"])?;
    assert_eq!(all_tables.status.code(), Some(0));
    assert!(stdout(&all_tables).contains("-T copy-mode    C-b send-keys -X cursor-left"));
    assert!(stdout(&all_tables).contains("-T copy-mode-vi C-b send-keys -X page-up"));
    assert!(stdout(&all_tables).contains("-T prefix       C-b send-prefix"));
    assert!(stderr(&all_tables).is_empty());

    let absent_bound = harness.run(&["list-keys", "-T", "prefix", "d"])?;
    assert_eq!(absent_bound.status.code(), Some(0));
    assert!(stdout(&absent_bound).is_empty());
    assert!(stderr(&absent_bound).is_empty());

    let absent_unbound = harness.run(&["list-keys", "-T", "prefix", "F12"])?;
    assert_eq!(absent_unbound.status.code(), Some(0));
    assert!(stdout(&absent_unbound).is_empty());
    assert!(stderr(&absent_unbound).is_empty());

    let invalid = harness.run(&["list-keys", "no-such-key!!"])?;
    assert_eq!(invalid.status.code(), Some(1));
    assert_eq!(stderr(&invalid), "invalid key: no-such-key!!\n");

    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let connected_all_tables = harness.run(&["list-keys", "C-b"])?;
    assert_eq!(connected_all_tables.status.code(), Some(0));
    assert!(stdout(&connected_all_tables).contains("-T prefix       C-b send-prefix"));
    assert!(stderr(&connected_all_tables).is_empty());

    let connected = harness.run(&["list-keys", "-T", "prefix", "d"])?;
    assert_eq!(connected.status.code(), Some(0));
    assert!(stdout(&connected).is_empty());
    assert!(stderr(&connected).is_empty());
    Ok(())
}

#[test]
fn list_keys_bare_reverse_does_not_reverse_default_order_without_server(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-keys-bare-reverse")?;

    let default = harness.run(&["list-keys", "-T", "prefix"])?;
    assert_eq!(default.status.code(), Some(0));
    assert!(stderr(&default).is_empty());

    let bare_reverse = harness.run(&["list-keys", "-r", "-T", "prefix"])?;
    assert_eq!(bare_reverse.status.code(), Some(0));
    assert_eq!(stdout(&bare_reverse), stdout(&default));
    assert!(stderr(&bare_reverse).is_empty());
    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_keys_unknown_table_errors_with_running_server() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-keys-running-server-table-error")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let missing = harness.run(&["list-keys", "-T", "nosuch"])?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(stdout(&missing).is_empty());
    assert_eq!(stderr(&missing), "table nosuch doesn't exist\n");

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn direct_cli_command_parse_errors_do_not_use_source_file_prefix() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("direct-cli-command-errors")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let unknown = harness.run(&["not-a-command"])?;
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stdout(&unknown).is_empty());
    assert_eq!(stderr(&unknown), "unknown command: not-a-command\n");

    let ambiguous = harness.run(&["list"])?;
    assert_eq!(ambiguous.status.code(), Some(1));
    assert!(stdout(&ambiguous).is_empty());
    assert!(stderr(&ambiguous).starts_with("ambiguous command: list, could be:"));
    assert!(!stderr(&ambiguous).starts_with("-:1:"));

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn list_keys_survives_custom_modified_key_bindings() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-keys-custom-modified")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["bind-key", "C-/", "display-message", "slash"])?);
    assert_success(&harness.run(&["bind-key", "M-a", "display-message", "meta"])?);

    let output = harness.run(&["list-keys"])?;
    assert_eq!(output.status.code(), Some(0));
    let rendered = stdout(&output);
    assert!(rendered.contains("C-/"));
    assert!(rendered.contains("display-message slash"));
    assert!(rendered.contains("M-a"));
    assert!(rendered.contains("display-message meta"));
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn bind_key_canonicalizes_ctrl_bracket_as_escape() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bind-key-ctrl-bracket")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["bind-key", "C-[", "display-message", "escape"])?);
    let output = harness.run(&["list-keys"])?;
    assert_eq!(output.status.code(), Some(0));
    let rendered = stdout(&output);
    assert!(rendered.lines().any(|line| {
        line.contains("-T prefix")
            && line.contains("Escape")
            && line.ends_with("display-message escape")
    }));
    assert!(!rendered.lines().any(|line| {
        line.contains("-T prefix")
            && line.contains("C-[")
            && line.ends_with("display-message escape")
    }));
    assert!(stderr(&output).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn help_and_list_commands_cover_the_full_tmux_command_table() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("full-command-surface")?;
    let list = harness.run(&["list-commands"])?;
    // The bare `list-commands` listing is byte-compared against tmux, so it omits
    // RMUX extensions even though they stay in the command inventory for help.
    // Keep this in sync with RMUX_EXTENSION_COMMANDS in
    // src/cli/command_inventory.rs.
    const RMUX_EXTENSION_COMMANDS: &[&str] = &[
        "capabilities",
        "claude",
        "doctor",
        "setup",
        "wait-pane",
        "pane-snapshot",
        "stream-pane",
        "collect-pane-output",
        "locator",
        "expect-pane",
        "find-panes",
        "find-sessions",
        "broadcast-keys",
        "with-session",
        "web-share",
    ];
    let expected = COMMAND_TABLE
        .iter()
        .map(|entry| entry.name.to_owned())
        .filter(|name| !RMUX_EXTENSION_COMMANDS.contains(&name.as_str()))
        .collect::<Vec<_>>();

    assert_eq!(list.status.code(), Some(0));
    assert_eq!(list_command_names(&stdout(&list)), expected);
    assert!(stderr(&list).is_empty());

    // Extensions stay reachable by explicit name even though the bare listing
    // hides them for tmux parity.
    let explicit = harness.run(&["list-commands", "web-share"])?;
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(
        list_command_names(&stdout(&explicit)),
        vec!["web-share".to_owned()]
    );
    assert!(stderr(&explicit).is_empty());

    let doctor = harness.run(&["list-commands", "doctor"])?;
    assert_eq!(doctor.status.code(), Some(0));
    assert_eq!(stdout(&doctor), "doctor tmux-dropin\n");
    assert!(stderr(&doctor).is_empty());

    let setup = harness.run(&["list-commands", "setup"])?;
    assert_eq!(setup.status.code(), Some(0));
    assert_eq!(stdout(&setup), "setup tmux-shim\n");
    assert!(stderr(&setup).is_empty());

    assert!(!harness.socket_path().exists());
    Ok(())
}

#[test]
fn list_commands_filters_by_name_alias_or_unique_prefix() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("list-commands-filter-exact")?;

    for (filter, error) in [
        ("list", "ambiguous command: list\n"),
        ("nosuch", "unknown command: nosuch\n"),
    ] {
        let result = harness.run(&["list-commands", filter])?;
        assert_eq!(result.status.code(), Some(1));
        assert!(stdout(&result).is_empty());
        assert_eq!(stderr(&result), error);
    }

    let alias = harness.run(&["list-commands", "lscm"])?;
    assert_eq!(alias.status.code(), Some(0));
    assert_eq!(
        stdout(&alias),
        "list-commands (lscm) [-F format] [command]\n"
    );
    assert!(stderr(&alias).is_empty());

    let prefix = harness.run(&["list-commands", "neww"])?;
    assert_eq!(prefix.status.code(), Some(0));
    assert!(stdout(&prefix).starts_with("new-window "));
    assert!(stderr(&prefix).is_empty());

    let parser_alias = harness.run(&["list-commands", "choose-session"])?;
    assert_eq!(parser_alias.status.code(), Some(0));
    assert!(stdout(&parser_alias).starts_with("choose-tree "));
    assert!(stderr(&parser_alias).is_empty());

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
fn kill_server_falls_back_to_wire_v3_shutdown_for_0_8_daemon() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("kill-server-wire-v3-fallback")?;
    let server = spawn_wire_v3_kill_server(harness.socket_path())?;

    let output = harness.run(&["kill-server"])?;

    assert_success(&output);
    server.join().expect("fake wire-v3 server should exit")?;
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
fn server_access_target_flag_is_accepted_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("server-access-target-flag")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["server-access", "-t", "%0", "-l", "ignored-user"])?;

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn invalid_listing_flags_are_rejected_by_cli_parser() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("invalid-listing-flags")?;

    for args in [
        &["list-clients", "-Q"][..],
        &["list-buffers", "-Q"][..],
        &["list-sessions", "-Q"][..],
    ] {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(1), "args={args:?}");
        assert!(stdout(&output).is_empty(), "args={args:?}");
        assert!(
            stderr(&output).contains("unexpected argument '-Q'"),
            "args={args:?}, stderr={}",
            stderr(&output)
        );
    }
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
fn direct_cli_list_windows_and_panes_forward_sort_and_reverse() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("direct-list-sort-reverse")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "z"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "a"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha", "-n", "m"])?);

    let windows = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-O",
        "name",
        "-F",
        "#{window_name}",
    ])?;
    assert_eq!(windows.status.code(), Some(0));
    assert!(stderr(&windows).is_empty());
    assert_eq!(stdout(&windows), "a\nm\nz\n");

    let invalid = harness.run(&["list-windows", "-t", "alpha", "-O", "bogus"])?;
    assert_eq!(invalid.status.code(), Some(1));
    assert_eq!(stderr(&invalid), "invalid sort order\n");

    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0"])?);
    let panes = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-O",
        "index",
        "-r",
        "-F",
        "#{pane_index}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert!(stderr(&panes).is_empty());
    assert_eq!(stdout(&panes), "1\n0\n");

    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "-n", "b"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "beta", "-n", "c"])?);
    let all_windows = harness.run(&[
        "list-windows",
        "-a",
        "-O",
        "name",
        "-F",
        "#{session_name}:#{window_name}",
    ])?;
    assert_eq!(all_windows.status.code(), Some(0));
    assert!(stderr(&all_windows).is_empty());
    assert_eq!(
        stdout(&all_windows),
        "alpha:a\nbeta:b\nbeta:c\nalpha:m\nalpha:z\n"
    );

    Ok(())
}

#[test]
fn capture_pane_invalid_bounds_are_rejected() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("capture-pane-invalid-bounds")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-x",
        "40",
        "-y",
        "5",
        "sleep 30",
    ])?);

    let output = harness.run(&["capture-pane", "-p", "-t", "alpha:0.0", "-E", "abc"])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "command capture-pane: -E expects a number\n"
    );
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
fn bare_semicolon_requires_existing_server_without_autostart() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("bare-semicolon-no-start")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let output = harness.run(&[";"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_absent_server_error(&output, &harness, ";");
    assert!(stdout(&output).is_empty());
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
fn control_mode_argv_command_uses_initial_flags_zero_frame() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-mode-argv-frame")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let output = harness.run(&["-C", "display-message", "-p", "hello"])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    let rendered = stdout(&output);
    assert!(
        !rendered.contains("%begin 1 0\n%end 1 0"),
        "argv command must not be preceded by an empty flags-0 pair: {rendered:?}"
    );
    assert!(
        rendered.contains("%begin ") && rendered.contains(" 1 0\nhello\n%end "),
        "argv command output must be inside its flags-0 frame: {rendered:?}"
    );
    assert!(
        !rendered.contains(" 2 1\nhello\n"),
        "argv command must not be reframed as stdin flags-1: {rendered:?}"
    );

    let list = harness.run(&[
        "-C",
        "display-message",
        "-p",
        "one",
        ";",
        "display-message",
        "-p",
        "two",
    ])?;
    assert_eq!(list.status.code(), Some(0));
    assert!(stderr(&list).is_empty());
    let rendered = stdout(&list);
    assert_eq!(
        rendered.matches(" 0\n").count(),
        4,
        "two argv commands should have two begin/end flags-0 pairs: {rendered:?}"
    );
    assert!(rendered.contains("one\n"));
    assert!(rendered.contains("two\n"));
    Ok(())
}

#[test]
fn control_mode_blocking_command_exits_on_server_shutdown() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-mode-shutdown-aborts")?;
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

    stdin.write_all(b"wait-for control-shutdown-abort\n")?;
    stdin.flush()?;
    wait_for_output_condition(
        &stdout_buffer,
        ATTACH_TIMEOUT,
        "control %begin for blocking wait-for",
        |rendered| rendered.contains("%begin "),
    )?;

    assert_success(&harness.run(&["kill-server"])?);
    drop(stdin);

    let status = wait_for_child_status(&mut child, ATTACH_TIMEOUT)?;
    let rendered = String::from_utf8(read_pipe_output(stdout_thread, "stdout")?)?;
    let stderr = String::from_utf8(read_pipe_output(stderr_thread, "stderr")?)?;

    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty());
    assert!(
        rendered.contains("%begin "),
        "control stream should have entered the blocking command before shutdown: {rendered:?}"
    );
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
fn new_session_auto_start_survives_immediate_restart_after_kill_server(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-restart-after-kill")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let first = harness.run_with(&["new-session", "-d", "-s", "alpha"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;
    assert_success(&first);

    let kill = harness.run(&["kill-server"])?;
    assert_success(&kill);

    let second = harness.run_with(&["new-session", "-d", "-s", "beta"], |command| {
        command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
    })?;
    assert_success(&second);
    assert_success(&harness.run(&["has-session", "-t", "beta"])?);
    Ok(())
}

#[test]
fn attached_socket_name_auto_starts_like_space_separated_socket_name() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("attached-socket-name-auto-start")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let create = harness.run_with(
        &["-Lglued", "new-session", "-d", "-s", "alpha"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&create);
    assert!(harness.pid_path().exists());

    let has = harness.run(&["-Lglued", "has-session", "-t", "alpha"])?;
    assert_success(&has);

    let kill = harness.run(&["-Lglued", "kill-server"])?;
    assert_success(&kill);
    Ok(())
}

#[test]
fn tmux_environment_does_not_route_client_to_inherited_socket() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("tmux-env-socket-routing")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let tmux_env = format!("{},0,0", harness.tmpdir().join("not-rmux.sock").display());
    let output = harness.run_with(
        &["display-message", "-p", "-t", "alpha", "#{session_name}"],
        |command| {
            command.env("TMUX", &tmux_env);
        },
    )?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "alpha\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn tmux_environment_routes_client_to_rmux_owned_socket() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("tmux-env-rmux-owned-socket")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let tmux_env = format!("{},0,0", harness.socket_path().display());
    let other_tmpdir = harness.tmpdir().join("other-rmux-root");
    let output = harness.run_with(&["has-session", "-t", "alpha"], |command| {
        command.env("TMUX", &tmux_env);
        command.env("RMUX_TMPDIR", &other_tmpdir);
    })?;

    assert_success(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn tmux_environment_does_not_route_client_to_explicit_socket_path() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("tmux-env-explicit-socket")?;
    let socket_path = harness.tmpdir().join("explicit.sock");
    let mut daemon = harness
        .base_command()
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Err(error) = wait_for_socket(&socket_path, &mut daemon) {
        let _ = terminate_child(&mut daemon);
        return Err(error);
    }

    let socket_arg = socket_path.to_string_lossy().to_string();
    assert_success(&harness.run(&["-S", &socket_arg, "new-session", "-d", "-s", "alpha"])?);

    let tmux_env = format!("{},0,0", socket_path.display());
    let output = harness.run_with(
        &["display-message", "-p", "-t", "alpha", "#{session_name}"],
        |command| {
            command.env("TMUX", &tmux_env);
        },
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    let stderr = stderr(&output);
    assert!(
        stderr.contains("no server running on ") && stderr.contains("/default"),
        "TMUX must not route rmux to an explicit tmux socket; got: {stderr}"
    );

    terminate_child(&mut daemon)?;
    let _ = fs::remove_file(&socket_path);
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
fn split_window_expands_start_directory_formats_at_spawn() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("split-window-format-start-directory")?;
    let _daemon = harness.start_hidden_daemon()?;
    let start_dir = harness.tmpdir().join("start-dir");
    let output_path = harness.tmpdir().join("split-cwd.txt");
    fs::create_dir_all(&start_dir)?;
    let start_dir_text = start_dir.to_string_lossy().to_string();

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-c", &start_dir_text])?);

    let shell_command = format!(
        "printf '%s' \"$(pwd)\" > {}; sleep 1",
        shell_quote(&output_path)
    );
    assert_success(&harness.run(&[
        "split-window",
        "-d",
        "-t",
        "alpha:0.0",
        "-c",
        "#{pane_current_path}",
        "sh",
        "-c",
        &shell_command,
    ])?);

    let expected_start_dir = fs::canonicalize(&start_dir)?.to_string_lossy().to_string();
    wait_for_file_contents(&output_path, &expected_start_dir, ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn new_window_expands_start_directory_formats_at_spawn() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-window-format-start-directory")?;
    let _daemon = harness.start_hidden_daemon()?;
    let start_dir = harness.tmpdir().join("start-dir");
    let output_path = harness.tmpdir().join("new-window-cwd.txt");
    fs::create_dir_all(&start_dir)?;
    let start_dir_text = start_dir.to_string_lossy().to_string();

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-c", &start_dir_text])?);

    let shell_command = format!(
        "printf '%s' \"$(pwd)\" > {}; sleep 1",
        shell_quote(&output_path)
    );
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-t",
        "alpha",
        "-c",
        "#{pane_current_path}",
        "sh",
        "-c",
        &shell_command,
    ])?);

    let expected_start_dir = fs::canonicalize(&start_dir)?.to_string_lossy().to_string();
    wait_for_file_contents(&output_path, &expected_start_dir, ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn respawn_pane_expands_start_directory_formats_at_spawn() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("respawn-pane-format-start-directory")?;
    let _daemon = harness.start_hidden_daemon()?;
    let start_dir = harness.tmpdir().join("start-dir");
    let output_path = harness.tmpdir().join("respawn-pane-cwd.txt");
    fs::create_dir_all(&start_dir)?;
    let start_dir_text = start_dir.to_string_lossy().to_string();

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-c", &start_dir_text])?);

    let shell_command = format!(
        "printf '%s' \"$(pwd)\" > {}; sleep 1",
        shell_quote(&output_path)
    );
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0.0",
        "-c",
        "#{pane_current_path}",
        "sh",
        "-c",
        &shell_command,
    ])?);

    let expected_start_dir = fs::canonicalize(&start_dir)?.to_string_lossy().to_string();
    wait_for_file_contents(&output_path, &expected_start_dir, ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn respawn_window_expands_start_directory_formats_at_spawn() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("respawn-window-format-start-directory")?;
    let _daemon = harness.start_hidden_daemon()?;
    let start_dir = harness.tmpdir().join("start-dir");
    let output_path = harness.tmpdir().join("respawn-window-cwd.txt");
    fs::create_dir_all(&start_dir)?;
    let start_dir_text = start_dir.to_string_lossy().to_string();

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-n",
        "work",
        "-c",
        &start_dir_text,
    ])?);

    let shell_command = format!(
        "printf '%s' \"$(pwd)\" > {}; sleep 1",
        shell_quote(&output_path)
    );
    assert_success(&harness.run(&[
        "respawn-window",
        "-k",
        "-t",
        "alpha:0",
        "-c",
        "#{pane_current_path}",
        "sh",
        "-c",
        &shell_command,
    ])?);

    let expected_start_dir = fs::canonicalize(&start_dir)?.to_string_lossy().to_string();
    wait_for_file_contents(&output_path, &expected_start_dir, ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn nested_cli_commands_inherit_calling_pane_target() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("nested-cli-current-pane-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    let display_path = harness.tmpdir().join("nested-display.txt");
    let split_marker = harness.tmpdir().join("nested-split.txt");
    let binary = shell_quote_str(env!("CARGO_BIN_EXE_rmux"));
    let command = format!(
        "sleep 1; {binary} display-message -p '#{{session_name}}:#{{pane_id}}' > {} 2>/dev/null; \
         {binary} split-window -d \"sh -c 'printf nested > {}; sleep 2'\"; sleep 2",
        shell_quote(&display_path),
        shell_quote(&split_marker)
    );

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sh", "-c", &command])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);

    wait_for_file_contents(&display_path, "alpha:%0\n", ATTACH_TIMEOUT)?;
    wait_for_file_contents(&split_marker, "nested", ATTACH_TIMEOUT)?;

    let panes = harness.run(&["list-panes", "-a", "-F", "#{session_name}:#{pane_index}"])?;
    assert_eq!(
        panes.status.code(),
        Some(0),
        "list-panes should succeed, stderr={}",
        stderr(&panes)
    );
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    assert!(
        panes.lines().any(|line| line == "alpha:1"),
        "nested split should create the new pane in alpha, got:\n{panes}"
    );
    assert!(
        !panes.lines().any(|line| line == "beta:1"),
        "nested split must not target the detached beta session, got:\n{panes}"
    );
    Ok(())
}

#[test]
fn foreign_socket_pane_hint_is_ignored_for_nested_targets() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("foreign-socket-pane-hint")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);
    let socket = harness.socket_path().to_string_lossy().to_string();
    let foreign_rmux = format!("{}/foreign.sock,123,0", harness.tmpdir().display());

    let output = harness.run_with(
        &["-S", &socket, "split-window", "-d", "sleep 30"],
        |command| {
            command.env("RMUX", &foreign_rmux);
            command.env("RMUX_PANE", "%0");
            command.env("TMUX_PANE", "%0");
        },
    )?;
    assert_success(&output);

    let panes = harness.run(&["list-panes", "-a", "-F", "#{session_name}:#{pane_index}"])?;
    assert_eq!(panes.status.code(), Some(0), "stderr={}", stderr(&panes));
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    assert!(
        panes.lines().any(|line| line == "beta:1"),
        "foreign hint must be ignored and the fallback should split beta, got:\n{panes}"
    );
    assert!(
        !panes.lines().any(|line| line == "alpha:1"),
        "foreign hint must not route the split to alpha, got:\n{panes}"
    );
    Ok(())
}

#[test]
fn queued_run_shell_children_inherit_calling_pane_target() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("queued-run-shell-current-pane-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    let gate = harness.tmpdir().join("go");
    let marker = harness.tmpdir().join("queued-run-shell-marker.txt");
    let command_file = harness.tmpdir().join("queued-run-shell.rmux");
    let child_script = harness.tmpdir().join("queued-run-shell-child.sh");
    let binary = shell_quote_str(env!("CARGO_BIN_EXE_rmux"));

    fs::write(
        &child_script,
        format!(
            "{binary} split-window -d \"sh -c 'printf queued > {}; sleep 2'\"\n",
            shell_quote(&marker)
        ),
    )?;
    fs::write(
        &command_file,
        format!("run-shell \"sh {}\"\n", shell_quote(&child_script)),
    )?;
    let pane_command = format!(
        "while [ ! -f {} ]; do sleep 0.05; done; {binary} source-file {}; sleep 2",
        shell_quote(&gate),
        shell_quote(&command_file)
    );

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "sh",
        "-c",
        &pane_command,
    ])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);
    fs::write(&gate, "go")?;

    wait_for_file_contents(&marker, "queued", ATTACH_TIMEOUT)?;
    let panes = harness.run(&["list-panes", "-a", "-F", "#{session_name}:#{pane_index}"])?;
    assert_eq!(panes.status.code(), Some(0), "stderr={}", stderr(&panes));
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    assert!(
        panes.lines().any(|line| line == "alpha:1"),
        "queued run-shell child should split alpha, got:\n{panes}"
    );
    assert!(
        !panes.lines().any(|line| line == "beta:1"),
        "queued run-shell child must not split detached beta, got:\n{panes}"
    );
    Ok(())
}

#[test]
fn nested_run_shell_commands_inherit_calling_pane_target() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("run-shell-c-current-pane-target")?;
    let _daemon = harness.start_hidden_daemon()?;
    let gate = harness.tmpdir().join("go");
    let marker = harness.tmpdir().join("run-shell-c-marker.txt");
    let binary = shell_quote_str(env!("CARGO_BIN_EXE_rmux"));
    let nested_command = format!(
        "split-window -d \"sh -c 'printf commands > {}; sleep 2'\"",
        shell_quote(&marker)
    );
    let pane_command = format!(
        "while [ ! -f {} ]; do sleep 0.05; done; {binary} run-shell -C {}; sleep 2",
        shell_quote(&gate),
        shell_quote_str(&nested_command)
    );

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "sh",
        "-c",
        &pane_command,
    ])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 30"])?);
    fs::write(&gate, "go")?;

    wait_for_file_contents(&marker, "commands", ATTACH_TIMEOUT)?;
    let panes = harness.run(&["list-panes", "-a", "-F", "#{session_name}:#{pane_index}"])?;
    assert_eq!(panes.status.code(), Some(0), "stderr={}", stderr(&panes));
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    assert!(
        panes.lines().any(|line| line == "alpha:1"),
        "run-shell -C should split alpha, got:\n{panes}"
    );
    assert!(
        !panes.lines().any(|line| line == "beta:1"),
        "run-shell -C must not split detached beta, got:\n{panes}"
    );
    Ok(())
}

#[test]
fn nested_new_session_expands_start_directory_against_calling_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("nested-new-session-format-start-directory")?;
    let _daemon = harness.start_hidden_daemon()?;
    let start_dir = harness.tmpdir().join("start-dir");
    let output_path = harness.tmpdir().join("nested-new-session-cwd.txt");
    let nested_script = harness.tmpdir().join("nested-new-session.sh");
    let nested_stdout = harness.tmpdir().join("nested-new-session.stdout");
    let nested_stderr = harness.tmpdir().join("nested-new-session.stderr");
    fs::create_dir_all(&start_dir)?;
    let start_dir_text = start_dir.to_string_lossy().to_string();
    let binary = shell_quote_str(env!("CARGO_BIN_EXE_rmux"));
    fs::write(
        &nested_script,
        format!(
            "{binary} new-session -d -s made -c '#{{pane_current_path}}' \
         \"sh -c 'printf %s \\\"$(pwd)\\\" > {}; sleep 2'\" > {} 2> {}\n\
         sleep 2\n",
            shell_quote(&output_path),
            shell_quote(&nested_stdout),
            shell_quote(&nested_stderr)
        ),
    )?;
    let command = format!("sh {}", shell_quote(&nested_script));

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-c",
        &start_dir_text,
        "sh",
        "-c",
        &command,
    ])?);

    let expected_start_dir = fs::canonicalize(&start_dir)?.to_string_lossy().to_string();
    wait_for_file_contents_any(
        &output_path,
        &[start_dir_text.as_str(), expected_start_dir.as_str()],
        Duration::from_secs(15),
    )
    .map_err(|error| {
        format!(
            "{error}; nested stdout={}; nested stderr={}",
            fs::read_to_string(&nested_stdout).unwrap_or_default(),
            fs::read_to_string(&nested_stderr).unwrap_or_default()
        )
    })?;
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
fn new_session_uses_client_shell_env_by_default() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("new-session-client-shell-env")?;
    let _daemon = harness.start_hidden_daemon()?;
    let output_path = harness.tmpdir().join("client-shell.txt");
    let expected_shell = "/bin/sh";
    let shell_command = format!("printf '%s' \"$SHELL\" > {}", shell_quote(&output_path));

    let create = harness.run_with(
        &["new-session", "-d", "-s", "alpha", &shell_command],
        |command| {
            command.env("SHELL", expected_shell);
        },
    )?;
    assert_success(&create);

    wait_for_file_contents(&output_path, expected_shell, ATTACH_TIMEOUT)?;
    Ok(())
}

#[test]
fn show_options_default_shell_reports_resolved_shell() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-options-default-shell")?;
    let expected_shell = "/bin/bash";
    let create = harness.run_with(&["new-session", "-d", "-s", "alpha"], |command| {
        command.env("SHELL", expected_shell);
    })?;
    assert_success(&create);

    let shown = harness.run(&["show-options", "-gv", "default-shell"])?;

    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), format!("{expected_shell}\n"));
    assert!(stderr(&shown).is_empty());

    let all = harness.run(&["show-options", "-g"])?;
    assert_eq!(all.status.code(), Some(0));
    assert!(
        stdout(&all).contains(&format!("default-shell {expected_shell}\n")),
        "{}",
        stdout(&all)
    );

    let local = harness.run(&["show-options", "-qv", "default-shell"])?;
    assert_eq!(local.status.code(), Some(0));
    assert!(stdout(&local).is_empty());
    assert!(stderr(&local).is_empty());
    Ok(())
}

#[test]
fn show_options_default_shell_preserves_explicit_value() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-options-default-shell-explicit")?;
    let _daemon = harness.start_hidden_daemon()?;
    let expected_shell = "/tmp/rmux-explicit-shell";

    assert_success(&harness.run(&["set-option", "-g", "default-shell", expected_shell])?);

    let named = harness.run(&["show-options", "-g", "default-shell"])?;
    assert_eq!(named.status.code(), Some(0));
    assert_eq!(stdout(&named), format!("default-shell {expected_shell}\n"));
    assert!(stderr(&named).is_empty());

    let value = harness.run(&["show-options", "-gv", "default-shell"])?;
    assert_eq!(value.status.code(), Some(0));
    assert_eq!(stdout(&value), format!("{expected_shell}\n"));
    assert!(stderr(&value).is_empty());

    let all = harness.run(&["show-options", "-g"])?;
    assert_eq!(all.status.code(), Some(0));
    assert!(stdout(&all).contains(&format!("default-shell {expected_shell}\n")));
    assert!(stderr(&all).is_empty());

    Ok(())
}

#[test]
fn default_shell_format_reports_resolved_shell() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("format-default-shell")?;
    let expected_shell = "/bin/bash";
    let create = harness.run_with(&["new-session", "-d", "-s", "alpha"], |command| {
        command.env("SHELL", expected_shell);
    })?;
    assert_success(&create);

    let shown = harness.run(&["display-message", "-p", "#{default-shell}"])?;

    assert_eq!(shown.status.code(), Some(0));
    assert_eq!(stdout(&shown), format!("{expected_shell}\n"));
    assert!(stderr(&shown).is_empty());
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
fn socket_path_flag_can_start_directly_under_tmp() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("socket-path-under-tmp")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let socket_path = std::env::temp_dir().join(format!(
        "rmux-direct-socket-{}-{}.sock",
        std::process::id(),
        "cli-surface"
    ));
    let socket_arg = socket_path.to_string_lossy().to_string();
    let _ = fs::remove_file(&socket_path);

    let created = harness.run_with(
        &["-S", &socket_arg, "new-session", "-d", "-s", "alpha"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&created);

    let found = harness.run(&["-S", &socket_arg, "has-session", "-t", "alpha"])?;
    assert_success(&found);

    let _ = harness.run(&["-S", &socket_arg, "kill-server"]);
    let _ = fs::remove_file(&socket_path);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn empty_socket_path_flag_is_distinct_from_the_default_socket() -> Result<(), Box<dyn Error>> {
    let _empty_socket_lock = acquire_empty_socket_path_lock()?;
    let harness = CliHarness::new("empty-socket-path")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let default_created =
        harness.run_with(&["new-session", "-d", "-s", "default_s"], |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        })?;
    assert_success(&default_created);

    let empty_created = harness.run_with(
        &["-S", "", "new-session", "-d", "-s", "empty_s"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&empty_created);

    let default_list = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(default_list.status.code(), Some(0));
    assert!(stderr(&default_list).is_empty());
    assert_eq!(stdout(&default_list), "default_s\n");

    let empty_list = harness.run(&["-S", "", "list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(empty_list.status.code(), Some(0));
    assert!(stderr(&empty_list).is_empty());
    assert_eq!(stdout(&empty_list), "empty_s\n");

    let _ = harness.run(&["-S", "", "kill-server"]);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[test]
fn empty_socket_path_flag_is_rejected_outside_linux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("empty-socket-path-rejected")?;
    let _cleanup = harness.auto_start_cleanup()?;

    let created = harness.run_with(
        &["-S", "", "new-session", "-d", "-s", "empty_s"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;

    assert_eq!(created.status.code(), Some(1));
    assert!(stdout(&created).is_empty());
    assert_eq!(
        stderr(&created),
        "i/o error: -S '' is only supported on Linux abstract Unix sockets\n"
    );
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
fn nested_attach_session_with_different_socket_name_uses_requested_server(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("attach-session-nested-other-socket")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let created_other = harness.run_with(
        &["-L", "other", "new-session", "-d", "-s", "beta"],
        |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;
    assert_success(&created_other);

    let rmux_env = format!("{},1,0", harness.socket_path().display());
    let output = harness.run_with(
        &["-L", "other", "attach-session", "-t", "beta"],
        |command| {
            command.env("RMUX", &rmux_env);
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        },
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert!(
        !stderr(&output).contains("no current client"),
        "attach-session -L other must not be rewritten to switch-client on inherited socket: {}",
        stderr(&output)
    );
    assert!(
        !stderr(&output).contains("switch-client requires an attached client"),
        "attach-session -L other must use requested socket instead of nested switch-client: {}",
        stderr(&output)
    );
    let listed_other = harness.run(&["-L", "other", "list-sessions", "-F", "#{session_name}"])?;
    assert_eq!(stdout(&listed_other), "beta\n");

    let _ = harness.run(&["-L", "other", "kill-server"]);
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
fn set_option_combined_scope_flags_use_tmux_precedence() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-option-combined-scope-precedence")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let server_beats_window =
        harness.run(&["set-option", "-sw", "-t", "alpha", "status", "off"])?;
    assert_success(&server_beats_window);
    let session_status = harness.run(&["show-options", "-v", "-t", "alpha", "status"])?;
    assert_eq!(stdout(&session_status), "off\n");

    let server_beats_window = harness.run(&["set-option", "-ws", "message-limit", "44"])?;
    assert_success(&server_beats_window);
    let server_limit = harness.run(&["show-options", "-sv", "message-limit"])?;
    assert_eq!(stdout(&server_limit), "44\n");

    let pane_beats_window =
        harness.run(&["set-option", "-pw", "-t", "alpha:0.0", "@pane", "yes"])?;
    assert_success(&pane_beats_window);
    let pane_value = harness.run(&["show-options", "-pv", "-t", "alpha:0.0", "@pane"])?;
    assert_eq!(stdout(&pane_value), "yes\n");

    Ok(())
}

#[test]
fn show_options_global_pane_combination_matches_tmux_scope_precedence() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("show-options-gp-precedence")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let pane_global = harness.run(&["set-option", "-gp", "pane-border-style", "fg=red"])?;
    assert_success(&pane_global);
    let pane_global_shown = harness.run(&["show-options", "-gpqv", "pane-border-style"])?;
    assert_eq!(pane_global_shown.status.code(), Some(0));
    assert_eq!(stdout(&pane_global_shown), "fg=red\n");
    assert!(stderr(&pane_global_shown).is_empty());

    let user_pane_global = harness.run(&["set-option", "-gp", "@pane-global", "yes"])?;
    assert_success(&user_pane_global);
    let user_pane_global_shown = harness.run(&["show-options", "-gpqv", "@pane-global"])?;
    assert_eq!(user_pane_global_shown.status.code(), Some(0));
    assert_eq!(stdout(&user_pane_global_shown), "yes\n");
    assert!(stderr(&user_pane_global_shown).is_empty());

    let status = harness.run(&["show-options", "-gp", "status"])?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(stdout(&status), "status on\n");
    assert!(stderr(&status).is_empty());

    let history_limit = harness.run(&["show-options", "-pg", "history-limit"])?;
    assert_eq!(history_limit.status.code(), Some(0));
    assert_eq!(stdout(&history_limit), "history-limit 2000\n");
    assert!(stderr(&history_limit).is_empty());

    let queued = harness.run(&["run-shell", "-C", "show-options -gp pane-border-style"])?;
    assert_eq!(queued.status.code(), Some(0));
    assert_eq!(stdout(&queued), "pane-border-style fg=red\n");
    assert!(stderr(&queued).is_empty());

    let queued_status = harness.run(&["run-shell", "-C", "show-options -gp status"])?;
    assert_eq!(queued_status.status.code(), Some(0));
    assert_eq!(stdout(&queued_status), "status on\n");
    assert!(stderr(&queued_status).is_empty());
    Ok(())
}

#[test]
fn set_option_explicit_scopes_coerce_known_options_to_natural_table() -> Result<(), Box<dyn Error>>
{
    let harness = CliHarness::new("set-option-explicit-scope-known-natural")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let window_status = harness.run(&["set-option", "-w", "-t", "alpha", "status", "off"])?;
    assert_success(&window_status);
    let shown_session_status = harness.run(&["show-options", "-v", "-t", "alpha", "status"])?;
    assert_eq!(stdout(&shown_session_status), "off\n");

    let pane_limit =
        harness.run(&["set-option", "-p", "-t", "alpha:0.0", "message-limit", "77"])?;
    assert_success(&pane_limit);
    let shown_server_limit = harness.run(&["show-options", "-sv", "message-limit"])?;
    assert_eq!(stdout(&shown_server_limit), "77\n");

    let server_status = harness.run(&["set-option", "-s", "-t", "alpha", "status", "on"])?;
    assert_success(&server_status);
    let shown_session_status = harness.run(&["show-options", "-v", "-t", "alpha", "status"])?;
    assert_eq!(stdout(&shown_session_status), "on\n");

    let compact_target = harness.run(&["set-option", "-talpha", "status", "off"])?;
    assert_success(&compact_target);
    let shown_compact_status = harness.run(&["show-options", "-v", "-t", "alpha", "status"])?;
    assert_eq!(stdout(&shown_compact_status), "off\n");

    let pane_style = harness.run(&[
        "set-option",
        "-pg",
        "-t",
        "alpha:0.0",
        "window-style",
        "bg=blue",
    ])?;
    assert_success(&pane_style);
    let pane_value = harness.run(&["show-options", "-pv", "-t", "alpha:0.0", "window-style"])?;
    assert_eq!(stdout(&pane_value), "bg=blue\n");
    let global_value = harness.run(&["show-options", "-gwv", "window-style"])?;
    assert_eq!(stdout(&global_value), "default\n");

    Ok(())
}

#[test]
fn set_option_bare_choice_toggles_by_index_like_tmux() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-option-choice-index-toggle")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    assert_success(&harness.run(&["set-option", "-w", "mode-keys"])?);
    let mode_keys = harness.run(&["show-options", "-wqv", "mode-keys"])?;
    assert_eq!(stdout(&mode_keys), "vi\n");

    assert_success(&harness.run(&["set-option", "-g", "status-position"])?);
    let status_position = harness.run(&["show-options", "-gqv", "status-position"])?;
    assert_eq!(stdout(&status_position), "top\n");

    assert_success(&harness.run(&["set-option", "-g", "set-clipboard", "external"])?);
    assert_success(&harness.run(&["set-option", "-g", "set-clipboard"])?);
    let clipboard_off = harness.run(&["show-options", "-gqv", "set-clipboard"])?;
    assert_eq!(stdout(&clipboard_off), "off\n");

    assert_success(&harness.run(&["set-option", "-g", "set-clipboard", "on"])?);
    assert_success(&harness.run(&["set-option", "-g", "set-clipboard"])?);
    let clipboard_on = harness.run(&["show-options", "-gqv", "set-clipboard"])?;
    assert_eq!(stdout(&clipboard_on), "on\n");

    Ok(())
}

#[test]
fn show_options_quotes_tmux37_special_values_and_sorts_user_options_first(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("show-options-tmux37-quoting")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for (name, value) in [
        ("@z", "1"),
        ("@a", "2"),
        ("@semi", "a;b"),
        ("@brace", "a}b"),
        ("@percent", "a%b"),
        ("@quote", "a'b"),
        ("@digit", "x$1y"),
        ("@bracevar", "x${z}"),
    ] {
        assert_success(&harness.run(&["set-option", "-g", name, value])?);
    }

    let all = harness.run(&["show-options", "-g"])?;
    let all_stdout = stdout(&all);
    let mut lines = all_stdout.lines();
    assert_eq!(lines.next(), Some("@a 2"));
    assert_eq!(lines.next(), Some("@brace \"a}b\""));
    assert_eq!(lines.next(), Some("@bracevar \"x\\${z}\""));

    assert_success(&harness.run(&["set-option", "-g", "@tilde", "~/bin"])?);
    assert_success(&harness.run(&["set-option", "-g", "@double", "has\"quote"])?);
    assert_success(&harness.run(&["set-option", "-g", "@control", "line1\nline2\tbell\u{7}"])?);

    for (name, expected) in [
        ("@semi", "@semi \"a;b\"\n"),
        ("@percent", "@percent \"a%b\"\n"),
        ("@quote", "@quote \"a'b\"\n"),
        ("@digit", "@digit \"x$1y\"\n"),
        ("@tilde", "@tilde \\~/bin\n"),
        ("@double", "@double 'has\"quote'\n"),
        ("@control", "@control line1\\nline2\\tbell\\a\n"),
    ] {
        let shown = harness.run(&["show-options", "-g", name])?;
        assert_eq!(stdout(&shown), expected);
    }

    let word_separators = harness.run(&["show-options", "-g", "word-separators"])?;
    assert_eq!(
        stdout(&word_separators),
        "word-separators \"!\\\"#$%&'()*+,-./:;<=>?@[\\\\]^`{|}~\"\n"
    );

    Ok(())
}

#[test]
fn set_option_scalar_mutations_keep_product_semantics() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("set-option-scalar-state")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);

    let toggle = harness.run(&["set-option", "-g", "status"])?;
    assert_success(&toggle);
    let status = harness.run(&["show-options", "-gqv", "status"])?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(stdout(&status), "off\n");

    assert_success(&harness.run(&["set-option", "-g", "history-limit", "100"])?);
    let append_scalar = harness.run(&["set-option", "-ga", "history-limit", "5"])?;
    assert_success(&append_scalar);
    assert!(stdout(&append_scalar).is_empty());
    assert!(stderr(&append_scalar).is_empty());
    let history_limit = harness.run(&["show-options", "-gqv", "history-limit"])?;
    assert_eq!(history_limit.status.code(), Some(0));
    assert_eq!(stdout(&history_limit), "5\n");

    let consultative_scope = harness.run(&["set-option", "-w", "-g", "exit-empty", "off"])?;
    assert_success(&consultative_scope);
    assert!(stdout(&consultative_scope).is_empty());
    assert!(stderr(&consultative_scope).is_empty());
    let exit_empty_window = harness.run(&["show-options", "-gwv", "exit-empty"])?;
    assert_eq!(exit_empty_window.status.code(), Some(0));
    assert_eq!(stdout(&exit_empty_window), "off\n");
    let exit_empty = harness.run(&["show-options", "-gqv", "exit-empty"])?;
    assert_eq!(exit_empty.status.code(), Some(0));
    assert_eq!(stdout(&exit_empty), "off\n");

    let already_set = harness.run(&["set-option", "-o", "-g", "status", "on"])?;
    assert_eq!(already_set.status.code(), Some(1));
    assert!(stdout(&already_set).is_empty());
    assert_eq!(stderr(&already_set), "already set: status\n");

    let dash_dash_value = harness.run(&["set-option", "-g", "status-left", "--", "-abc"])?;
    assert_eq!(dash_dash_value.status.code(), Some(1));
    assert!(stdout(&dash_dash_value).is_empty());
    assert_eq!(
        stderr(&dash_dash_value),
        "command set-option: too many arguments (need at most 2)\n"
    );
    let status_left = harness.run(&["show-options", "-gqv", "status-left"])?;
    assert_eq!(status_left.status.code(), Some(0));
    assert_ne!(stdout(&status_left), "-abc\n");

    let dash_dash_terminator = harness.run(&["set-option", "-g", "--", "status-left", "plain"])?;
    assert_eq!(dash_dash_terminator.status.code(), Some(0));
    assert!(stdout(&dash_dash_terminator).is_empty());
    assert!(stderr(&dash_dash_terminator).is_empty());
    let status_left = harness.run(&["show-options", "-gqv", "status-left"])?;
    assert_eq!(status_left.status.code(), Some(0));
    assert_eq!(stdout(&status_left), "plain\n");

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

    let noisy_show = harness.run(&["show-options", "-g", "definitely-not-real-option"])?;
    assert_eq!(noisy_show.status.code(), Some(1));
    assert!(stdout(&noisy_show).is_empty());
    assert_eq!(
        stderr(&noisy_show),
        "invalid option: definitely-not-real-option\n"
    );

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
fn invalid_option_choice_uses_toplevel_tty_error_text() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("invalid-option-choice-text")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["set-option", "-g", "status", "maybe"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "unknown value: maybe\n");
    Ok(())
}

#[test]
fn explicit_empty_option_choice_is_invalid() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("empty-option-choice-text")?;
    let _daemon = harness.start_hidden_daemon()?;

    let output = harness.run(&["set-option", "-g", "mode-keys", ""])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), "unknown value: \n");
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
    assert_eq!(stdout(&alpha_status), "");
    let alpha_status_inherited = harness.run(&["show-options", "-Av", "-t", "alpha", "status"])?;
    assert_eq!(stdout(&alpha_status_inherited), "on\n");
    let beta_status = harness.run(&["show-options", "-v", "-t", "beta", "status"])?;
    assert_eq!(stdout(&beta_status), "off\n");

    assert_success(&harness.run(&["set-option", "mode-keys", "vi"])?);
    let alpha_mode = harness.run(&["show-options", "-wv", "-t", "alpha", "mode-keys"])?;
    assert_eq!(stdout(&alpha_mode), "");
    let alpha_mode_inherited =
        harness.run(&["show-options", "-wAv", "-t", "alpha", "mode-keys"])?;
    assert_eq!(stdout(&alpha_mode_inherited), "emacs\n");
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
    assert_absent_server_error(&show_options_w_no_target, &harness, "show-options");
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
    assert_eq!(stderr(&output), "can't find session: never-created\n");

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
    assert_eq!(stderr(&output), LONG_OPTION_HELP);
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

fn wait_for_child_status(
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }

    let _ = terminate_child(child);
    Err(format!("timed out waiting for child process {}", child.id()).into())
}

fn wait_for_file_contents(
    path: &Path,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    wait_for_file_contents_any(path, &[expected], timeout)
}

fn wait_for_file_contents_any(
    path: &Path,
    expected_values: &[&str],
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last_contents = None;

    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if expected_values.contains(&contents.as_str()) => return Ok(()),
            Ok(contents) => {
                last_contents = Some(contents);
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }

    Err(format!(
        "timed out waiting for '{}' to contain one of {:?}; last contents: {:?}",
        path.display(),
        expected_values,
        last_contents
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
