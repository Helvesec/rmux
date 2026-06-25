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
    let alpha_dir = std::fs::canonicalize(&alpha_dir).expect("canonicalize alpha dir");
    let beta_dir = std::fs::canonicalize(&beta_dir).expect("canonicalize beta dir");

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

fn rendered_context_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

#[tokio::test]
async fn list_panes_returns_error_for_missing_session() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: session_name("missing"),
            format: None,
            target_window_index: None,
        }))
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
