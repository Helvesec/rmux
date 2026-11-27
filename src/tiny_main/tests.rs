use super::*;
#[cfg(windows)]
use crate::windows_shell::WindowsShellEnvironment;
#[cfg(windows)]
use crate::windows_shell::{
    client_shell_for_parent_name as windows_client_shell_for_parent_name,
    invoking_client_environment as windows_invoking_client_environment,
};
use parse::{
    has_queue_separator, parse_display_message, parse_has_session, parse_join_pane,
    parse_kill_pane, parse_kill_session, parse_list_panes, parse_list_windows, parse_new_session,
    parse_new_window, parse_rename_window, parse_resize_pane, parse_select_window, parse_send_keys,
    parse_set_option, parse_show_options, parse_source_file, parse_split_window,
};
use rmux_proto::{
    ErrorResponse, ResizePaneAdjustment, Response, RmuxError, SourceFileResponse, SplitDirection,
    Target,
};

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn split_window_full_and_direct_tiny_share_direction_precedence() {
    for flags in [["-h", "-v"], ["-v", "-h"]] {
        let argv = [
            "rmux",
            "split-window",
            "-d",
            "-t",
            "schema:0.0",
            flags[0],
            flags[1],
        ];
        let full = crate::cli_args::parse(argv).expect("canonical split-window");
        let crate::cli_args::Command::SplitWindow(full) = full.command.unwrap() else {
            panic!("expected full split-window");
        };
        let TinyInvocation::Direct(tiny) = TinyInvocation::parse(&os_args(&argv)) else {
            panic!("expected direct tiny split-window");
        };
        let TinyCommand::SplitWindow { request, .. } = *tiny else {
            panic!("expected tiny split-window request");
        };
        assert_eq!(full.direction(), SplitDirection::Horizontal, "{argv:?}");
        assert_eq!(request.direction, full.direction(), "{argv:?}");
        assert_eq!(request.target.as_deref(), Some("schema:0.0"));
        assert!(request.detached);
    }
}

fn assert_tiny_direct(args: &[&str], command: &str) {
    match TinyInvocation::parse(&os_args(args)) {
        TinyInvocation::Direct(parsed) => assert_eq!(parsed.name(), command),
        TinyInvocation::Fallback => panic!("{args:?} unexpectedly fell back to helper"),
        TinyInvocation::Version => panic!("{args:?} unexpectedly parsed as version"),
    }
}

fn assert_tiny_starts_server(args: &[&str], expected: bool) {
    match TinyInvocation::parse(&os_args(args)) {
        TinyInvocation::Direct(parsed) => assert_eq!(parsed.starts_server(), expected),
        TinyInvocation::Fallback => panic!("{args:?} unexpectedly fell back to helper"),
        TinyInvocation::Version => panic!("{args:?} unexpectedly parsed as version"),
    }
}

fn assert_tiny_fallback(args: &[&str]) {
    match TinyInvocation::parse(&os_args(args)) {
        TinyInvocation::Fallback => {}
        TinyInvocation::Direct(parsed) => {
            panic!("{args:?} unexpectedly parsed as tiny {}", parsed.name())
        }
        TinyInvocation::Version => panic!("{args:?} unexpectedly parsed as version"),
    }
}

fn assert_source_file_dispatch(args: &[&str]) {
    let command_args = args
        .iter()
        .position(|arg| *arg == "source-file")
        .map(|index| os_args(&args[index + 1..]))
        .expect("source-file command");
    if source_file_should_use_tiny(&command_args) {
        assert_tiny_direct(args, "source-file");
    } else {
        assert_tiny_fallback(args);
    }
}

fn explicit_socket_path() -> String {
    if cfg!(windows) {
        rmux_client::resolve_socket_path(Some(OsStr::new("benchsock")), None)
            .expect("valid Windows explicit pipe path")
            .to_string_lossy()
            .into_owned()
    } else {
        "/tmp/rmux-bench.sock".to_owned()
    }
}

#[test]
fn tiny_runtime_alias_probe_uses_the_original_command_arguments() {
    assert_eq!(
        tiny_invoked_command_arguments(&os_args(&["rmux", "list-sessions"])),
        Some(vec!["list-sessions".to_owned()])
    );
    assert_eq!(
        tiny_invoked_command_arguments(&os_args(&["rmux", "-L", "demo", "ls"])),
        Some(vec!["ls".to_owned()])
    );
    assert_eq!(
        tiny_invoked_command_arguments(&os_args(&[
            "rmux",
            "-S",
            "/tmp/demo.sock",
            "display-message",
            "-p",
            "$HOME",
        ])),
        Some(vec![
            "display-message".to_owned(),
            "-p".to_owned(),
            "$HOME".to_owned(),
        ])
    );
}

#[test]
fn tiny_start_server_inventory_covers_every_cold_alias_entry_path() {
    for args in [
        &["rmux", "start-server"][..],
        &["rmux", "new-session", "-d"][..],
        &["rmux", "attach-session"][..],
    ] {
        assert_tiny_starts_server(args, true);
    }

    for args in [
        &["rmux", "list-sessions"][..],
        &["rmux", "has-session", "-t", "alpha"][..],
        &["rmux", "kill-server"][..],
    ] {
        assert_tiny_starts_server(args, false);
    }
}

#[test]
fn tiny_startup_alias_inventory_is_independent_of_nested_context() {
    const CHILD_CONTEXT: &str = "RMUX_TEST_TINY_STARTUP_CONTEXT";
    if env::var_os(CHILD_CONTEXT).is_none() {
        let executable = rmux_os::build_identity::resolve(
            env::current_exe().expect("test executable").as_os_str(),
        )
        .expect("test executable identity");
        for context in ["outside", "RMUX", "TMUX"] {
            let mut child = std::process::Command::new(&executable);
            child
                .args([
                    "--exact",
                    "tiny_main::tests::tiny_startup_alias_inventory_is_independent_of_nested_context",
                    "--nocapture",
                ])
                .env(CHILD_CONTEXT, context)
                .env_remove("RMUX")
                .env_remove("TMUX")
                .env_remove("RMUX_DISABLE_TINY_CLI");
            if context != "outside" {
                child.env(context, "outer-session,123,0");
            }
            let output = child.output().expect("isolated startup inventory");
            assert!(
                output.status.success(),
                "{context}: {:?}\n{}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        return;
    }

    for (canonical, names, tail) in [
        (
            "start-server",
            ["start-server", "start", "start-s"],
            &[][..],
        ),
        ("new-session", ["new-session", "new", "new-s"], &["-d"][..]),
        (
            "attach-session",
            ["attach-session", "attach", "att"],
            &[][..],
        ),
    ] {
        for name in names {
            for binary in ["rmux", "tmux"] {
                let mut args = vec![binary, name];
                args.extend_from_slice(tail);
                assert_tiny_direct(&args, canonical);
                assert_tiny_starts_server(&args, true);
            }
        }
    }
}

#[test]
fn reusable_tiny_connection_is_scoped_to_one_matching_socket() {
    let socket_path = Path::new("/tmp/rmux-tiny-live.sock");
    let other_socket_path = Path::new("/tmp/rmux-tiny-other.sock");
    let mut connection = TinyCommandConnection::reusable(socket_path, 7_u8);

    assert_eq!(connection.take_reusable(other_socket_path), None);
    assert_eq!(connection.take_reusable(socket_path), Some(7));
    assert_eq!(connection.take_reusable(socket_path), None);
}

#[test]
fn native_long_version_product_divergence_uses_shared_decision() {
    for tail in [
        vec!["--version"],
        vec!["--version", "-x"],
        vec!["-vVx"],
        vec!["-u", "--version"],
        vec!["-vL", "--version", "-V"],
    ] {
        let args = std::iter::once("rmux")
            .chain(tail)
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert!(
            matches!(TinyInvocation::parse(&args), TinyInvocation::Version),
            "{args:?}"
        );
    }
    for args in [
        &["tmux", "--version"][..],
        &["rmux", "--version=x"][..],
        &["rmux", "-h", "--version"][..],
        &["rmux", "--help", "--version"][..],
        &["rmux", "-x", "--version"][..],
        &["rmux", "--", "--version"][..],
        &["rmux", "-vL", "--version"][..],
        &["rmux", "-vf", "--version"][..],
    ] {
        assert_tiny_fallback(args);
    }
}

#[test]
fn missing_top_level_arguments_stay_on_full_helper_path() {
    for personality in ["rmux", "tmux"] {
        for flag in ['L', 'S', 'f', 'T', 'c'] {
            for option in [format!("-{flag}"), format!("-uv{flag}")] {
                assert_tiny_fallback(&[personality, &option]);
            }
        }
    }
}

#[test]
fn long_help_stays_on_full_helper_path() {
    for personality in ["rmux", "tmux"] {
        for args in [
            vec![personality, "--help"],
            vec![personality, "-2CNu", "--help"],
            vec![personality, "--help", "-V"],
            vec![personality, "--help", "--version"],
            vec![personality, "--nope", "--help"],
            vec![personality, "-h", "--help"],
            vec![personality, "-vL", "value", "--help"],
            vec![personality, "-S", "--help", "-x"],
            vec![personality, "--", "--help"],
            vec![personality, "list-commands", "--help"],
        ] {
            assert_tiny_fallback(&args);
        }
    }
}

#[test]
fn tiny_connect_error_uses_tmux_shape() {
    let message = client_error(
        std::path::Path::new("/tmp/rmux-missing.sock"),
        rmux_client::ClientError::Io(std::io::ErrorKind::NotFound.into()),
    );

    assert!(message.starts_with("error connecting to /tmp/rmux-missing.sock ("));
    assert!(!message.contains("failed to connect to rmux server"));
    assert!(!message.contains("os error"));
}

#[test]
fn tiny_incompatible_daemon_message_uses_kill_server_hint() {
    let explicit = std::path::Path::new("/tmp/rmux stale.sock");
    let explicit_message = incompatible_daemon_error(explicit);
    assert!(explicit_message.contains("uses an incompatible protocol"));
    #[cfg(windows)]
    let expected_hint = "rmux -S \"/tmp/rmux stale.sock\" kill-server";
    #[cfg(not(windows))]
    let expected_hint = "rmux -S '/tmp/rmux stale.sock' kill-server";
    assert!(explicit_message.contains(expected_hint));

    let default_socket = rmux_client::default_socket_path().expect("default socket path");
    let default_message = incompatible_daemon_error(&default_socket);
    assert!(default_message.contains("rmux kill-server"));
}

#[test]
fn tiny_kill_server_legacy_fallback_accepts_every_prior_wire_version() {
    for got in 1..RMUX_WIRE_VERSION {
        let error = ClientError::Protocol(RmuxError::UnsupportedWireVersion {
            got,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        });
        assert_eq!(legacy_shutdown_fallback_wire_version(&error), Some(got));
    }

    for got in [0, RMUX_WIRE_VERSION, RMUX_WIRE_VERSION + 1] {
        let error = ClientError::Protocol(RmuxError::UnsupportedWireVersion {
            got,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        });
        assert_eq!(legacy_shutdown_fallback_wire_version(&error), None);
    }
}

#[derive(Clone)]
struct FuzzRng(u64);

impl FuzzRng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_usize(&mut self, upper: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 32) as usize) % upper
    }
}

fn fuzz_args<'a>(rng: &mut FuzzRng, pool: &'a [&'a str], max_len: usize) -> Vec<OsString> {
    let len = rng.next_usize(max_len + 1);
    (0..len)
        .map(|_| OsString::from(pool[rng.next_usize(pool.len())]))
        .collect()
}

fn all_utf8(args: &[OsString]) -> bool {
    args.iter().all(|arg| arg.to_str().is_some())
}

fn exact_target(value: &str) -> bool {
    !tmux_selector(value) && Target::parse(value).is_ok()
}

fn exact_pane_target(value: &str) -> bool {
    if tmux_selector(value) {
        return false;
    }
    matches!(Target::parse(value), Ok(Target::Pane(_)))
}

fn tmux_selector(value: &str) -> bool {
    matches!(
        value.as_bytes().first(),
        Some(b'%' | b'@' | b'!' | b'=' | b'~' | b'{' | b'$' | b'+' | b'-')
    )
}

fn display_message_should_use_tiny(args: &[OsString]) -> bool {
    if has_queue_separator(args) {
        return false;
    }

    let mut print = false;
    let mut format = false;
    let mut message = false;
    let mut index = 0;
    while index < args.len() {
        let Some(arg) = args[index].to_str() else {
            return false;
        };
        match arg {
            "--" => {
                if args[index + 1..].len() != 1 || !all_utf8(&args[index + 1..]) {
                    return false;
                }
                message = true;
                break;
            }
            "-p" => print = true,
            "-t" => {
                index += 1;
                let Some(target) = args.get(index).and_then(|arg| arg.to_str()) else {
                    return false;
                };
                if target.contains(':') || !exact_target(target) {
                    return false;
                }
            }
            "-F" => {
                index += 1;
                if args.get(index).and_then(|arg| arg.to_str()).is_none() {
                    return false;
                }
                format = true;
            }
            "-a" | "-I" | "-l" | "-N" | "-v" | "--json" | "-c" | "-d" => return false,
            value if value.starts_with('-') => return false,
            _ => {
                if args[index..].len() != 1 || !all_utf8(&args[index..]) {
                    return false;
                }
                message = true;
                break;
            }
        }
        index += 1;
    }
    print && !(format && message)
}

fn send_keys_should_use_tiny(args: &[OsString]) -> bool {
    if has_queue_separator(args) {
        return false;
    }

    let mut target = false;
    let mut index = 0;
    while index < args.len() {
        let Some(arg) = args[index].to_str() else {
            return false;
        };
        match arg {
            "--" => return target && all_utf8(&args[index + 1..]),
            "-t" => {
                index += 1;
                let Some(value) = args.get(index).and_then(|arg| arg.to_str()) else {
                    return false;
                };
                let _ = value;
                target = true;
            }
            "-F" | "-H" | "-l" | "-K" | "-M" | "-p" | "-R" | "-X" | "-N" | "-c" => {
                return false;
            }
            value if value.starts_with('-') => return false,
            _ => return target && all_utf8(&args[index..]),
        }
        index += 1;
    }
    target
}

fn source_file_should_use_tiny(args: &[OsString]) -> bool {
    if std::env::var_os("RMUX").is_some_and(|value| !value.is_empty())
        || std::env::var_os("TMUX").is_some_and(|value| !value.is_empty())
        || has_queue_separator(args)
    {
        return false;
    }

    let Some(first) = args.first().and_then(|arg| arg.to_str()) else {
        return false;
    };
    let paths = match first {
        "--" => &args[1..],
        "-F" | "-n" | "-q" | "-v" | "-t" => return false,
        value if value.starts_with('-') => return false,
        _ => args,
    };
    !paths.is_empty()
        && paths
            .iter()
            .all(|path| path.to_str().is_some_and(|path| path != "-"))
}

mod commands;
mod fuzz;
mod responses;
mod routing;
#[cfg(unix)]
mod runtime_alias_perf;
mod targets;
#[cfg(windows)]
mod windows;
