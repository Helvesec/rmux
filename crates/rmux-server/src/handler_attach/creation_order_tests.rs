use super::*;
use crate::control::ControlModeUpgrade;
use rmux_proto::{ControlMode, SessionName, TerminalSize};

async fn control(
    handler: &RequestHandler,
    pid: u32,
) -> (
    u64,
    tokio::sync::mpsc::Receiver<crate::control::ControlServerEvent>,
) {
    let (tx, rx) = mpsc::channel(64);
    let id = handler
        .register_control_with_closing(
            pid,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            tx,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    (id, rx)
}

#[tokio::test]
async fn client_creation_order_is_shared_and_reserved_before_registration() {
    let handler = RequestHandler::new();
    let name = SessionName::new("order").unwrap();
    handler
        .state
        .lock()
        .await
        .sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let (first, _rx1) = control(&handler, 51001).await;
    let reserved = handler.reserve_attach_id().await;
    let (third, _rx3) = control(&handler.clone(), 51003).await;
    assert!(
        first < reserved && reserved < third,
        "control, first-frame reservation, control: {first}, {reserved}, {third}"
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let registration = AttachRegistration {
        control_tx: tx,
        control_backlog: Arc::new(AtomicUsize::new(0)),
        closing: Arc::new(AtomicBool::new(false)),
        persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
        terminal_context: OuterTerminalContext::default(),
        client_title: None,
        flags: super::super::ClientFlags::default(),
        render_stream: false,
        uid: current_owner_uid(),
        user: handler.server_owner_identity(),
        can_write: true,
        client_size: None,
    };
    let identity = handler
        .register_attach_identity(
            51002,
            super::AttachRegistrationTarget {
                session_name: name.clone(),
                expected_session_id: None,
            },
            registration,
            Some(reserved),
            None,
            super::AttachRegistrationClient::new("A".to_owned(), super::AttachFocusOrigin::Native),
        )
        .await
        .unwrap();
    assert_eq!(identity.attach_id(), reserved);
    let snapshots = handler.list_clients_snapshot().await;
    assert_eq!(
        snapshots.iter().map(|c| c.pid).collect::<Vec<_>>(),
        [51001, 51002, 51003]
    );
    handler.finish_attach(51002, reserved).await;
    let (tx, _rx4) = mpsc::unbounded_channel();
    let fourth = handler.register_attach(51002, name, tx).await;
    assert!(fourth > third, "reattach reserves a new creation position");
    // Finishing the retired generation must not delete the new registration.
    handler.finish_attach(51002, reserved).await;
    let snapshots = handler.list_clients_snapshot().await;
    assert_eq!(
        snapshots.iter().map(|c| c.pid).collect::<Vec<_>>(),
        [51001, 51003, 51002]
    );
}
