use super::*;
use crate::control::{ControlModeUpgrade, ControlServerEvent};
use crate::handler::client_support::install_managed_client_resolution_pause;
use rmux_proto::{ControlMode, NewSessionRequest, Request};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::mpsc;

async fn register(
    handler: &RequestHandler,
    pid: u32,
    session: &SessionName,
) -> (u64, mpsc::Receiver<ControlServerEvent>, Arc<AtomicBool>) {
    let closing = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel(512);
    let id = handler
        .register_control_with_closing(
            pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
            },
            tx,
            closing.clone(),
        )
        .await;
    handler
        .set_control_session(pid, Some(session.clone()))
        .await
        .unwrap();
    (id, rx, closing)
}

fn request(pid: u32, value: &str) -> RefreshClientRequest {
    RefreshClientRequest {
        target_client: Some(pid.to_string()),
        adjustment: None,
        clear_pan: false,
        pan_left: false,
        pan_right: false,
        pan_up: false,
        pan_down: false,
        status_only: false,
        clipboard_query: false,
        flags: None,
        flags_alias: None,
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size: None,
        colour_report: Some(value.to_owned()),
    }
}

async fn create(handler: &RequestHandler, name: &str) -> SessionName {
    let session = SessionName::new(name).unwrap();
    let result = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&result);
    session
}

async fn query(handler: &RequestHandler, session: &SessionName, pane: u32) -> Vec<u8> {
    handler
        .state
        .lock()
        .await
        .colour_query_replies_for_test(session, pane.into())
}

#[tokio::test]
async fn refresh_colour_public_wire_mutates_real_parser_with_cross_session_pane_isolation() {
    let handler = RequestHandler::new();
    let a = create(&handler, "colour-a").await;
    let b = create(&handler, "colour-b").await;
    let (_, _rx_a, closing_a) = register(&handler, 91401, &a).await;
    let (_, _rx_b, closing_b) = register(&handler, 91402, &b).await;
    assert!(query(&handler, &a, 0).await.is_empty());
    for (pid, report, expected) in [
        (
            91401,
            "%0:\x1b]10;#123456\x07",
            b"\x1b]10;rgb:1212/3434/5656\x07".as_slice(),
        ),
        (
            91402,
            "%0:\x1b]10;#abcdef\x1b\\",
            b"\x1b]10;rgb:abab/cdcd/efef\x07".as_slice(),
        ),
        (
            91401,
            "%0:\x1b]10;red\0\x07",
            b"\x1b]10;rgb:abab/cdcd/efef\x07".as_slice(),
        ),
        (
            91401,
            "%0:\x1b]10;invalid\x07",
            b"\x1b]10;rgb:abab/cdcd/efef\x07".as_slice(),
        ),
    ] {
        let response = handler
            .dispatch(pid, Request::RefreshClient(Box::new(request(pid, report))))
            .await
            .response;
        assert!(
            matches!(response, Response::RefreshClient(_)),
            "{response:?}"
        );
        assert_eq!(query(&handler, &a, 0).await, expected);
        assert!(query(&handler, &b, 1).await.is_empty());
    }
    closing_a.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(!query(&handler, &a, 0).await.is_empty());
    closing_b.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(query(&handler, &a, 0).await.is_empty());
    let (_, _new_rx, _) = register(&handler, 91403, &b).await;
    assert_eq!(
        query(&handler, &a, 0).await,
        b"\x1b]10;rgb:abab/cdcd/efef\x07"
    );
}

#[tokio::test]
async fn refresh_colour_stale_control_or_attach_resolution_cannot_mutate_replacement() {
    for control in [true, false] {
        let handler = Arc::new(RequestHandler::new());
        let session = create(&handler, "colour-identity").await;
        let (old_id, _rx, _) = register(&handler, 91501, &session).await;
        let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
        let pid = if control { 91501 } else { 91502 };
        if !control {
            handler
                .register_attach(pid, session.clone(), attach_tx)
                .await;
        }
        let pause = install_managed_client_resolution_pause(pid);
        let pending = {
            let handler = handler.clone();
            tokio::spawn(async move {
                handler
                    .handle_refresh_client(pid, request(pid, "%0:\x1b]10;red\x07"))
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), pause.reached.notified())
            .await
            .unwrap();
        let mut new_rx = None;
        let mut new_attach_rx = None;
        if control {
            let (new_id, rx, _) = register(&handler, pid, &session).await;
            assert_ne!(new_id, old_id);
            new_rx = Some(rx);
        } else {
            let (tx, rx) = mpsc::unbounded_channel();
            handler.register_attach(pid, session.clone(), tx).await;
            new_attach_rx = Some(rx);
        }
        pause.release.notify_one();
        assert!(matches!(pending.await.unwrap(), Response::Error(_)));
        assert!(query(&handler, &session, 0).await.is_empty());
        drop((new_rx, new_attach_rx));
    }
}

#[tokio::test]
async fn pan_precedes_colour_and_size_for_control_and_attach() {
    for control in [true, false] {
        let handler = RequestHandler::new();
        let session = create(&handler, "pan-colour-priority").await;
        let (_, _control_rx, _) = register(&handler, 91511, &session).await;
        let (tx, _attach_rx) = mpsc::unbounded_channel();
        handler.register_attach(91512, session.clone(), tx).await;
        let pid = if control { 91511 } else { 91512 };
        let baseline = request(pid, "%0:\x1b]10;#123456\x07");
        assert!(matches!(
            handler.handle_refresh_client(pid, baseline).await,
            Response::RefreshClient(_)
        ));
        let expected = query(&handler, &session, 0).await;
        assert_eq!(expected, b"\x1b]10;rgb:1212/3434/5656\x07");

        for direction in 0..5 {
            let mut r = request(pid, "%0:\x1b]10;red\x07");
            r.clear_pan = direction == 0;
            r.pan_left = direction == 1;
            r.pan_right = direction == 2;
            r.pan_up = direction == 3;
            r.pan_down = direction == 4;
            r.control_size = Some("bad".to_owned());
            r.flags = Some("invalid-flag".to_owned());
            assert!(matches!(
                handler.handle_refresh_client(pid, r).await,
                Response::RefreshClient(_)
            ));
            assert_eq!(query(&handler, &session, 0).await, expected);
        }

        // With no pan, a valid report still changes the same live transcript.
        assert!(matches!(
            handler
                .handle_refresh_client(pid, request(pid, "%0:\x1b]10;red\x07"))
                .await,
            Response::RefreshClient(_)
        ));
        assert_eq!(
            query(&handler, &session, 0).await,
            b"\x1b]10;rgb:ffff/0000/0000\x07"
        );
    }
}

#[tokio::test]
async fn parsed_pan_colour_union_preserves_canonical_report_and_priority() {
    use rmux_core::command_parser::{CommandArgument, CommandParser};

    let handler = RequestHandler::new();
    let session = create(&handler, "parsed-pan-colour").await;
    let (_, _rx, _) = register(&handler, 91513, &session).await;
    let report = "%0:\x1b]10;cmy:0/\x0b0/0\x07";
    let canonical = CommandArgument::String(report.to_owned()).to_tmux_reparse_string();
    for flags in ["-R -C bad -r", "-RCbad -r", "-Rr", "-c -r"] {
        let command = format!("refresh-client -t 91513 {flags} {canonical} 2");
        handler
            .execute_parsed_commands_for_test(91513, CommandParser::new().parse(&command).unwrap())
            .await
            .unwrap();
        assert!(query(&handler, &session, 0).await.is_empty());
    }
    let compact = CommandArgument::String(format!("-r{report}")).to_tmux_reparse_string();
    let command = format!("refresh-client -t 91513 {compact}");
    handler
        .execute_parsed_commands_for_test(91513, CommandParser::new().parse(&command).unwrap())
        .await
        .unwrap();
    assert_eq!(
        query(&handler, &session, 0).await,
        b"\x1b]10;rgb:ffff/ffff/ffff\x07"
    );
}
