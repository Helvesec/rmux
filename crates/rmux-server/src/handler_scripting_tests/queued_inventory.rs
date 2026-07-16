use super::*;

#[tokio::test]
async fn parsed_command_list_routes_start_server_and_named_inventory_lookups() {
    let handler = RequestHandler::new();
    let parsed = CommandParser::new()
        .parse(
            "start-server ; \
             list-commands -F '#{command_list_name}|#{command_list_usage}' send-keys ; \
             list-commands -F '#{command_list_name}|#{command_list_usage}' set-buffer ; \
             list-commands -F '#{command_list_name}|#{command_list_usage}' set-environment ; \
             list-commands -F '#{command_list_name}|#{command_list_usage}' show-environment ; \
             list-commands -F '#{command_list_name}|#{command_list_usage}' show-hooks ; \
             list-commands -F '#{command_list_name}|#{command_list_usage}' switch-client ; \
             list-commands -F '#{?command_list_alias,alias,none}' list-commands",
        )
        .expect("command list parses");

    let output = handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("shared queue inventory succeeds");

    assert_eq!(
        String::from_utf8(output.stdout).expect("inventory output is utf-8"),
        concat!(
            "send-keys|[-FHKlMRX] [-c target-client] [-N repeat-count] [-t target-pane] [key ...]\n",
            "set-buffer|[-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] [data]\n",
            "set-environment|[-Fhgru] [-t target-session] variable [value]\n",
            "show-environment|[-hgs] [-t target-session] [variable]\n",
            "show-hooks|[-gpw] [-t target-pane] [hook]\n",
            "switch-client|[-ElnprZ] [-c target-client] [-t target-session] [-T key-table] [-O order]\n",
            "alias\n",
        )
    );
}

#[tokio::test]
async fn control_queue_allows_read_only_start_server_and_list_commands() {
    let handler = RequestHandler::new();
    let requester_pid = 52_001;
    let _access = handler.begin_detached_requester_access(requester_pid, false);
    let parsed = CommandParser::new()
        .parse("start-server ; list-commands new-window")
        .expect("control commands parse");

    let result = handler
        .execute_control_commands(requester_pid, parsed)
        .await;

    assert_eq!(result.error, None);
    assert_eq!(
        String::from_utf8(result.stdout).expect("inventory output is utf-8"),
        "new-window (neww) [-abdkPS] [-c start-directory] [-e environment] [-F format] [-n window-name] [-t target-window] [shell-command [argument ...]]\n"
    );
}
