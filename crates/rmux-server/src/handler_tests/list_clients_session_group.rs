use super::*;
use crate::client_names::{attached_client_name, control_client_name};

const GROUP_FORMAT: &str = concat!(
    "#{session_name}|#{session_group}|#{session_group_size}|",
    "#{session_group_attached}|#{session_group_many_attached}|",
    "#{session_group_attached_list}|#{session_attached}|",
    "#{session_many_attached}|#{client_session}|#{client_control_mode}",
);

async fn list(
    handler: &RequestHandler,
    target: Option<&str>,
    filter: Option<&str>,
    format: &str,
    reversed: bool,
) -> String {
    let response = handler
        .handle(Request::ListClients(Box::new(
            rmux_proto::ListClientsRequest {
                target_session: target.map(session_name),
                filter: filter.map(str::to_owned),
                format: Some(format.to_owned()),
                sort_order: reversed.then(|| "index".to_owned()),
                reversed,
            },
        )))
        .await;
    let Response::ListClients(response) = response else {
        panic!("expected list-clients response: {response:?}");
    };
    let output = String::from_utf8(response.output.stdout().to_vec()).expect("UTF-8 output");
    assert_eq!(response.match_count, output.lines().count());
    output
}

async fn create_session(handler: &RequestHandler, name: &str, group: Option<&str>) {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name(name)),
            working_directory: None,
            detached: true,
            size: None,
            environment: None,
            group_target: group.map(session_name),
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
    crate::session_fixture::assert_new_session(&response);
}

#[tokio::test]
async fn list_clients_binds_each_session_before_group_formats_and_filters() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let outside = session_name("outside");
    create_session(&handler, "alpha", None).await;
    create_session(&handler, "beta", Some("alpha")).await;
    create_session(&handler, "gamma", Some("alpha")).await;
    create_session(&handler, "outside", None).await;
    let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(94_101, alpha.clone(), attach_tx)
        .await;
    let (outside_tx, _outside_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(94_103, outside.clone(), outside_tx)
        .await;
    let (event_tx, _event_rx) = mpsc::channel(crate::control::CONTROL_SERVER_EVENT_CAPACITY);
    handler
        .register_control_with_closing(
            94_102,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            event_tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    handler
        .set_control_session(94_102, Some(beta.clone()))
        .await
        .unwrap();

    // tmux 3.7b: a detached member contributes no client; outside stays ungrouped.
    let attached = attached_client_name(94_101);
    let control = control_client_name(94_102);
    let names = format!("{attached},{control}");
    let alpha_line = format!("alpha|alpha|3|2|1|{names}|1|0|alpha|0\n");
    let beta_line = format!("beta|alpha|3|2|1|{names}|1|0|beta|1\n");
    let outside_line = "outside||||||1|0|outside|0\n";
    // Registration order is alpha, outside, beta: group order skips outsiders.
    assert_eq!(
        list(&handler, None, None, GROUP_FORMAT, false).await,
        format!("{alpha_line}{outside_line}{beta_line}"),
    );
    assert_eq!(
        list(&handler, Some("alpha"), None, GROUP_FORMAT, false).await,
        alpha_line,
    );
    assert_eq!(
        list(
            &handler,
            None,
            Some("#{session_group_many_attached}"),
            GROUP_FORMAT,
            false
        )
        .await,
        format!("{alpha_line}{beta_line}"),
    );
    assert_eq!(
        list(&handler, None, None, GROUP_FORMAT, true).await,
        format!("{beta_line}{outside_line}{alpha_line}"),
    );
    let alpha_id = handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .unwrap()
        .id();
    assert_eq!(
        list(
            &handler,
            Some("alpha"),
            Some("#{==:#{client_control_mode},0}"),
            "#{session_id}|#{client_name}|#{L:#{client_session};}",
            false
        )
        .await,
        format!("{alpha_id}|{attached}|alpha;outside;beta;\n"),
    );

    // Mutation siblings: moving and detaching a control client refresh the group.
    handler
        .set_control_session(94_102, Some(outside))
        .await
        .unwrap();
    assert_eq!(
        list(&handler, Some("alpha"), None, GROUP_FORMAT, false).await,
        format!("alpha|alpha|3|1|0|{attached}|1|0|alpha|0\n"),
    );
    assert_eq!(
        list(&handler, Some("outside"), None, GROUP_FORMAT, false).await,
        "outside||||||2|1|outside|0\noutside||||||2|1|outside|1\n",
    );
    handler.set_control_session(94_102, None).await.unwrap();
    assert_eq!(
        list(&handler, Some("outside"), None, GROUP_FORMAT, false).await,
        outside_line,
    );
    assert!(list(&handler, Some("beta"), None, GROUP_FORMAT, false)
        .await
        .is_empty());
    let renamed = session_name("renamed");
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: alpha,
            new_name: renamed,
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    assert_eq!(
        list(&handler, Some("renamed"), None, GROUP_FORMAT, false).await,
        format!("renamed|alpha|3|1|0|{attached}|1|0|renamed|0\n"),
    );
}
