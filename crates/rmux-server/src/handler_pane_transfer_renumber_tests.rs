use super::pane_group_transfer_tests::{create_grouped_session, create_session};
use super::RequestHandler;
use rmux_core::WindowId;
use rmux_proto::{
    BindKeyRequest, HookLifecycle, HookName, JoinPaneRequest, LinkWindowRequest, MovePaneRequest,
    NewWindowRequest, OptionName, OptionScopeSelector, PaneTarget, Request, Response,
    ScopeSelector, SendKeysExtRequest, SendKeysResponse, SessionName, SetHookRequest,
    SetOptionByNameRequest, SetOptionMode, SetOptionRequest, SplitDirection, WindowTarget,
};

const SURVIVOR_OPTION: &str = "@w13-m10-survivor";

#[derive(Clone, Copy)]
enum TransferCommand {
    Join,
    Move,
}

impl TransferCommand {
    const fn label(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Move => "move",
        }
    }

    fn request(self, source: PaneTarget, target: PaneTarget) -> Request {
        let join = JoinPaneRequest {
            source,
            target,
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        };
        match self {
            Self::Join => Request::JoinPane(join),
            Self::Move => Request::MovePane(MovePaneRequest {
                source: join.source,
                target: join.target,
                direction: join.direction,
                detached: join.detached,
                before: join.before,
                full_size: join.full_size,
                size: join.size,
            }),
        }
    }
}

async fn create_window(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    name: &str,
) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some(name.to_owned()),
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(window_index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler.wait_for_initial_panes_for_test().await;
}

async fn set_renumber(handler: &RequestHandler, session_name: &SessionName) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session_name.clone()),
            option: OptionName::RenumberWindows,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn mark_survivor(handler: &RequestHandler, target: &WindowTarget, marker: &str) -> WindowId {
    let option = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(target.clone()),
            name: SURVIVOR_OPTION.to_owned(),
            value: Some(marker.to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(matches!(option, Response::SetOptionByName(_)), "{option:?}");

    let automatic_rename = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(target.clone()),
            option: OptionName::AutomaticRename,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(
        matches!(automatic_rename, Response::SetOption(_)),
        "{automatic_rename:?}"
    );

    let hook = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Window(target.clone()),
            hook: HookName::WindowLayoutChanged,
            command: format!("display-message {marker}"),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");

    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .map(rmux_core::Window::id)
        .expect("marked survivor exists")
}

async fn assert_renumbered_survivor(
    handler: &RequestHandler,
    session_name: &SessionName,
    expected_id: WindowId,
    marker: &str,
) {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(session_name)
        .expect("source session survives");
    assert_eq!(
        session
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "{session_name} must be contiguous after the source window disappears"
    );
    let survivor = session.window_at(1).expect("survivor is reindexed to one");
    assert_eq!(survivor.id(), expected_id);
    assert_eq!(survivor.name(), Some("SURVIVOR"));
    assert!(!survivor.automatic_rename());

    let target = WindowTarget::with_window(session_name.clone(), 1);
    assert_eq!(
        state
            .options
            .explicit_value_by_name(
                &OptionScopeSelector::Window(target.clone()),
                SURVIVOR_OPTION,
            )
            .expect("valid user option")
            .1
            .as_deref(),
        Some(marker)
    );
    assert_eq!(
        state
            .hooks
            .window_bindings_view(&target, Some(HookName::WindowLayoutChanged))
            .iter()
            .map(|binding| binding.command())
            .collect::<Vec<_>>(),
        vec![format!("display-message {marker}")]
    );
}

fn transfer_target(response: Response, command: TransferCommand) -> PaneTarget {
    match (command, response) {
        (TransferCommand::Join, Response::JoinPane(response)) => response.target,
        (TransferCommand::Move, Response::MovePane(response)) => response.target,
        (_, response) => panic!("{}-pane failed: {response:?}", command.label()),
    }
}

async fn prepare_renumber_source(
    handler: &RequestHandler,
    label: &str,
    marker: &str,
) -> (SessionName, WindowId) {
    let session = create_session(handler, label).await;
    create_window(handler, &session, 1, "SOURCE").await;
    create_window(handler, &session, 2, "SURVIVOR").await;
    set_renumber(handler, &session).await;
    let _ = mark_survivor(
        handler,
        &WindowTarget::with_window(session.clone(), 1),
        "discarded-source",
    )
    .await;
    let survivor_id = mark_survivor(
        handler,
        &WindowTarget::with_window(session.clone(), 2),
        marker,
    )
    .await;
    (session, survivor_id)
}

async fn run_same_session_case(command: TransferCommand) {
    let handler = RequestHandler::new();
    let (session, survivor_id) = prepare_renumber_source(
        &handler,
        &format!("w13-m10-{}-same", command.label()),
        "same-survivor",
    )
    .await;

    let response = handler
        .handle(command.request(
            PaneTarget::with_window(session.clone(), 1, 0),
            PaneTarget::with_window(session.clone(), 2, 0),
        ))
        .await;
    assert_eq!(
        transfer_target(response, command),
        PaneTarget::with_window(session.clone(), 1, 1),
        "response must follow the target window from index 2 to index 1"
    );
    assert_renumbered_survivor(&handler, &session, survivor_id, "same-survivor").await;
}

async fn run_cross_session_case(command: TransferCommand) {
    let handler = RequestHandler::new();
    let source = create_session(
        &handler,
        &format!("w13-m10-{}-cross-source", command.label()),
    )
    .await;
    let destination = create_session(
        &handler,
        &format!("w13-m10-{}-cross-destination", command.label()),
    )
    .await;
    create_window(&handler, &source, 1, "SOURCE").await;
    create_window(&handler, &source, 2, "SURVIVOR").await;
    set_renumber(&handler, &source).await;
    let _ = mark_survivor(
        &handler,
        &WindowTarget::with_window(source.clone(), 1),
        "discarded-source",
    )
    .await;
    let survivor_id = mark_survivor(
        &handler,
        &WindowTarget::with_window(source.clone(), 2),
        "cross-survivor",
    )
    .await;

    let response = handler
        .handle(command.request(
            PaneTarget::with_window(source.clone(), 1, 0),
            PaneTarget::with_window(destination, 0, 0),
        ))
        .await;
    let _ = transfer_target(response, command);
    assert_renumbered_survivor(&handler, &source, survivor_id, "cross-survivor").await;
}

async fn run_grouped_case(command: TransferCommand) {
    let handler = RequestHandler::new();
    let owner = create_session(
        &handler,
        &format!("w13-m10-{}-group-owner", command.label()),
    )
    .await;
    create_window(&handler, &owner, 1, "SOURCE").await;
    create_window(&handler, &owner, 2, "SURVIVOR").await;
    let peer = create_grouped_session(
        &handler,
        &format!("w13-m10-{}-group-peer", command.label()),
        &owner,
    )
    .await;
    handler.wait_for_initial_panes_for_test().await;
    set_renumber(&handler, &owner).await;
    set_renumber(&handler, &peer).await;
    let _ = mark_survivor(
        &handler,
        &WindowTarget::with_window(owner.clone(), 1),
        "discarded-source",
    )
    .await;
    let owner_survivor = mark_survivor(
        &handler,
        &WindowTarget::with_window(owner.clone(), 2),
        "group-survivor",
    )
    .await;

    let response = handler
        .handle(command.request(
            PaneTarget::with_window(owner.clone(), 1, 0),
            PaneTarget::with_window(owner.clone(), 0, 0),
        ))
        .await;
    let _ = transfer_target(response, command);
    assert_renumbered_survivor(&handler, &owner, owner_survivor, "group-survivor").await;
    assert_renumbered_survivor(&handler, &peer, owner_survivor, "group-survivor").await;
}

async fn run_linked_case(command: TransferCommand) {
    let handler = RequestHandler::new();
    let source = create_session(
        &handler,
        &format!("w13-m10-{}-linked-source", command.label()),
    )
    .await;
    let alias = create_session(
        &handler,
        &format!("w13-m10-{}-linked-alias", command.label()),
    )
    .await;
    let destination = create_session(
        &handler,
        &format!("w13-m10-{}-linked-destination", command.label()),
    )
    .await;
    create_window(&handler, &source, 1, "SOURCE").await;
    create_window(&handler, &source, 2, "SURVIVOR").await;
    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source.clone(), 1),
            target: WindowTarget::with_window(alias.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)), "{link:?}");
    create_window(&handler, &alias, 2, "SURVIVOR").await;
    set_renumber(&handler, &source).await;
    set_renumber(&handler, &alias).await;
    let _ = mark_survivor(
        &handler,
        &WindowTarget::with_window(source.clone(), 1),
        "discarded-source",
    )
    .await;
    let source_survivor = mark_survivor(
        &handler,
        &WindowTarget::with_window(source.clone(), 2),
        "source-survivor",
    )
    .await;
    let alias_survivor = mark_survivor(
        &handler,
        &WindowTarget::with_window(alias.clone(), 2),
        "alias-survivor",
    )
    .await;

    let response = handler
        .handle(command.request(
            PaneTarget::with_window(source.clone(), 1, 0),
            PaneTarget::with_window(destination, 0, 0),
        ))
        .await;
    let _ = transfer_target(response, command);
    assert_renumbered_survivor(&handler, &source, source_survivor, "source-survivor").await;
    assert_renumbered_survivor(&handler, &alias, alias_survivor, "alias-survivor").await;
}

// tmux 3.7b measured on 2026-07-26: a join/move which consumes the
// source window applies renumber-windows to surviving source-session slots.
#[tokio::test]
async fn join_and_move_renumber_destroyed_same_session_source() {
    for command in [TransferCommand::Join, TransferCommand::Move] {
        run_same_session_case(command).await;
    }
}

#[tokio::test]
async fn join_and_move_renumber_destroyed_cross_session_source() {
    for command in [TransferCommand::Join, TransferCommand::Move] {
        run_cross_session_case(command).await;
    }
}

#[tokio::test]
async fn join_and_move_renumber_destroyed_grouped_source_family() {
    for command in [TransferCommand::Join, TransferCommand::Move] {
        run_grouped_case(command).await;
    }
}

#[tokio::test]
async fn join_and_move_renumber_destroyed_linked_source_family() {
    for command in [TransferCommand::Join, TransferCommand::Move] {
        run_linked_case(command).await;
    }
}

#[tokio::test]
async fn bind_key_join_and_move_renumber_destroyed_source() {
    for command in [TransferCommand::Join, TransferCommand::Move] {
        let handler = RequestHandler::new();
        let marker = format!("binding-{}-survivor", command.label());
        let (session, survivor_id) = prepare_renumber_source(
            &handler,
            &format!("w13-m10-{}-binding", command.label()),
            &marker,
        )
        .await;
        let requester_pid = std::process::id();
        let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
        handler
            .register_attach(requester_pid, session.clone(), control_tx)
            .await;
        let response = handler
            .handle(Request::BindKey(Box::new(BindKeyRequest {
                table_name: "prefix".to_owned(),
                key: "x".to_owned(),
                note: None,
                repeat: false,
                command: Some(vec![
                    format!("{}-pane", command.label()),
                    "-d".to_owned(),
                    "-s".to_owned(),
                    format!("{session}:1.0"),
                    "-t".to_owned(),
                    format!("{session}:0.0"),
                ]),
            })))
            .await;
        assert!(matches!(response, Response::BindKey(_)), "{response:?}");

        let response = handler
            .handle(Request::SendKeysExt(SendKeysExtRequest {
                target: Some(PaneTarget::with_window(session.clone(), 0, 0)),
                keys: vec!["C-b".to_owned(), "x".to_owned()],
                expand_formats: false,
                hex: false,
                literal: false,
                dispatch_key_table: true,
                copy_mode_command: false,
                forward_mouse_event: false,
                reset_terminal: false,
                repeat_count: None,
            }))
            .await;
        assert_eq!(
            response,
            Response::SendKeys(SendKeysResponse { key_count: 2 })
        );
        assert_renumbered_survivor(&handler, &session, survivor_id, &marker).await;
    }
}
