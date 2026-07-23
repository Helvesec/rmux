use super::super::client_support::install_managed_client_resolution_pause;
use super::super::overlay_support::{AttachedOverlayInput, ClientOverlayState};
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use rmux_proto::{
    KillSessionRequest, LinkWindowRequest, NewSessionRequest, NewWindowRequest, PaneTarget,
    Request, RespawnPaneRequest, Response, SessionName, Target, TerminalSize, UnlinkWindowRequest,
    WindowTarget,
};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &SessionName) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn attach(
    handler: &RequestHandler,
    pid: u32,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(pid, session.clone(), control_tx)
        .await;
    control_rx
}

async fn execute_overlay(
    handler: &RequestHandler,
    requester_pid: u32,
    command: &str,
) -> Result<(), rmux_proto::RmuxError> {
    let parsed = handler.parse_control_commands(command).await?;
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .map(|_| ())
}

async fn link_window(
    handler: &RequestHandler,
    owner: &SessionName,
    alias: &SessionName,
) -> Response {
    handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await
}

#[tokio::test]
async fn client_selector_and_explicit_pane_target_keep_distinct_identities() {
    let handler = RequestHandler::new();
    let alpha = session_name("overlay-selector-alpha");
    let beta = session_name("overlay-selector-beta");
    let alpha_pid = 971_001;
    let beta_pid = 971_002;
    create_session(&handler, &alpha).await;
    create_session(&handler, &beta).await;
    let _alpha_rx = attach(&handler, alpha_pid, &alpha).await;
    let _beta_rx = attach(&handler, beta_pid, &beta).await;

    execute_overlay(
        &handler,
        alpha_pid,
        &format!("display-menu -c {beta_pid} -t {alpha}:0.0 Item i 'display-message chosen'"),
    )
    .await
    .expect("menu opens on selected client");
    {
        let active_attach = handler.active_attach.lock().await;
        let Some(ClientOverlayState::Menu(menu)) = active_attach.by_pid[&beta_pid].overlay.as_ref()
        else {
            panic!("selected client owns the menu");
        };
        assert_eq!(
            menu.current_target,
            rmux_proto::Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0))
        );
    }
    handler
        .clear_interactive_overlay(beta_pid, true)
        .await
        .expect("menu cleanup");

    execute_overlay(
        &handler,
        alpha_pid,
        &format!("display-popup -N -c {beta_pid} -t {alpha}:0.0 -T Popup"),
    )
    .await
    .expect("popup opens on selected client");
    let active_attach = handler.active_attach.lock().await;
    let Some(ClientOverlayState::Popup(popup)) = active_attach.by_pid[&beta_pid].overlay.as_ref()
    else {
        panic!("selected client owns the popup");
    };
    assert_eq!(
        popup.current_target,
        rmux_proto::Target::Pane(PaneTarget::with_window(alpha, 0, 0))
    );
}

#[tokio::test]
async fn same_pid_replacement_during_client_resolution_cannot_receive_menu() {
    let handler = RequestHandler::new();
    let alpha = session_name("overlay-client-aba");
    let pid = 971_011;
    create_session(&handler, &alpha).await;
    let _old_rx = attach(&handler, pid, &alpha).await;
    let pause = install_managed_client_resolution_pause(pid);
    let parsed = handler
        .parse_control_commands(&format!(
            "display-menu -c {pid} Item i 'display-message chosen'"
        ))
        .await
        .expect("menu parses");
    let task_handler = handler.clone();
    let task = tokio::spawn(async move {
        task_handler
            .execute_parsed_commands_for_test(pid, parsed)
            .await
    });
    timeout(Duration::from_secs(2), pause.reached.notified())
        .await
        .expect("client resolution pauses");

    let _replacement_rx = attach(&handler, pid, &alpha).await;
    pause.release.notify_one();
    let result = timeout(Duration::from_secs(2), task)
        .await
        .expect("menu command finishes")
        .expect("menu task does not panic");
    assert!(result.is_err(), "stale client identity must fail closed");
    assert!(handler.active_attach.lock().await.by_pid[&pid]
        .overlay
        .is_none());
}

#[tokio::test]
async fn menu_action_is_discarded_after_target_pane_respawn() {
    let handler = RequestHandler::new();
    let alpha = session_name("overlay-pane-respawn");
    let pid = 971_021;
    create_session(&handler, &alpha).await;
    let _control_rx = attach(&handler, pid, &alpha).await;
    execute_overlay(
        &handler,
        pid,
        &format!("display-menu -t {alpha}:0.0 Item i 'display-message should-not-run'"),
    )
    .await
    .expect("menu opens");

    let response = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: PaneTarget::with_window(alpha.clone(), 0, 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![crate::test_shell::stdin_discard_command()]),
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    handler
        .handle_attached_live_input_for_test(pid, b"i")
        .await
        .expect("stale menu input is consumed");
    assert!(handler.active_attach.lock().await.by_pid[&pid]
        .overlay
        .is_none());
}

#[tokio::test]
async fn popup_is_discarded_after_same_window_is_unlinked_and_relinked() {
    let handler = RequestHandler::new();
    let host = session_name("overlay-relink-host");
    let owner = session_name("overlay-relink-owner");
    let alias = session_name("overlay-relink-alias");
    let pid = 971_031;
    for session in [&host, &owner, &alias] {
        create_session(&handler, session).await;
    }
    let _control_rx = attach(&handler, pid, &host).await;
    let extra = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alias.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(extra, Response::NewWindow(_)), "{extra:?}");
    let relinked = link_window(&handler, &owner, &alias).await;
    assert!(matches!(relinked, Response::LinkWindow(_)), "{relinked:?}");
    execute_overlay(
        &handler,
        pid,
        &format!("display-popup -N -t {alias}:0.0 -T Popup"),
    )
    .await
    .expect("popup opens on linked pane");

    let unlinked = handler
        .handle(Request::UnlinkWindow(UnlinkWindowRequest {
            target: WindowTarget::with_window(alias.clone(), 0),
            kill_if_last: false,
        }))
        .await;
    assert!(
        matches!(unlinked, Response::UnlinkWindow(_)),
        "{unlinked:?}"
    );
    let relinked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 1),
            after: false,
            before: true,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(relinked, Response::LinkWindow(_)), "{relinked:?}");

    let mut pending = Vec::new();
    let outcome = handler
        .handle_attached_overlay_input(pid, &mut pending, b"x")
        .await
        .expect("stale popup input is consumed");
    assert_eq!(outcome, AttachedOverlayInput::Consumed);
    assert!(pending.is_empty());
    assert!(handler.active_attach.lock().await.by_pid[&pid]
        .overlay
        .is_none());
}

#[tokio::test]
async fn overlay_command_context_rejects_a_same_name_session_replacement_after_validation() {
    let handler = RequestHandler::new();
    let host = session_name("overlay-command-host");
    let target = session_name("overlay-command-target");
    let pid = 971_041;
    create_session(&handler, &host).await;
    create_session(&handler, &target).await;
    let _control_rx = attach(&handler, pid, &host).await;
    let client = handler.active_attach_identity_for_test(pid).await;
    let target = Target::Session(target);
    let identity = {
        let mut state = handler.state.lock().await;
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&pid)
            .expect("client remains attached");
        let identity =
            super::identity::OverlayIdentity::capture(&mut state, client, target.clone())
                .expect("overlay target identity captures");
        assert!(identity.matches(&state, active, &target));
        identity
    };
    let context =
        identity.command_context(QueueExecutionContext::without_caller_cwd(), target.clone());

    let target_name = target.session_name().clone();
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: target_name.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    create_session(&handler, &target_name).await;

    let commands = handler
        .parse_control_commands("set-buffer -b overlay-command-aba fired")
        .await
        .expect("overlay command parses");
    let result = handler
        .execute_parsed_commands(pid, commands, context)
        .await;
    assert!(result.is_err(), "replacement session must fail closed");
    assert!(handler
        .state
        .lock()
        .await
        .buffers
        .show(Some("overlay-command-aba"))
        .is_err());
}
