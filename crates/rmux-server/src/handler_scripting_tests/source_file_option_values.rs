use super::*;

async fn show_user_option(handler: &RequestHandler, name: &str) -> Option<String> {
    let parsed = CommandParser::new()
        .parse(&format!("show-options -gqv {name}"))
        .expect("parse show-options");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("show-options -q succeeds");
    let value = String::from_utf8(output.stdout().to_vec()).expect("utf-8 option value");
    (!value.is_empty()).then(|| value.trim_end_matches('\n').to_owned())
}

#[tokio::test]
async fn invalid_set_option_value_fails_its_line_and_the_config_continues() {
    // tmux 3.7b, `source-file` of each file: rc=1, the message alone on
    // stderr, and both @before and @after applied.
    let handler = RequestHandler::new();
    create_background_identity_session(&handler, session_name("option-values")).await;
    let root = temp_root("option-values");
    for (index, (line, message)) in [
        ("set -g status-style 'ab\u{e9}'", "invalid style: ab\u{e9}"),
        ("set -g status-style 'fg=nope'", "invalid style: fg=nope"),
        ("set -g status-left-length -5", "value is too small: -5"),
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("values-{index}.conf"));
        write_config(
            &path,
            &format!("set -g @before{index} one\n{line}\nset -g @after{index} two\n"),
        );
        let response = handler
            .handle(source_file_request(
                vec![path.to_string_lossy().into_owned()],
                None,
            ))
            .await;
        let Response::SourceFile(response) = response else {
            panic!("expected source-file response for {line:?}: {response:?}");
        };
        assert_eq!(response.exit_status(), Some(1), "{line:?}: {response:?}");
        assert!(
            response.command_output().is_none(),
            "{line:?}: {response:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(response.stderr()),
            format!("{message}\n"),
            "{line:?}"
        );
        assert_eq!(
            show_user_option(&handler, &format!("@before{index}")).await,
            Some("one".to_owned()),
            "{line:?}"
        );
        assert_eq!(
            show_user_option(&handler, &format!("@after{index}")).await,
            Some("two".to_owned()),
            "{line:?}"
        );
    }
    fs::remove_dir_all(root).expect("remove config root");
}

#[tokio::test]
async fn parse_only_source_file_does_not_validate_option_values() {
    // tmux 3.7b: `source-file -n` of a file with an invalid style is rc=0.
    let handler = RequestHandler::new();
    let root = temp_root("option-values-parse-only");
    let path = root.join("values.conf");
    write_config(&path, "set -g status-style 'ab\u{e9}'\n");
    let mut request = source_file_request(vec![path.to_string_lossy().into_owned()], None);
    if let Request::SourceFile(source) = &mut request {
        source.parse_only = true;
    }
    let response = handler.handle(request).await;
    let Response::SourceFile(response) = response else {
        panic!("expected source-file response: {response:?}");
    };
    assert_eq!(response.exit_status().unwrap_or(0), 0, "{response:?}");
    fs::remove_dir_all(root).expect("remove config root");
}

#[tokio::test]
async fn source_file_parse_only_defers_option_lookup_errors() {
    let handler = RequestHandler::new();
    let root = temp_root("option-lookup-parse-only");
    let path = root.join("lookup.conf");
    for line in [
        "set-option -g s2572-bogus on",
        "set -g s2572-bogus on",
        "set-window-option -g s2572-bogus on",
        "show-options -g s2572-bogus",
        "set-option -g status- on",
        "if-shell -F 1 { set-option -g s2572-bogus on }",
    ] {
        write_config(&path, &format!("{line}\nset -g @s2572 after\n"));
        let mut request = source_file_request(vec![path.to_string_lossy().into_owned()], None);
        if let Request::SourceFile(source) = &mut request {
            source.parse_only = true;
        }
        let response = handler.handle(request).await;
        let Response::SourceFile(response) = response else {
            panic!("expected source-file response: {response:?}");
        };
        assert_eq!(
            response.exit_status().unwrap_or(0),
            0,
            "{line}: {response:?}"
        );
        assert!(response.stderr().is_empty(), "{line}: {response:?}");
        assert_eq!(show_user_option(&handler, "@s2572").await, None);
    }
    fs::remove_dir_all(root).expect("remove config root");
}
