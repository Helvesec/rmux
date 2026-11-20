use super::*;

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
fn split_window_keep_alive_form_is_tiny_parseable() {
    let args = os_args(&["-d", "-k", "-t", "0", "exit 7"]);

    let request = parse_split_window(&args).expect("split-window -k fast path");
    assert!(request.detached);
    assert_eq!(request.keep_alive_on_exit, Some(true));
    assert_eq!(request.command.as_deref(), Some(&["exit 7".into()][..]));
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
        ["-S", "-d", "-t", "bench"].as_slice(),
        ["-Sd", "-t", "bench"].as_slice(),
        ["-dS", "-t", "bench"].as_slice(),
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
        ["-gH"].as_slice(),
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
    for name in [
        "tree-mode-preview-format",
        "tree-mode-preview-style",
        "window-pane-status-format",
        "window-pane-current-status-format",
    ] {
        let value = if name.ends_with("style") {
            "fg=red"
        } else {
            ""
        };
        let parsed =
            parse_set_option(&os_args(&["-g", name, value]), false).expect("named tiny path");
        assert_eq!(parsed.option, name);
        assert_eq!(parsed.scope, OptionScopeSelector::WindowGlobal);
        assert_eq!(parsed.value, value);
        assert!(parse_set_option(&os_args(&["-g", "-F", name, value]), false).is_none());
    }
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
