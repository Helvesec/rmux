use super::*;
use parse::{
    has_queue_separator, parse_display_message, parse_has_session, parse_join_pane,
    parse_kill_pane, parse_kill_session, parse_list_panes, parse_list_windows, parse_new_session,
    parse_new_window, parse_rename_window, parse_resize_pane, parse_select_window, parse_send_keys,
    parse_set_option, parse_show_options, parse_source_file, parse_split_window,
};
#[cfg(windows)]
use parse::{windows_client_shell_for_parent_name, windows_invoking_client_environment};
use rmux_proto::{
    ErrorResponse, ResizePaneAdjustment, Response, RmuxError, SourceFileResponse, SplitDirection,
    Target,
};

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn assert_tiny_direct(args: &[&str], command: &str) {
    match TinyInvocation::parse(&os_args(args)) {
        TinyInvocation::Direct(parsed) => assert_eq!(parsed.name(), command),
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
fn long_version_stays_on_full_helper_path_for_tmux_compatibility() {
    assert_tiny_fallback(&["rmux", "--version"]);
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
                if args[index + 1..].is_empty() || !all_utf8(&args[index + 1..]) {
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
                if !all_utf8(&args[index..]) {
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
                if !exact_pane_target(value) {
                    return false;
                }
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

#[test]
fn split_window_with_command_is_tiny_parseable() {
    let args = os_args(&["-h", "-t", "0", "sleep", "1"]);

    let request = parse_split_window(&args).expect("split-window command fast path");
    assert!(matches!(request.direction, SplitDirection::Horizontal));
    assert_eq!(request.target.as_deref(), Some("0"));
    assert_eq!(
        request.command.as_deref(),
        Some(&["sleep".into(), "1".into()][..])
    );
}

#[test]
fn split_window_simple_form_is_tiny_parseable() {
    let args = os_args(&["-h", "-d", "-t", "0"]);

    let request = parse_split_window(&args).expect("simple split-window fast path");
    assert!(matches!(request.direction, SplitDirection::Horizontal));
    assert!(request.detached);
    assert_eq!(request.target.as_deref(), Some("0"));
    assert!(request.command.is_none());
    assert!(request.process_command.is_none());
}

#[test]
fn new_session_detached_simple_form_is_tiny_parseable() {
    let args = os_args(&["-d", "-s", "bench", "sleep", "1"]);

    let request = parse_new_session(&args).expect("new-session detached fast path");
    assert!(request.detached);
    assert_eq!(
        request
            .session_name
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("bench")
    );
    assert_eq!(
        request.command.as_deref(),
        Some(&["sleep".into(), "1".into()][..])
    );
}

#[test]
fn new_window_detached_simple_form_is_tiny_parseable() {
    let args = os_args(&["-d", "-n", "bench-window", "-t", "bench", "sleep", "1"]);

    let request = parse_new_window(&args).expect("new-window detached fast path");
    assert!(request.detached);
    assert_eq!(request.target.to_string(), "bench");
    assert_eq!(request.name.as_deref(), Some("bench-window"));
    assert_eq!(
        request.command.as_deref(),
        Some(&["sleep".into(), "1".into()][..])
    );
}

#[test]
fn new_window_complex_forms_stay_on_full_helper_path() {
    for args in [
        ["-P", "-d", "-t", "bench"].as_slice(),
        ["-d", "-t", "bench:1"].as_slice(),
        ["-d", "-t", "%1"].as_slice(),
    ] {
        assert!(parse_new_window(&os_args(args)).is_none());
    }
}

#[test]
fn kill_session_simple_form_is_tiny_parseable() {
    let args = os_args(&["-t", "bench"]);

    let request = parse_kill_session(&args).expect("kill-session fast path");
    assert_eq!(request.target.to_string(), "bench");
}

#[test]
fn kill_session_complex_forms_stay_on_full_helper_path() {
    for args in [
        ["-a", "-t", "bench"].as_slice(),
        ["-t", "$bench"].as_slice(),
    ] {
        assert!(parse_kill_session(&os_args(args)).is_none());
    }
}

#[test]
fn show_options_global_simple_forms_are_tiny_parseable() {
    assert!(parse_show_options(&os_args(&["-g"]), false).is_some());
    assert!(parse_show_options(&os_args(&["-g"]), true).is_some());
}

#[test]
fn show_options_complex_forms_stay_on_full_helper_path() {
    for args in [
        [].as_slice(),
        ["-g", "status"].as_slice(),
        ["-g", "-v"].as_slice(),
        ["-w"].as_slice(),
    ] {
        assert!(parse_show_options(&os_args(args), false).is_none());
    }
}

#[test]
fn window_mutation_simple_forms_are_tiny_parseable() {
    let rename = parse_rename_window(&os_args(&["-t", "bench:0", "renamed"]))
        .expect("rename-window fast path");
    assert_eq!(rename.target.to_string(), "bench:0");
    assert_eq!(rename.name, "renamed");

    let select =
        parse_select_window(&os_args(&["-t", "bench:1"])).expect("select-window fast path");
    assert_eq!(select.target.to_string(), "bench:1");
}

#[test]
fn window_mutation_complex_forms_stay_on_full_helper_path() {
    for args in [
        ["-t", "bench", "renamed"].as_slice(),
        ["-t", "%1", "renamed"].as_slice(),
        ["-t", "bench:0", "renamed", "extra"].as_slice(),
    ] {
        assert!(parse_rename_window(&os_args(args)).is_none());
    }

    for args in [
        ["-n"].as_slice(),
        ["-t", "bench"].as_slice(),
        ["-t", "%1"].as_slice(),
    ] {
        assert!(parse_select_window(&os_args(args)).is_none());
    }
}

#[test]
fn kill_pane_simple_form_is_tiny_parseable() {
    let request = parse_kill_pane(&os_args(&["-t", "bench:0.1"])).expect("kill-pane fast path");
    assert_eq!(request.target.to_string(), "bench:0.1");
}

#[test]
fn kill_pane_complex_forms_stay_on_full_helper_path() {
    for args in [
        ["-a", "-t", "bench:0.1"].as_slice(),
        ["-t", "bench:0"].as_slice(),
        ["-t", "%1"].as_slice(),
    ] {
        assert!(parse_kill_pane(&os_args(args)).is_none());
    }
}

#[test]
fn join_pane_simple_form_is_tiny_parseable() {
    let request = parse_join_pane(&os_args(&["-d", "-s", "bench:1.0", "-t", "bench:0.0"]))
        .expect("join-pane fast path");
    assert!(request.detached);
    assert_eq!(request.source.to_string(), "bench:1.0");
    assert_eq!(request.target.to_string(), "bench:0.0");
}

#[test]
fn join_pane_complex_forms_stay_on_full_helper_path() {
    for args in [
        ["-h", "-d", "-s", "bench:1.0", "-t", "bench:0.0"].as_slice(),
        ["-d", "-s", "bench:1", "-t", "bench:0.0"].as_slice(),
        ["-d", "-s", "%1", "-t", "bench:0.0"].as_slice(),
    ] {
        assert!(parse_join_pane(&os_args(args)).is_none());
    }
}

#[test]
fn set_option_simple_global_forms_are_tiny_parseable() {
    let session =
        parse_set_option(&os_args(&["-g", "status", "on"]), false).expect("set-option fast path");
    assert_eq!(session.option, "status");
    assert_eq!(session.value, "on");

    let window = parse_set_option(&os_args(&["-g", "automatic-rename", "off"]), true)
        .expect("set-window-option fast path");
    assert_eq!(window.option, "automatic-rename");
    assert_eq!(window.value, "off");
}

#[test]
fn set_option_complex_forms_stay_on_full_helper_path() {
    for args in [
        ["status", "on"].as_slice(),
        ["-g", "status"].as_slice(),
        ["-g", "-q", "status", "on"].as_slice(),
        ["-t", "bench", "status", "on"].as_slice(),
    ] {
        assert!(parse_set_option(&os_args(args), false).is_none());
    }
}

#[test]
fn tiny_tmux_invocation_is_case_insensitive() {
    assert!(invoked_as_tmux_from(
        &os_args(&["Tmux", "list-sessions"]),
        None,
    ));
    assert!(invoked_as_tmux_from(
        &os_args(&["TMUX", "list-sessions"]),
        None,
    ));
}

#[test]
fn tiny_tmux_invocation_honors_internal_override() {
    assert!(invoked_as_tmux_from(
        &os_args(&["rmux", "list-sessions"]),
        Some(OsStr::new("1")),
    ));
    for value in ["", "0", "true", "yes"] {
        assert!(
            !invoked_as_tmux_from(
                &os_args(&["rmux", "list-sessions"]),
                Some(OsStr::new(value))
            ),
            "internal tmux override should require exactly 1, got {value:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn new_session_tiny_windows_replaces_inherited_client_shell_hint() {
    let environment = windows_invoking_client_environment(
        [
            (OsString::from("Path"), OsString::from("C:\\bin")),
            (
                OsString::from("RMUX_CLIENT_SHELL"),
                OsString::from("stale.exe"),
            ),
            (
                OsString::from("RMUX_INTERNAL_INVOKED_AS_TMUX"),
                OsString::from("1"),
            ),
            (
                OsString::from("RMUX_INTERNAL_CLIENT_SHELL"),
                OsString::from("stale-internal.exe"),
            ),
            (
                OsString::from("USERPROFILE"),
                OsString::from("C:\\Users\\Shadow"),
            ),
        ],
        Some("pwsh.exe".to_owned()),
    );

    assert!(environment.iter().any(|entry| entry == "Path=C:\\bin"));
    assert!(environment
        .iter()
        .any(|entry| entry == "USERPROFILE=C:\\Users\\Shadow"));
    assert!(environment
        .iter()
        .any(|entry| entry == "RMUX_CLIENT_SHELL=pwsh.exe"));
    assert!(!environment
        .iter()
        .any(|entry| entry == "RMUX_CLIENT_SHELL=stale.exe"));
    assert!(!environment
        .iter()
        .any(|entry| entry.starts_with("RMUX_INTERNAL_CLIENT_SHELL=")));
    assert!(!environment
        .iter()
        .any(|entry| entry.starts_with("RMUX_INTERNAL_INVOKED_AS_TMUX=")));
}

#[cfg(windows)]
#[test]
fn tiny_windows_client_shell_mapping_matches_full_cli_surface() {
    let expected_cmd = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned());
    assert_eq!(
        windows_client_shell_for_parent_name("cmd.exe").as_deref(),
        Some(expected_cmd.as_str())
    );
    assert_eq!(
        windows_client_shell_for_parent_name("pwsh.exe").as_deref(),
        Some("pwsh.exe")
    );
    assert_eq!(
        windows_client_shell_for_parent_name("bash.exe").as_deref(),
        Some("bash.exe")
    );
    assert_eq!(windows_client_shell_for_parent_name("unknown.exe"), None);
}

#[test]
fn new_session_attached_stays_on_full_helper_path() {
    let args = os_args(&["-s", "bench"]);

    assert!(parse_new_session(&args).is_none());
}

#[test]
fn has_session_exact_target_is_tiny_parseable() {
    let args = os_args(&["-t", "bench"]);

    let request = parse_has_session(&args).expect("has-session fast path");
    assert_eq!(request.target.to_string(), "bench");
}

#[test]
fn has_session_complex_forms_fall_back_to_helper() {
    for args in [
        [].as_slice(),
        ["-t", "bench", "-t", "other"].as_slice(),
        ["-t", "bench:0"].as_slice(),
        ["-t", "$1"].as_slice(),
        ["-t", "bench", ";", "list-sessions"].as_slice(),
    ] {
        assert!(
            parse_has_session(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn list_windows_exact_session_target_is_tiny_parseable() {
    let args = os_args(&["-t", "bench"]);

    let request = parse_list_windows(&args).expect("list-windows fast path");
    assert_eq!(request.target.to_string(), "bench");
}

#[test]
fn list_windows_complex_forms_fall_back_to_helper() {
    for args in [
        [].as_slice(),
        ["-a"].as_slice(),
        ["-t", "bench", "-t", "other"].as_slice(),
        ["-t", "bench:0"].as_slice(),
        ["-t", "bench", "-F", "#{window_id}"].as_slice(),
        ["-t", "bench", "--json"].as_slice(),
        ["-t", "bench", ";", "list-sessions"].as_slice(),
    ] {
        assert!(
            parse_list_windows(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn list_panes_exact_targets_are_tiny_parseable() {
    let session =
        parse_list_panes(&os_args(&["-t", "bench"])).expect("list-panes session fast path");
    match session {
        TinyListPanes::Target {
            target,
            target_window_index,
        } => {
            assert_eq!(target.to_string(), "bench");
            assert_eq!(target_window_index, None);
        }
        TinyListPanes::AllSessions => panic!("expected targeted list-panes"),
    }

    let window =
        parse_list_panes(&os_args(&["-t", "bench:2"])).expect("list-panes window fast path");
    match window {
        TinyListPanes::Target {
            target,
            target_window_index,
        } => {
            assert_eq!(target.to_string(), "bench");
            assert_eq!(target_window_index, Some(2));
        }
        TinyListPanes::AllSessions => panic!("expected targeted list-panes"),
    }

    let pane = parse_list_panes(&os_args(&["-t", "bench:3.4"])).expect("list-panes pane fast path");
    match pane {
        TinyListPanes::Target {
            target,
            target_window_index,
        } => {
            assert_eq!(target.to_string(), "bench");
            assert_eq!(target_window_index, Some(3));
        }
        TinyListPanes::AllSessions => panic!("expected targeted list-panes"),
    }
}

#[test]
fn list_panes_all_sessions_is_tiny_parseable() {
    assert!(matches!(
        parse_list_panes(&os_args(&["-a"])),
        Some(TinyListPanes::AllSessions)
    ));
}

#[test]
fn list_panes_complex_forms_fall_back_to_helper() {
    for args in [
        [].as_slice(),
        ["-a", "-t", "bench"].as_slice(),
        ["-s", "-t", "bench"].as_slice(),
        ["-t", "bench", "-t", "other"].as_slice(),
        ["-t", "$1"].as_slice(),
        ["-t", "bench", "-F", "#{pane_id}"].as_slice(),
        ["-t", "bench", "--json"].as_slice(),
        ["-t", "bench", ";", "list-sessions"].as_slice(),
    ] {
        assert!(
            parse_list_panes(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn resize_pane_relative_numeric_delta_is_tiny_parseable() {
    let args = os_args(&["-t", "0", "-R", "10"]);

    let request = parse_resize_pane(&args).expect("resize-pane relative delta fast path");
    assert_eq!(request.target.as_deref(), Some("0"));
    assert!(matches!(
        request.adjustment,
        ResizePaneAdjustment::Right { cells: 10 }
    ));
}

#[test]
fn resize_pane_conflicting_adjustments_fall_back_to_helper() {
    for args in [["-R", "10", "-t", "0"].as_slice(), ["-R", "0"].as_slice()] {
        assert!(
            parse_resize_pane(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn resize_pane_composed_adjustments_stay_tiny_parseable() {
    let relative = parse_resize_pane(&os_args(&["-R", "-L", "-t", "0"]))
        .expect("priority-collapsed relative resize");
    assert!(matches!(
        relative.adjustment,
        ResizePaneAdjustment::Left { cells: 1 }
    ));

    let composed = parse_resize_pane(&os_args(&["-x", "80", "-R", "-t", "0"]))
        .expect("absolute plus relative resize");
    assert!(matches!(
        composed.adjustment,
        ResizePaneAdjustment::Composite {
            columns: Some(80),
            rows: None,
            relative: Some(rmux_proto::ResizePaneRelativeDirection::Right),
            cells: 1
        }
    ));

    let zoom =
        parse_resize_pane(&os_args(&["-Z", "-R", "-t", "0"])).expect("zoom with extra adjustment");
    assert_eq!(zoom.adjustment, ResizePaneAdjustment::Zoom);
}

#[test]
fn resize_pane_absolute_size_remains_tiny_parseable() {
    let args = os_args(&["-x", "100", "-y", "30", "-t", "0"]);

    let request = parse_resize_pane(&args).expect("resize-pane absolute size fast path");
    assert_eq!(request.target.as_deref(), Some("0"));
    assert!(matches!(
        request.adjustment,
        ResizePaneAdjustment::AbsoluteSize {
            columns: 100,
            rows: 30
        }
    ));
}

#[test]
fn display_message_print_exact_target_is_tiny_parseable() {
    let args = os_args(&["-p", "-t", "bench", "#{session_name}"]);

    let request = parse_display_message(&args).expect("display-message print fast path");
    assert!(matches!(request.target, Some(Target::Session(_))));
    assert_eq!(request.message.as_deref(), Some("#{session_name}"));
}

#[test]
fn display_message_complex_forms_fall_back_to_helper() {
    for args in [
        ["-t", "bench:0.0", "#{pane_id}"].as_slice(),
        ["-p", "-t", "bench:0", "#{session_name}"].as_slice(),
        ["-p", "-t", "bench:0.0", "#{session_name}"].as_slice(),
        ["-p", "-c", "client", "#{pane_id}"].as_slice(),
        ["-p", "-v", "#{pane_id}"].as_slice(),
        ["-p", "--json", "#{pane_id}"].as_slice(),
        ["-p", "-F", "#{pane_id}", "#{pane_id}"].as_slice(),
        ["-p", "-t", "%1", "#{pane_id}"].as_slice(),
        ["-p", "#{pane_id}", ";", "list-sessions"].as_slice(),
        ["-p", "echo;", "list-sessions"].as_slice(),
    ] {
        assert!(
            parse_display_message(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn send_keys_exact_pane_target_is_tiny_parseable() {
    let args = os_args(&["-t", "bench:0.0", "true", "Enter"]);

    let request = parse_send_keys(&args).expect("send-keys fast path");
    assert_eq!(request.target.to_string(), "bench:0.0");
    assert_eq!(request.keys, ["true".to_owned(), "Enter".to_owned()]);
}

#[test]
fn send_keys_complex_forms_fall_back_to_helper() {
    for args in [
        ["true", "Enter"].as_slice(),
        ["-t", "bench", "true", "Enter"].as_slice(),
        ["-t", "bench:0", "true", "Enter"].as_slice(),
        ["-t", "bench:0.0", "-l", "abc"].as_slice(),
        ["-t", "bench:0.0", "-X", "copy-selection"].as_slice(),
        ["-c", "client", "-t", "bench:0.0", "Enter"].as_slice(),
        ["-t", "bench:0.0", "Enter", ";", "list-sessions"].as_slice(),
        ["-t", "bench:0.0", "echo;", "list-sessions"].as_slice(),
    ] {
        assert!(
            parse_send_keys(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn source_file_plain_paths_are_tiny_parseable() {
    let args = os_args(&["/tmp/rmux-source-a.conf", "/tmp/rmux-source-b.conf"]);

    let request = parse_source_file(&args).expect("source-file fast path");
    assert_eq!(
        request.paths,
        [
            "/tmp/rmux-source-a.conf".to_owned(),
            "/tmp/rmux-source-b.conf".to_owned()
        ]
    );
}

#[test]
fn source_file_complex_forms_fall_back_to_helper() {
    for args in [
        ["-"].as_slice(),
        ["-q", "/tmp/rmux-source.conf"].as_slice(),
        ["-F", "/tmp/#{session_name}.conf"].as_slice(),
        ["-t", "bench:0.0", "/tmp/rmux-source.conf"].as_slice(),
        ["--", "-"].as_slice(),
        ["/tmp/rmux-source.conf", ";", "list-sessions"].as_slice(),
        ["/tmp/rmux-source.conf;", "list-sessions"].as_slice(),
    ] {
        assert!(
            parse_source_file(&os_args(args)).is_none(),
            "{args:?} should stay on the canonical helper path"
        );
    }
}

#[test]
fn display_message_tiny_parser_fuzz_stays_on_allowlist() {
    let pool = [
        "-p",
        "-t",
        "bench",
        "bench:0",
        "bench:0.0",
        "%1",
        "-F",
        "#{pane_id}",
        "#{session_name}:#{window_index}.#{pane_index}",
        "literal",
        "-c",
        "client",
        "-d",
        "10",
        "-a",
        "-I",
        "-l",
        "-N",
        "-v",
        "--json",
        "--",
        ";",
        "-hyphen-message",
    ];
    let mut rng = FuzzRng::new(0xD15F_1A7E_2026_0700);

    for _ in 0..10_000 {
        let args = fuzz_args(&mut rng, &pool, 8);
        assert_eq!(
            parse_display_message(&args).is_some(),
            display_message_should_use_tiny(&args),
            "unexpected display-message tiny classification for {args:?}"
        );
    }
}

#[test]
fn send_keys_tiny_parser_fuzz_stays_on_allowlist() {
    let pool = [
        "-t",
        "bench",
        "bench:0",
        "bench:0.0",
        "%1",
        "true",
        "Enter",
        "C-c",
        "--",
        "-literal-key",
        "-F",
        "-H",
        "-l",
        "-K",
        "-M",
        "-p",
        "-R",
        "-X",
        "-N",
        "2",
        "-c",
        "client",
        ";",
    ];
    let mut rng = FuzzRng::new(0x5EED_5EAD_2026_0700);

    for _ in 0..10_000 {
        let args = fuzz_args(&mut rng, &pool, 8);
        assert_eq!(
            parse_send_keys(&args).is_some(),
            send_keys_should_use_tiny(&args),
            "unexpected send-keys tiny classification for {args:?}"
        );
    }
}

#[test]
fn source_file_tiny_parser_fuzz_stays_on_allowlist() {
    let pool = [
        "/tmp/rmux-a.conf",
        "/tmp/rmux-b.conf",
        "relative.conf",
        "/tmp/#{session_name}.conf",
        "-",
        "-q",
        "-F",
        "-n",
        "-v",
        "-t",
        "bench:0.0",
        "--",
        "-dash.conf",
        ";",
    ];
    let mut rng = FuzzRng::new(0x50C0_2026_0700);

    for _ in 0..10_000 {
        let args = fuzz_args(&mut rng, &pool, 8);
        assert_eq!(
            parse_source_file(&args).is_some(),
            source_file_should_use_tiny(&args),
            "unexpected source-file tiny classification for {args:?}"
        );
    }
}

#[test]
fn automation_extensions_stay_on_full_helper_path() {
    for command in [
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
    ] {
        assert_tiny_fallback(&["rmux", command, "--help"]);
    }
}

#[test]
fn send_keys_wait_extensions_stay_on_full_helper_path() {
    for args in [
        [
            "rmux",
            "send-keys",
            "-t",
            "bench:0.0",
            "--wait",
            "quiet",
            "--",
            "true",
            "Enter",
        ]
        .as_slice(),
        [
            "rmux",
            "send-keys",
            "-t",
            "bench:0.0",
            "--wait-next-text",
            "done",
            "--",
            "true",
            "Enter",
        ]
        .as_slice(),
        [
            "rmux",
            "send-keys",
            "-t",
            "bench:0.0",
            "--wait-pane-exit",
            "--",
            "exit",
            "Enter",
        ]
        .as_slice(),
    ] {
        assert_tiny_fallback(args);
    }
}

#[test]
fn benchmark_shapes_stay_direct_with_socket_selectors() {
    let explicit_socket_path = explicit_socket_path();
    let direct_shapes = [
        vec!["rmux", "-L", "benchsock", "start-server"],
        vec!["rmux", "-L", "benchsock", "list-sessions"],
        vec!["rmux", "-S", explicit_socket_path.as_str(), "list-sessions"],
        vec!["rmux", "-L", "benchsock", "has-session", "-t", "bench"],
        vec!["rmux", "-L", "benchsock", "list-windows", "-t", "bench"],
        vec!["rmux", "-L", "benchsock", "list-panes", "-t", "bench"],
        vec!["rmux", "-L", "benchsock", "list-panes", "-a"],
        vec!["rmux", "-L", "benchsock", "kill-server"],
        vec!["rmux", "-L", "benchsock", "capture-pane", "-p"],
        vec![
            "rmux",
            "-L",
            "benchsock",
            "display-message",
            "-p",
            "#{pane_id}",
        ],
        vec![
            "rmux",
            "-S",
            explicit_socket_path.as_str(),
            "send-keys",
            "-t",
            "bench:0.0",
            "true",
            "Enter",
        ],
        vec![
            "rmux",
            "-L",
            "benchsock",
            "source-file",
            "/tmp/rmux-source.conf",
        ],
    ];

    for shape in direct_shapes {
        let command = shape
            .iter()
            .find(|arg| {
                matches!(
                    **arg,
                    "start-server"
                        | "list-sessions"
                        | "has-session"
                        | "list-windows"
                        | "list-panes"
                        | "kill-server"
                        | "capture-pane"
                        | "display-message"
                        | "send-keys"
                        | "source-file"
                )
            })
            .expect("shape contains command");
        assert_tiny_direct(&shape, command);
    }
}

#[test]
fn unsupported_top_level_flags_keep_benchmark_shapes_on_helper_path() {
    for args in [
        [
            "rmux",
            "-f",
            "/tmp/rmux.conf",
            "display-message",
            "-p",
            "#{pane_id}",
        ]
        .as_slice(),
        ["rmux", "-2", "send-keys", "-t", "bench:0.0", "Enter"].as_slice(),
        ["rmux", "-u", "source-file", "/tmp/rmux-source.conf"].as_slice(),
    ] {
        assert_tiny_fallback(args);
    }
}

#[test]
fn readme_benchmark_shapes_stay_on_tiny_direct_paths() {
    assert_tiny_direct(&["rmux", "start-server"], "start-server");
    assert_tiny_direct(&["rmux", "list-sessions"], "list-sessions");
    assert_tiny_direct(&["rmux", "has-session", "-t", "bench"], "has-session");
    assert_tiny_direct(&["rmux", "list-windows", "-t", "bench"], "list-windows");
    assert_tiny_direct(&["rmux", "list-panes", "-t", "bench"], "list-panes");
    assert_tiny_direct(&["rmux", "list-panes", "-a"], "list-panes");
    assert_tiny_direct(&["rmux", "kill-server"], "kill-server");
    assert_tiny_direct(&["rmux", "capture-pane", "-p"], "capture-pane");
    assert_tiny_direct(&["rmux", "new-session", "-d", "-s", "bench"], "new-session");
    assert_tiny_direct(
        &["rmux", "split-window", "-h", "-d", "-t", "bench"],
        "split-window",
    );
    assert_tiny_direct(&["rmux", "new-window", "-d", "-t", "bench"], "new-window");
    assert_tiny_direct(
        &[
            "rmux",
            "new-window",
            "-d",
            "-n",
            "bench-window",
            "-t",
            "bench",
        ],
        "new-window",
    );
    assert_tiny_direct(
        &["rmux", "resize-pane", "-t", "bench", "-R", "10"],
        "resize-pane",
    );
    assert_tiny_direct(&["rmux", "kill-session", "-t", "bench"], "kill-session");
    assert_tiny_direct(&["rmux", "show-options", "-g"], "show-options");
    assert_tiny_direct(
        &["rmux", "show-window-options", "-g"],
        "show-window-options",
    );
    assert_tiny_direct(
        &["rmux", "rename-window", "-t", "bench:0", "renamed"],
        "rename-window",
    );
    assert_tiny_direct(&["rmux", "select-window", "-t", "bench:1"], "select-window");
    assert_tiny_direct(&["rmux", "kill-pane", "-t", "bench:0.1"], "kill-pane");
    assert_tiny_direct(
        &[
            "rmux",
            "join-pane",
            "-d",
            "-s",
            "bench:1.0",
            "-t",
            "bench:0.0",
        ],
        "join-pane",
    );
    assert_tiny_direct(&["rmux", "set-option", "-g", "status", "on"], "set-option");
    assert_tiny_direct(
        &["rmux", "set-window-option", "-g", "automatic-rename", "off"],
        "set-window-option",
    );
    assert_tiny_direct(
        &["rmux", "display-message", "-p", "#{session_name}"],
        "display-message",
    );
    assert_tiny_direct(
        &["rmux", "send-keys", "-t", "bench:0.0", "true", "Enter"],
        "send-keys",
    );
    assert_tiny_direct(
        &["rmux", "source-file", "/tmp/rmux-source.conf"],
        "source-file",
    );
    if std::env::var_os("RMUX").is_none() && std::env::var_os("TMUX").is_none() {
        assert_tiny_direct(&["rmux", "attach-session", "-t", "bench"], "attach-session");
    }
}

#[test]
fn queue_suffix_terminator_keeps_tiny_on_helper_path() {
    for args in [
        ["-h", "-t", "0", "echo;", "list-sessions"].as_slice(),
        [
            "-d",
            "-s",
            "bench",
            "sleep",
            "1;",
            "display-message",
            "-p",
            "hi",
        ]
        .as_slice(),
        ["-p", "-t", "bench:0.0", "echo\\;"].as_slice(),
    ] {
        let os_args = os_args(args);
        if args.iter().any(|arg| arg.ends_with("\\;")) {
            assert!(
                !has_queue_separator(&os_args),
                "{args:?} escaped semicolon should remain a command argument"
            );
        } else {
            assert!(
                has_queue_separator(&os_args),
                "{args:?} should be recognized as a queued command"
            );
        }
    }

    assert!(parse_split_window(&os_args(&["-h", "-t", "0", "echo;", "list-sessions"])).is_none());
    assert!(parse_new_session(&os_args(&[
        "-d",
        "-s",
        "bench",
        "sleep",
        "1;",
        "display-message",
        "-p",
        "hi"
    ]))
    .is_none());
}

#[test]
fn tiny_response_errors_use_tmux_cli_error_surface() {
    let response = Response::Error(ErrorResponse {
        error: RmuxError::InvalidTarget {
            value: "s:0.99".to_owned(),
            reason: "can't find pane: 99".to_owned(),
        },
    });

    assert_eq!(
        write_response_output_or_error(response, "split-window").unwrap_err(),
        "can't find pane: 99"
    );
}

#[test]
fn tiny_display_message_missing_explicit_target_uses_empty_context() {
    let invalid_target = Response::Error(ErrorResponse {
        error: RmuxError::InvalidTarget {
            value: "s:0.99".to_owned(),
            reason: "can't find pane: 99".to_owned(),
        },
    });
    let missing_session = Response::Error(ErrorResponse {
        error: RmuxError::SessionNotFound("bogus".to_owned()),
    });

    assert!(display_message_missing_target_uses_empty_context(
        &invalid_target
    ));
    assert!(display_message_missing_target_uses_empty_context(
        &missing_session
    ));
}

#[test]
fn source_file_tiny_response_preserves_exit_status() {
    let response = Response::SourceFile(SourceFileResponse::no_output().with_exit_status(Some(1)));

    assert_eq!(
        write_source_file_response(response).expect("source response"),
        1
    );
}

#[test]
fn source_file_tiny_line_errors_use_stdout_exit_status() {
    let response = Response::Error(ErrorResponse {
        error: RmuxError::Server("/tmp/rmux.conf:12: unknown command: bad".to_owned()),
    });

    assert_eq!(
        write_source_file_response(response).expect("source response"),
        1
    );
}

#[test]
fn custom_startup_config_keeps_new_session_on_full_helper_path() {
    assert_tiny_fallback(&[
        "rmux",
        "-f",
        "/tmp/rmux-test.conf",
        "new-session",
        "-d",
        "-s",
        "bench",
    ]);
}

#[test]
fn explicit_terminal_context_flags_keep_attach_on_full_helper_path() {
    assert_tiny_fallback(&["rmux", "-2", "attach-session"]);
    assert_tiny_fallback(&["rmux", "-T", "RGB", "attach-session"]);
    assert_tiny_fallback(&["rmux", "-u", "attach-session"]);
}
