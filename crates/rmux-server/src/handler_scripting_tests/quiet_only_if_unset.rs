use super::*;

#[tokio::test]
async fn quiet_only_if_unset_preserves_explicit_options_in_queues_and_config() {
    let handler = RequestHandler::new();
    create_background_identity_session(&handler, session_name("quiet-options")).await;
    let root = temp_root("quiet-only-if-unset");
    fs::create_dir_all(&root).expect("config root");

    for (command, scope, option, show) in [
        (
            "set-option",
            "-g",
            "base-index",
            "show-options -gv base-index",
        ),
        (
            "set-option",
            "-t quiet-options",
            "base-index",
            "show-options -v -t quiet-options base-index",
        ),
        (
            "set-window-option",
            "-g",
            "pane-base-index",
            "show-options -gwv pane-base-index",
        ),
        (
            "set-window-option",
            "-t quiet-options:0",
            "pane-base-index",
            "show-options -wv -t quiet-options:0 pane-base-index",
        ),
        (
            "set-option",
            "-g",
            "history-limit",
            "show-options -gv history-limit",
        ),
        (
            "set-option",
            "-t quiet-options",
            "history-limit",
            "show-options -v -t quiet-options history-limit",
        ),
    ] {
        for config in [false, true] {
            for (flags, succeeds, expected) in [
                ("-o -q", true, "5\n"),
                ("-qo", true, "5\n"),
                ("-o", false, "5\n"),
                ("-q", true, "7\n"),
            ] {
                let script =
                    format!("{command} {scope} {option} 5; {command} {scope} {flags} {option} 7");
                if config {
                    let path = root.join("options.conf");
                    fs::write(&path, script.replace(';', "\n")).expect("write config");
                    let response = handler
                        .handle(source_file_request(
                            vec![path.to_string_lossy().into_owned()],
                            None,
                        ))
                        .await;
                    let Response::SourceFile(response) = response else {
                        panic!("expected source-file response: {response:?}");
                    };
                    assert_eq!(
                        response.exit_status().unwrap_or(0),
                        i32::from(!succeeds),
                        "config: {script}: {response:?}"
                    );
                    if succeeds {
                        assert!(
                            response
                                .command_output()
                                .is_none_or(|output| output.stdout().is_empty()),
                            "quiet config emitted a diagnostic: {response:?}"
                        );
                    }
                } else {
                    let result = handler
                        .execute_parsed_commands_for_test(
                            std::process::id(),
                            CommandParser::new().parse(&script).expect("parse commands"),
                        )
                        .await;
                    assert_eq!(result.is_ok(), succeeds, "queue: {script}: {result:?}");
                    if !succeeds {
                        assert!(result.unwrap_err().to_string().contains("already set"));
                    }
                }
                let output = handler
                    .execute_parsed_commands_for_test(
                        std::process::id(),
                        CommandParser::new().parse(show).expect("parse show"),
                    )
                    .await
                    .expect("show explicit option");
                assert_eq!(output.stdout(), expected.as_bytes(), "{script}");
            }
        }
    }
    fs::remove_dir_all(root).expect("remove config root");
}
