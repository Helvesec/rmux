use super::*;

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
fn tiny_target_resolution_errors_require_canonical_helper_retry() {
    let invalid_target = Response::Error(ErrorResponse {
        error: RmuxError::InvalidTarget {
            value: "s:0.99".to_owned(),
            reason: "can't find pane: 99".to_owned(),
        },
    });
    let missing_session = Response::Error(ErrorResponse {
        error: RmuxError::SessionNotFound("bogus".to_owned()),
    });

    assert!(response_needs_session_resolution_retry(&invalid_target));
    assert!(response_needs_session_resolution_retry(&missing_session));
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

#[test]
fn legacy_q_versions_are_direct_but_downstream_headers_still_use_the_helper() {
    for args in [
        vec!["rmux", "-qV", "-x"],
        vec!["rmux", "-qq", "--version", "-h"],
        vec!["tmux", "-uq", "-V"],
    ] {
        assert!(matches!(
            TinyInvocation::parse(&os_args(&args)),
            TinyInvocation::Version
        ));
    }
    for args in [
        vec!["rmux", "-q", "list-sessions"],
        vec!["rmux", "-q", "--help"],
        vec!["tmux", "-q", "--version"],
        vec!["rmux", "-q", "-L"],
        vec!["rmux", "-q", "--", "-V"],
    ] {
        assert_tiny_fallback(&args);
    }
}

#[test]
fn send_keys_session_window_and_pane_targets_are_resolved_by_the_server() {
    for target in ["bench", "bench:0", "bench:0.0", "%12", "=bench", "prefix"] {
        let parsed = parse_send_keys(&os_args(&["-t", target, "Enter"])).expect("server target");
        assert_eq!(parsed.raw_target, target);
    }
}

#[test]
fn l1_040_mode_with_invalid_height_falls_back_to_full_helper() {
    for command in ["resize-pane", "resizep"] {
        for width in ["10", "25%"] {
            for flags in [
                &["-Z"][..],
                &["-T"][..],
                &["-Z", "-T"][..],
                &["-T", "-Z"][..],
                &[][..],
            ] {
                let mut argv = vec!["rmux", command, "-t", "compat:0.0", "-x", width, "-ybad"];
                argv.extend_from_slice(flags);
                assert_tiny_fallback(&argv);
                // The typed full-helper transport tests exercise this same argv
                // after canonicalization; no helper process is spawned here.
            }
        }
    }
}

#[test]
fn set_option_full_and_tiny_stop_flags_at_the_first_positional() {
    for command in ["set-option", "set-window-option"] {
        for tail in [
            vec!["-g", "status", "on", "-g"],
            vec!["-g", "status", "-g", "on"],
        ] {
            let argv = std::iter::once("rmux")
                .chain(std::iter::once(command))
                .chain(tail.iter().copied());
            assert!(crate::cli_args::parse(argv).is_err(), "{command} {tail:?}");
            assert!(
                parse_set_option(&os_args(&tail), command == "set-window-option").is_none(),
                "tiny must defer the same invalid argument tail: {command} {tail:?}",
            );
        }
        let tail = ["-g", "status", "on"];
        assert!(crate::cli_args::parse(["rmux", command, "-g", "status", "on"]).is_ok());
        assert!(parse_set_option(&os_args(&tail), command == "set-window-option").is_some());
    }
}

#[test]
fn repeated_targets_preserve_tiny_option_values_and_tail() {
    for owner in ["-n", "-c"] {
        for value in ["-t", "--"] {
            let args = os_args(&[
                "-d", owner, value, "-t", "first", "-t", "last", "printf", "-t", "shell",
            ]);
            let request = parse_new_window(&args).expect("supported tiny new-window");
            let actual = if owner == "-n" {
                request.name.as_deref()
            } else {
                request.start_directory.as_deref().and_then(|p| p.to_str())
            };
            assert_eq!(actual, Some(value));
            assert_eq!(request.target.as_str(), "last");
            assert_eq!(
                request.command,
                Some(vec!["printf".into(), "-t".into(), "shell".into()])
            );
        }
    }
    let window_targets = os_args(&["-n", "-t", "-t", "alpha:1", "-t", "alpha:2"]);
    assert!(
        parse_new_window(&window_targets).is_none(),
        "window targets use full helper"
    );
}
