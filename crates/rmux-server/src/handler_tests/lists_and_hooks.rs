use super::*;

#[tokio::test]
async fn list_sessions_returns_empty_output_when_no_sessions_exist() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ListSessions(ListSessionsRequest {
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;

    let output = response
        .command_output()
        .expect("list-sessions returns command output");
    assert!(output.stdout().is_empty());
}

#[tokio::test]
async fn list_sessions_sorts_sessions_by_name() {
    let handler = RequestHandler::new();
    for name in ["charlie", "alpha", "bravo"] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name(name),
                detached: true,
                size: None,

                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let response = handler
        .handle(Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;

    let output = response
        .command_output()
        .expect("list-sessions returns command output");
    assert_eq!(
        std::str::from_utf8(output.stdout()).expect("utf-8"),
        "alpha\nbravo\ncharlie\n"
    );
}

#[tokio::test]
async fn list_sessions_format_uses_each_sessions_active_pane_context() {
    let handler = RequestHandler::new();
    let root =
        std::env::temp_dir().join(format!("rmux-list-sessions-context-{}", std::process::id()));
    let alpha_dir = root.join("alpha");
    let beta_dir = root.join("beta");
    std::fs::create_dir_all(&alpha_dir).expect("create alpha dir");
    std::fs::create_dir_all(&beta_dir).expect("create beta dir");
    let alpha_dir = canonical_context_path(&alpha_dir);
    let beta_dir = canonical_context_path(&beta_dir);

    for (name, path) in [("alpha", &alpha_dir), ("beta", &beta_dir)] {
        let created = handler
            .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
                session_name: Some(session_name(name)),
                working_directory: Some(path.to_string_lossy().into_owned()),
                detached: true,
                size: None,
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            })))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let response = handler
        .handle(Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}|#{session_path}|#{pane_current_path}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await;

    let output = response
        .command_output()
        .expect("list-sessions returns command output");
    let stdout = std::str::from_utf8(output.stdout()).expect("utf-8");
    assert_eq!(
        stdout,
        format!(
            "alpha|{}|{}\nbeta|{}|{}\n",
            rendered_context_path(&alpha_dir),
            rendered_context_path(&alpha_dir),
            rendered_context_path(&beta_dir),
            rendered_context_path(&beta_dir)
        )
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn session_path_stays_at_session_cwd_when_pane_cwds_differ() {
    let handler = RequestHandler::new();
    let root =
        std::env::temp_dir().join(format!("rmux-session-path-context-{}", std::process::id()));
    let session_dir = root.join("session");
    let split_dir = root.join("split");
    std::fs::create_dir_all(&session_dir).expect("create session dir");
    std::fs::create_dir_all(&split_dir).expect("create split dir");
    let session_dir = canonical_context_path(&session_dir);
    let split_dir = canonical_context_path(&split_dir);
    let session = session_name("session-path-context");

    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: Some(session_dir.to_string_lossy().into_owned()),
            detached: true,
            size: None,
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)), "{created:?}");

    let split = handler
        .handle(Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: None,
                start_directory: Some(split_dir.clone()),
                keep_alive_on_exit: None,
                detached: true,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    let response = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: session,
            format: Some("#{pane_index}|#{session_path}|#{pane_current_path}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = response
        .command_output()
        .expect("list-panes returns command output");
    let stdout = std::str::from_utf8(output.stdout()).expect("utf-8");
    assert_eq!(
        stdout,
        format!(
            "0|{session_path}|{session_path}\n1|{session_path}|{split_path}\n",
            session_path = rendered_context_path(&session_dir),
            split_path = rendered_context_path(&split_dir),
        )
    );

    let _ = std::fs::remove_dir_all(root);
}

fn canonical_context_path(path: &std::path::Path) -> std::path::PathBuf {
    let canonical = std::fs::canonicalize(path).expect("canonicalize context directory");
    #[cfg(windows)]
    {
        let rendered = canonical.to_string_lossy();
        if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = rendered.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(rest);
        }
    }
    canonical
}

fn rendered_context_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

#[tokio::test]
async fn list_panes_returns_error_for_missing_session() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: session_name("missing"),
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );
}

fn format_value<'a>(formats: &'a [(String, String)], name: &str) -> Option<&'a str> {
    formats
        .iter()
        .rev()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn after_hook_formats_preserve_repeated_flag_values() {
    let parsed =
        parse_command_string("new-window -d -e FOO=1 -e BAR=2 -t alpha").expect("command parses");
    let command = parsed.commands().first().expect("one command");

    let formats = after_hook_format_values(HookName::AfterNewWindow, Some(command));

    assert_eq!(format_value(&formats, "hook"), Some("after-new-window"));
    assert_eq!(
        format_value(&formats, "hook_arguments"),
        Some("-d -e FOO=1 -e BAR=2 -t alpha")
    );
    assert_eq!(format_value(&formats, "hook_flag_d"), Some("1"));
    assert_eq!(format_value(&formats, "hook_flag_e"), Some("BAR=2"));
    assert_eq!(format_value(&formats, "hook_flag_e_0"), Some("FOO=1"));
    assert_eq!(format_value(&formats, "hook_flag_e_1"), Some("BAR=2"));
    assert_eq!(format_value(&formats, "hook_flag_t"), Some("alpha"));
    assert_eq!(format_value(&formats, "hook_flag_t_0"), Some("alpha"));
}
