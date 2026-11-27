use super::*;

async fn source_response(
    handler: &RequestHandler,
    root: &std::path::Path,
    route: &str,
    paths: &[&str],
    parse_only: bool,
    quiet: bool,
) -> rmux_proto::SourceFileResponse {
    let response = if route == "direct" {
        handler
            .handle(Request::SourceFile(Box::new(SourceFileRequest {
                paths: paths.iter().map(|path| (*path).to_owned()).collect(),
                quiet,
                parse_only,
                verbose: false,
                expand_paths: false,
                target: None,
                caller_cwd: Some(root.to_path_buf()),
                stdin: None,
            })))
            .await
    } else {
        let mut arguments = vec![if route == "alias" {
            "sourcequeue"
        } else {
            "source-file"
        }
        .into()];
        if parse_only {
            arguments.push("-n".into());
        }
        if quiet {
            arguments.push("-q".into());
        }
        arguments.extend(paths.iter().map(|path| (*path).to_owned()));
        if route == "nested" {
            // Test the ordinary nested queue as well as the negotiated wrapper.
            std::fs::write(root.join("outer.conf"), arguments.join(" ")).unwrap();
            arguments = vec!["source-file".into(), "outer.conf".into()];
        }
        let request =
            SourceFileInputV1Request::new(arguments, Some(root.to_path_buf()), Some(Vec::new()));
        send(
            handler,
            if route == "legacy" {
                request
            } else {
                request.requiring_queue_status()
            },
        )
        .await
    };
    match response {
        Response::SourceFile(source) => source,
        Response::SourceFileQueueV1(result) => {
            assert!(
                !result.stop_queue,
                "selected files keep the caller queue alive"
            );
            result.source
        }
        response => panic!("{route}: {response:?}"),
    }
}

#[tokio::test]
async fn mixed_parse_and_budget_errors_keep_separate_streams_across_source_routes() {
    let (handler, root) = setup("mixed-budget-diagnostics").await;
    for group in ["A", "B"] {
        std::fs::create_dir(root.join(group)).unwrap();
        for index in 0..128 {
            std::fs::write(
                root.join(group).join(format!("{index:03}.conf")),
                "set -ga @selected x\n",
            )
            .unwrap();
        }
    }
    std::fs::write(root.join("tail.conf"), "set -g @tail yes\n").unwrap();
    run(
        &handler,
        "set-option -s command-alias[20] sourcequeue=source-file",
        QueueExecutionContext::without_caller_cwd(),
    )
    .await
    .unwrap();
    for parse_error in [false, true] {
        std::fs::write(
            root.join("middle.conf"),
            if parse_error { "not-a-command\n" } else { "" },
        )
        .unwrap();
        for route in ["direct", "legacy", "negotiated", "alias", "nested"] {
            for parse_only in [false, true] {
                for quiet in [false, true] {
                    run(
                        &handler,
                        "set -gu @selected",
                        QueueExecutionContext::without_caller_cwd(),
                    )
                    .await
                    .unwrap();
                    let source = source_response(
                        &handler,
                        &root,
                        route,
                        &["A/*.conf", "middle.conf", "B/*.conf", "tail.conf"],
                        parse_only,
                        quiet,
                    )
                    .await;
                    assert_eq!(source.exit_status(), Some(1), "{route}: {source:?}");
                    let stderr = String::from_utf8_lossy(source.stderr());
                    assert_eq!(
                        stderr.matches("exceeds 256 matched files").count(),
                        1,
                        "{route}, parse_only={parse_only}: {source:?}"
                    );
                    assert!(!stderr.contains("not-a-command"), "{source:?}");
                    let stdout = source
                        .command_output()
                        .map(|output| output.stdout())
                        .unwrap_or_default();
                    let stdout = String::from_utf8_lossy(stdout);
                    assert!(!stdout.contains("exceeds 256 matched files"), "{source:?}");
                    assert_eq!(
                        stdout.matches("unknown command: not-a-command").count(),
                        usize::from(parse_error),
                        "{source:?}"
                    );
                    let selected = run(
                        &handler,
                        "show-options -gqv @selected",
                        QueueExecutionContext::without_caller_cwd(),
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        String::from_utf8_lossy(selected.stdout()).trim(),
                        "x".repeat(if parse_only { 0 } else { 128 }),
                        "{route}"
                    );
                    let tail = run(
                        &handler,
                        "show-options -gqv @tail",
                        QueueExecutionContext::without_caller_cwd(),
                    )
                    .await
                    .unwrap();
                    assert!(String::from_utf8_lossy(tail.stdout()).trim().is_empty());
                }
            }
        }
    }
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn mixed_parse_and_missing_read_errors_keep_separate_streams_in_either_order() {
    let (handler, root) = setup("mixed-read-diagnostics").await;
    std::fs::write(root.join("bad.conf"), "not-a-command\n").unwrap();
    for route in ["direct", "legacy", "negotiated", "nested"] {
        for parse_only in [false, true] {
            for paths in [["bad.conf", "missing.conf"], ["missing.conf", "bad.conf"]] {
                let source =
                    source_response(&handler, &root, route, &paths, parse_only, false).await;
                assert_eq!(source.exit_status(), Some(1), "{route}: {source:?}");
                let stderr = String::from_utf8_lossy(source.stderr());
                assert_eq!(
                    stderr.matches("No such file or directory").count(),
                    1,
                    "{source:?}"
                );
                assert!(!stderr.contains("not-a-command"), "{source:?}");
                let stdout = source
                    .command_output()
                    .map(|output| output.stdout())
                    .unwrap_or_default();
                let stdout = String::from_utf8_lossy(stdout);
                assert_eq!(
                    stdout.matches("unknown command: not-a-command").count(),
                    1,
                    "{source:?}"
                );
                assert!(!stdout.contains("No such file or directory"), "{source:?}");
            }
        }
    }
    cleanup(&handler, root).await;
}

#[tokio::test]
async fn source_dimension_errors_execute_in_order_and_parse_only_ignores_values() {
    // tmux 3.7b: a dimension value fails at execution, unlike an unknown
    // command/flag. A newline starts a fresh queue and still runs its tail.
    let handler = RequestHandler::new();
    for command in ["new-session", "new"] {
        for flag in ["-x", "-y"] {
            for value in ["bad", "-1", "9223372036854775808"] {
                for separator in [" ; ", "\n"] {
                    for parse_only in [false, true] {
                        let response = handler
                            .handle(Request::SourceFile(Box::new(SourceFileRequest {
                                paths: vec!["-".into()],
                                quiet: false,
                                parse_only,
                                verbose: false,
                                expand_paths: false,
                                target: None,
                                caller_cwd: None,
                                stdin: Some(format!(
                                    "display-message -p before{separator}{command} {flag} {value}{separator}display-message -p after\n"
                                )),
                            })))
                            .await;
                        let Response::SourceFile(source) = response else {
                            panic!("{command} {flag} {value}: {response:?}");
                        };
                        let stdout = source
                            .command_output()
                            .map(|o| o.stdout())
                            .unwrap_or_default();
                        let expected = if parse_only {
                            ""
                        } else if separator == "\n" {
                            "before\nafter\n"
                        } else {
                            "before\n"
                        };
                        assert_eq!(
                            stdout,
                            expected.as_bytes(),
                            "{command} {flag} {value}, parse_only={parse_only}"
                        );
                        assert_eq!(source.stderr().is_empty(), parse_only, "{source:?}");
                        assert_eq!(source.exit_status().unwrap_or(0), i32::from(!parse_only));
                    }
                }
            }
        }
    }
}
