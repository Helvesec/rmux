use super::*;

async fn set_command_alias(handler: &RequestHandler, alias: &str) {
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::CommandAlias,
                value: alias.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
}

#[tokio::test]
async fn runtime_command_alias_option_drives_command_string_parser() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "say=display-message -p --").await;

    let parsed = handler
        .parse_command_string_one_group("say hello")
        .await
        .expect("runtime alias should parse");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("runtime alias should execute");

    assert_eq!(output.stdout(), b"hello\n");
}

#[tokio::test]
async fn runtime_command_alias_option_drives_source_file_parser() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "sbuf=set-buffer -b aliased").await;

    let root = temp_root("command-alias");
    let config = root.join("main.conf");
    write_config(&config, "sbuf from-source\n");
    assert_eq!(
        handler
            .handle(source_file_request(
                vec!["main.conf".to_owned()],
                Some(root)
            ))
            .await,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );

    assert_eq!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some("aliased".to_owned()),
            }))
            .await
            .command_output()
            .expect("aliased buffer output")
            .stdout(),
        b"from-source"
    );
}
