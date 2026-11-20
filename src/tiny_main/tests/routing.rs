use super::*;

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
        if *command == "source-file" {
            assert_source_file_dispatch(&shape);
        } else {
            assert_tiny_direct(&shape, command);
        }
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
fn repeated_idempotent_top_level_flags_keep_tiny_on_helper_path() {
    for args in [
        ["rmux", "-2", "-2", "list-sessions"].as_slice(),
        ["rmux", "-22", "--", "list-sessions"].as_slice(),
        ["rmux", "-D", "-D"].as_slice(),
        ["rmux", "-DD", "--"].as_slice(),
        ["rmux", "-l", "-l", "list-sessions"].as_slice(),
        ["rmux", "-ll", "--", "list-sessions"].as_slice(),
        ["rmux", "-N", "-N", "list-sessions"].as_slice(),
        ["rmux", "-NN", "--", "list-sessions"].as_slice(),
        ["rmux", "-u", "-u", "list-sessions"].as_slice(),
        ["rmux", "-uu", "--", "list-sessions"].as_slice(),
        ["rmux", "-u2lN", "-Nlu2", "--", "list-sessions"].as_slice(),
        ["rmux", "-DD", "-uu", "-22", "-ll", "--"].as_slice(),
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
    assert_source_file_dispatch(&["rmux", "source-file", "/tmp/rmux-source.conf"]);
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
