use super::*;
use rmux_proto::{
    BindKeyRequest, HookLifecycle, ListKeysRequest, SetBufferRequest, SetHookMutationRequest,
};

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
async fn runtime_command_alias_preserves_option_like_positional_values() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "literal=set-option -g @alias-value").await;

    let parsed = handler
        .parse_command_string_one_group("literal -tfoo ; show-options -gqv @alias-value")
        .await
        .expect("runtime alias should preserve its option-like appended argument");
    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("runtime alias should execute");

    assert_eq!(output.stdout(), b"-tfoo\n");
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

#[tokio::test]
async fn internal_canonical_execution_does_not_expand_aliases_again() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "if-shell=display-message -p second").await;

    let response = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("if-shell -F 1 \"display-message -p first\"".to_owned()),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .expect("canonical queue output")
            .stdout(),
        b"first\n"
    );
}

#[tokio::test]
async fn internal_canonical_execution_keeps_deferred_branch_aliases_dynamic() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "inner=display-message -p nested").await;

    let response = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("if-shell -F 1 inner".to_owned()),
        })))
        .await;

    assert_eq!(
        response
            .command_output()
            .expect("deferred branch output")
            .stdout(),
        b"nested\n"
    );
}

#[tokio::test]
async fn internal_canonical_execution_rejects_malformed_shapes() {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![
                INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned(),
                "-".to_owned(),
            ],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("display-message -p no".to_owned()),
        })))
        .await;

    let Response::Error(error) = response else {
        panic!("malformed internal canonical request should fail: {response:?}");
    };
    assert!(error
        .error
        .to_string()
        .contains("invalid internal source-file request path"));
}

#[tokio::test]
async fn runtime_command_alias_option_drives_hook_registration_parser() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "sbuf=set-buffer -b aliased").await;

    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterSetBuffer,
            command: Some("sbuf from-hook".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");

    let response = handler
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("origin".to_owned()),
            content: b"origin".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)), "{response:?}");

    let output = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("aliased".to_owned()),
        }))
        .await;
    assert_eq!(
        output
            .command_output()
            .expect("hook-created aliased buffer output")
            .stdout(),
        b"from-hook"
    );

    let root = temp_root("command-alias-hook");
    let config = root.join("hook.conf");
    write_config(
        &config,
        "set-hook -g after-set-buffer 'sbuf from-source-hook'\n",
    );
    assert_eq!(
        handler
            .handle(source_file_request(
                vec!["hook.conf".to_owned()],
                Some(root)
            ))
            .await,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );
    let response = handler
        .handle(Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("source-origin".to_owned()),
            content: b"origin".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)), "{response:?}");
    let output = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("aliased".to_owned()),
        }))
        .await;
    assert_eq!(
        output
            .command_output()
            .expect("source-hook-created aliased buffer output")
            .stdout(),
        b"from-source-hook"
    );
}

async fn listed_root_bindings(handler: &RequestHandler) -> String {
    let response = handler
        .handle(Request::ListKeys(Box::new(ListKeysRequest {
            table_name: Some("root".to_owned()),
            first_only: false,
            notes: false,
            include_unnoted: true,
            reversed: false,
            format: None,
            sort_order: None,
            prefix: None,
            key: None,
        })))
        .await;
    let Response::ListKeys(response) = response else {
        panic!("expected list-keys success, got {response:?}");
    };
    String::from_utf8(response.command_output().stdout().to_vec()).expect("list-keys utf8")
}

#[tokio::test]
async fn runtime_command_alias_option_drives_binding_payload_parsers() {
    let handler = RequestHandler::new();
    set_command_alias(&handler, "sbuf=set-buffer -b aliased").await;

    for (key, command) in [
        (
            "F10",
            vec!["sbuf".to_owned(), "from-protocol-argv".to_owned()],
        ),
        ("F11", vec!["sbuf from-protocol-string".to_owned()]),
    ] {
        let response = handler
            .handle(Request::BindKey(Box::new(BindKeyRequest {
                table_name: "root".to_owned(),
                key: key.to_owned(),
                note: None,
                repeat: false,
                command: Some(command),
            })))
            .await;
        assert!(matches!(response, Response::BindKey(_)), "{response:?}");
    }

    let parsed = handler
        .parse_command_string_one_group("bind-key -T root F12 sbuf from-queue")
        .await
        .expect("queued bind-key parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queued bind-key executes");

    let root = temp_root("command-alias-bind-key");
    let config = root.join("main.conf");
    write_config(&config, "bind-key -T root F9 sbuf from-source-binding\n");
    assert_eq!(
        handler
            .handle(source_file_request(
                vec!["main.conf".to_owned()],
                Some(root)
            ))
            .await,
        Response::SourceFile(rmux_proto::SourceFileResponse::no_output())
    );

    let bindings = listed_root_bindings(&handler).await;
    for expected in [
        "set-buffer -b aliased from-protocol-argv",
        "set-buffer -b aliased from-protocol-string",
        "set-buffer -b aliased from-queue",
        "set-buffer -b aliased from-source-binding",
    ] {
        assert!(
            bindings.contains(expected),
            "missing canonical binding {expected:?} in {bindings:?}"
        );
    }
}
