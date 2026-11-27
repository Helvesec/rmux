use super::*;
use crate::handler::test_support::SERVER_SIGNAL_LIVENESS;
use rmux_proto::{
    NewSessionRequest, Request, RespawnPaneRequest, Response, SessionName,
    SplitWindowCommandV1Request, TerminalSize,
};

#[tokio::test]
async fn sc2_key_intent_cannot_remove_respawned_generation_or_replacement_attach() {
    for replace_attach in [false, true] {
        let handler = RequestHandler::new();
        let session = SessionName::new("sc2-generation").unwrap();
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        handler
            .handle(Request::SplitWindowCommandV1(Box::new(
                SplitWindowCommandV1Request::new(
                    vec!["split-window".into(), "-k".into(), "exit 7".into()],
                    None,
                    None,
                ),
            )))
            .await;
        let target = PaneTarget::new(session.clone(), 1);
        let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
        loop {
            if is_dead_key(&*handler.state.lock().await, &target) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the kept `exit 7` pane never published its death"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let pid = 962001;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, session.clone(), tx).await;
        let (identity, session_id) = {
            let attaches = handler.active_attach.lock().await;
            let active = attaches.by_pid.get(&pid).unwrap();
            (
                ActiveAttachIdentity::new(pid, active.id, active.session_id),
                active.session_id,
            )
        };
        let intent = handler
            .capture_key_dismissal(identity, &target, session_id)
            .await
            .unwrap();
        if replace_attach {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            handler.register_attach(pid, session.clone(), tx).await;
        } else {
            assert!(matches!(
                handler
                    .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                        target: target.clone(),
                        kill: false,
                        start_directory: None,
                        environment: None,
                        command: Some(vec!["sleep 30".into()]),
                        process_command: None,
                    })))
                    .await,
                Response::RespawnPane(_)
            ));
        }
        handler.dismiss_kept_pane_by_key(intent).await.unwrap();
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .sessions
                .session(&session)
                .unwrap()
                .window()
                .pane_count(),
            2
        );
    }
}
