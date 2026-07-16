use super::*;

#[tokio::test]
async fn stale_cleanup_identity_preserves_reregistered_same_pid_and_session() {
    let handler = RequestHandler::new();
    let session = session_name("attach-cleanup-aba");
    let attach_pid = 94_201;
    let mut original_rx = create_attached_session(&handler, attach_pid, &session).await;
    let stale_identity = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attach exists")
            .identity(attach_pid)
    };

    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    let replacement_id = handler
        .register_attach(attach_pid, session.clone(), replacement_tx)
        .await;
    assert!(matches!(original_rx.try_recv(), Ok(AttachControl::Detach)));

    let removed = handler
        .remove_attached_clients_for_session(&session, vec![stale_identity])
        .await;

    assert!(removed.is_empty());
    let active_attach = handler.active_attach.lock().await;
    let replacement = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("replacement attach must survive stale cleanup");
    assert_eq!(replacement.id, replacement_id);
    assert_eq!(replacement.session_name, session);
    assert!(matches!(
        replacement_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));
}
