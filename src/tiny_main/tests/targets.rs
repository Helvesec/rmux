use super::*;

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
fn has_session_raw_targets_preserve_direct_and_helper_routing() {
    for command in ["has-session", "has"] {
        for target in ["bench", "other_session", "session-2"] {
            let argv = ["rmux", command, "-t", target];
            let TinyInvocation::Direct(parsed) = TinyInvocation::parse(&os_args(&argv)) else {
                panic!("{argv:?} should use the tiny has-session path");
            };
            let TinyCommand::HasSession { request, .. } = *parsed else {
                panic!("{argv:?} should parse as has-session");
            };
            assert_eq!(request.target.to_string(), target);
        }
        assert_tiny_direct(&["rmux", command, "-t", "."], "has-session");
        // The canonical parser now keeps raw strings. Empty targets, selectors
        // and invalid targets still need the full helper's resolution.
        for target in ["", ":", "=", "=bench", "$42", "bench:0", "bench:0.0"] {
            assert_tiny_fallback(&["rmux", command, "-t", target]);
        }
    }
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
fn resize_pane_position_delegates_to_the_full_cli() {
    assert!(parse_resize_pane(&os_args(&["-t", "0", "-X", "3"])).is_none());
    assert_tiny_fallback(&["resizep", "-t", "0", "-Y", "-2"]);
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
        ["--json", "-a"].as_slice(),
        ["--json", "-I"].as_slice(),
        ["--json", "-l", "#{pane_id}"].as_slice(),
        ["--json", "-v", "#{pane_id}"].as_slice(),
        ["-p", "-F", "#{pane_id}", "#{pane_id}"].as_slice(),
        ["-p", "-t", "%1", "#{pane_id}"].as_slice(),
        ["-p", "#{pane_id}", ";", "list-sessions"].as_slice(),
        ["-p", "echo;", "list-sessions"].as_slice(),
        ["-p", "one", "two"].as_slice(),
        ["-p", "--", "one", "two"].as_slice(),
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
    assert_eq!(request.raw_target, "bench:0.0");
    assert_eq!(request.keys, ["true".to_owned(), "Enter".to_owned()]);
}

#[test]
fn send_keys_resolution_forwards_errors_before_any_mutation() {
    let target = match Target::parse("bench:0.0").expect("exact pane target") {
        Target::Pane(target) => target,
        other => panic!("expected pane target, got {other:?}"),
    };
    let resolved = Response::ResolveTarget(rmux_proto::ResolveTargetResponse {
        target: Target::Pane(target.clone()),
    });
    assert_eq!(send_keys_resolved_target(resolved), Ok(target));

    let error = Response::Error(ErrorResponse {
        error: RmuxError::SessionNotFound("missing".to_owned()),
    });
    assert_eq!(
        send_keys_resolved_target(error),
        Err("session not found: missing".to_owned())
    );

    let wrong_target = Response::ResolveTarget(rmux_proto::ResolveTargetResponse {
        target: Target::Session(rmux_proto::SessionName::new("bench").expect("session name")),
    });
    assert_eq!(
        send_keys_resolved_target(wrong_target),
        Err("protocol error: resolve-target returned a non-pane target for send-keys".to_owned())
    );
}

#[test]
fn send_keys_complex_forms_fall_back_to_helper() {
    for args in [
        ["true", "Enter"].as_slice(),
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
fn source_file_plain_paths_follow_nested_context_policy() {
    let args = os_args(&["/tmp/rmux-source-a.conf", "/tmp/rmux-source-b.conf"]);

    if source_file_should_use_tiny(&args) {
        let request = parse_source_file(&args).expect("source-file fast path");
        assert_eq!(
            request.paths,
            [
                "/tmp/rmux-source-a.conf".to_owned(),
                "/tmp/rmux-source-b.conf".to_owned()
            ]
        );
    } else {
        assert!(parse_source_file(&args).is_none());
    }
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
fn resize_required_dimensions_preserve_tiny_option_shaped_targets() {
    for (arguments, target) in [
        (vec!["-x", "20", "-t", "-y"], "-y"),
        (vec!["-x", "20", "-t", "-x"], "-x"),
        (vec!["-x", "20", "-t", "--"], "--"),
    ] {
        let request = parse_resize_pane(&os_args(&arguments))
            .unwrap_or_else(|| panic!("tiny resize rejected {arguments:?}"));
        assert_eq!(request.target.as_deref(), Some(target));
        assert_eq!(
            request.adjustment,
            ResizePaneAdjustment::AbsoluteWidth { columns: 20 }
        );
    }
}

#[test]
fn resize_required_dimensions_compact_and_invalid_tiny_fallback() {
    for arguments in [
        vec!["-x20", "-t-y"],
        vec!["-x"],
        vec!["-y"],
        vec!["-x", "20", "--", "-y"],
        vec!["3", "-y"],
    ] {
        assert!(
            parse_resize_pane(&os_args(&arguments)).is_none(),
            "{arguments:?}"
        );
    }
}

#[test]
fn backslash_session_targets_keep_their_stored_text() {
    // `new -s 'b\c'` stores `b\\c`; tmux 3.7b and the full CLI find it with
    // `-t 'b\\c'`, so the tiny client must not escape that text again.
    let argv = ["rmux", "has-session", "-t", "b\\\\c"];
    let TinyInvocation::Direct(parsed) = TinyInvocation::parse(&os_args(&argv)) else {
        panic!("{argv:?} should use the tiny has-session path");
    };
    let TinyCommand::HasSession { request, .. } = *parsed else {
        panic!("{argv:?} should parse as has-session");
    };
    assert_eq!(request.target.as_str(), "b\\\\c");
}
