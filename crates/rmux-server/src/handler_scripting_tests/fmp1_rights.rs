use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use crate::server_access::AccessMode;
use rmux_core::{command_parser::CommandParser, LifecycleEvent};
use rmux_ipc::PeerIdentity;
use rmux_os::identity::UserIdentity;
use std::future::{poll_fn, Future};
use std::task::Poll;

async fn exercise_control_commit(revoke: bool) {
    let (handler, name) = if revoke {
        super::frs1::setup_quiet_window("fmp-review-rights").await
    } else {
        super::frs1::setup("fmp-review-rights").await
    };
    let pid = std::process::id();
    let _control = super::super::register_control_for_session(&handler, pid, name.clone()).await;
    let before = super::frs1::window(&handler, &name).await;
    let resize_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();
    let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
    let mut moving = Box::pin(super::frs1::queue(
        &handler,
        "resize-pane -t fmp-review-rights:0.1 -X 17",
    ));
    tokio::select! {
        _ = pause.reached.notified() => {},
        result = &mut moving => panic!("expected capture pause: {result:?}"),
    }

    // Hold the commit mutex, then poll the real queue beyond its first access
    // check. The mutation must revalidate after this wait, under that mutex.
    let state = handler.state.lock().await;
    pause.release.notify_one();
    poll_fn(|cx| {
        assert!(moving.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    if revoke {
        handler
            .active_control
            .lock()
            .await
            .by_pid
            .get_mut(&pid)
            .unwrap()
            .can_write = false;
    }
    drop(state);

    let result = moving.await;
    let after = super::frs1::window(&handler, &name).await;
    let layout_events = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }))
        .count();
    let resize_count_after = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    eprintln!(
        "FMP1-R1 revoke={revoke} result={result:?}, before={:?}, after={:?}, events={layout_events}",
        super::frs1::floating_rect(&before, 1),
        super::frs1::floating_rect(&after, 1)
    );
    super::frs1::cleanup(&handler).await;

    assert_eq!(resize_count_after, resize_count, "move must not resize OS");
    if revoke {
        assert!(
            result.is_err(),
            "rights revoked before state/commit lock must refuse"
        );
        assert_eq!(after, before, "revocation must prevent the mutation");
        assert_eq!(layout_events, 0, "revocation must not notify");
    } else {
        assert!(result.is_ok());
        assert_eq!(super::frs1::floating_rect(&after, 1).x(), 17);
        assert_eq!(layout_events, 1);
    }
}

#[tokio::test]
async fn fmp1_revoked_write_while_waiting_for_commit_refuses() {
    exercise_control_commit(true).await;
}

#[tokio::test]
async fn fmp1_commit_with_unchanged_rights_succeeds() {
    exercise_control_commit(false).await;
}

#[tokio::test]
async fn fmp1_control_queue_authority_is_revalidated_at_commit() {
    for revoke in [false, true] {
        let (handler, name) = super::frs1::setup_quiet_window("fmp-control-rights").await;
        let pid = 843_201;
        let (_control_id, _events) =
            super::super::register_control_for_session(&handler, pid, name.clone()).await;
        let before = super::frs1::window(&handler, &name).await;
        let mut events = handler.subscribe_lifecycle_events();
        let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
        let command = CommandParser::new()
            .parse("resize-pane -t fmp-control-rights:0.1 -X 18")
            .unwrap();
        let mut moving = Box::pin(handler.execute_control_commands(pid, command));
        tokio::select! {
            _ = pause.reached.notified() => {},
            result = &mut moving => panic!("expected capture pause: {result:?}"),
        }
        let state = handler.state.lock().await;
        pause.release.notify_one();
        poll_fn(|cx| {
            assert!(moving.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        if revoke {
            handler
                .active_control
                .lock()
                .await
                .by_pid
                .get_mut(&pid)
                .unwrap()
                .can_write = false;
        }
        drop(state);

        let result = moving.await;
        let after = super::frs1::window(&handler, &name).await;
        let event_count = layout_event_count(&mut events);
        super::frs1::cleanup(&handler).await;
        if revoke {
            assert!(result.error.is_some(), "{result:?}");
            assert_eq!(after, before);
            assert_eq!(event_count, 0);
        } else {
            assert!(result.error.is_none(), "{result:?}");
            assert_eq!(super::frs1::floating_rect(&after, 1).x(), 18);
            assert_eq!(event_count, 1);
        }
    }
}

#[tokio::test]
async fn fmp1_attach_authority_is_revalidated_at_commit() {
    for revoke in [false, true] {
        let (handler, name) = super::frs1::setup_quiet_window("fmp-attach-rights").await;
        let pid = 843_202;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, name.clone(), tx).await;
        let before = super::frs1::window(&handler, &name).await;
        // Drive startup and its name refresh after the snapshot: an unnamed
        // interactive fixture can change rmux -> sh independently of authority.
        // Keep full Window equality below by using the quiet, named fixture.
        super::frs1::wait_for_quiet_window_terminals(&handler, &name).await;
        handler
            .refresh_automatic_window_name_for_pane_target(&rmux_proto::PaneTarget::with_window(
                name.clone(),
                0,
                0,
            ))
            .await;
        let mut events = handler.subscribe_lifecycle_events();
        let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
        let command = CommandParser::new()
            .parse("resize-pane -t fmp-attach-rights:0.1 -Y 12")
            .unwrap();
        let mut moving = Box::pin(handler.execute_parsed_commands_for_test(pid, command));
        tokio::select! {
            _ = pause.reached.notified() => {},
            result = &mut moving => panic!("expected capture pause: {result:?}"),
        }
        let state = handler.state.lock().await;
        pause.release.notify_one();
        poll_fn(|cx| {
            assert!(moving.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        if revoke {
            handler
                .active_attach
                .lock()
                .await
                .by_pid
                .get_mut(&pid)
                .unwrap()
                .can_write = false;
        }
        drop(state);

        let result = moving.await;
        let after = super::frs1::window(&handler, &name).await;
        let event_count = layout_event_count(&mut events);
        super::frs1::cleanup(&handler).await;
        if revoke {
            assert!(
                matches!(&result, Err(rmux_proto::RmuxError::Server(message)) if message == "client is read-only"),
                "{result:?}"
            );
            assert_eq!(after, before);
            assert_eq!(event_count, 0);
        } else {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(super::frs1::floating_rect(&after, 1).y(), 12);
            assert_eq!(event_count, 1);
        }
    }
}

#[tokio::test]
async fn fmp1_detached_authority_is_revalidated_at_commit() {
    for revoke in [false, true] {
        let (handler, name) = super::frs1::setup_quiet_window("fmp-detached-rights").await;
        let pid = 843_203;
        let uid = crate::server_access::current_owner_uid()
            .wrapping_add(43_203)
            .max(1);
        handler
            .set_test_access_mode_for_uid(uid, AccessMode::ReadWrite)
            .unwrap();
        let admission = handler
            .server_access_admission_for_peer(&PeerIdentity {
                pid,
                uid,
                user: UserIdentity::Uid(uid),
            })
            .unwrap();
        let _scope = handler.begin_detached_requester_access(pid, admission);
        let before = super::frs1::window(&handler, &name).await;
        let mut events = handler.subscribe_lifecycle_events();
        let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
        let command = CommandParser::new()
            .parse("resize-pane -t fmp-detached-rights:0.1 -X -8")
            .unwrap();
        let mut moving = Box::pin(handler.execute_parsed_commands_for_test(pid, command));
        tokio::select! {
            _ = pause.reached.notified() => {},
            result = &mut moving => panic!("expected capture pause: {result:?}"),
        }
        let state = handler.state.lock().await;
        pause.release.notify_one();
        poll_fn(|cx| {
            assert!(moving.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        if revoke {
            handler
                .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
                .unwrap();
        }
        drop(state);

        let result = moving.await;
        let after = super::frs1::window(&handler, &name).await;
        let event_count = layout_event_count(&mut events);
        super::frs1::cleanup(&handler).await;
        if revoke {
            assert!(result.is_err(), "{result:?}");
            assert_eq!(after, before);
            assert_eq!(event_count, 0);
        } else {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(super::frs1::floating_rect(&after, 1).x(), -8);
            assert_eq!(event_count, 1);
        }
    }
}

fn layout_event_count(
    events: &mut tokio::sync::broadcast::Receiver<crate::handler::QueuedLifecycleEvent>,
) -> usize {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }))
        .count()
}
