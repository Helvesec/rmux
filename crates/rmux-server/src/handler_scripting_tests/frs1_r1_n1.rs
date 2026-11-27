use super::frs1::{cleanup, floating_rect, queue, setup, window};
use super::*;
use rmux_proto::{ResizePaneAdjustment, ResizePaneTargetActionRequest};
use std::{
    future::{poll_fn, Future},
    task::Poll,
};

#[tokio::test]
async fn frs1_inherited_session_identity_preserves_unattached_linked_current_pane() {
    let (handler, primary) = setup("frs-inherited-current").await;
    queue(&handler, "select-pane -t frs-inherited-current:0.1")
        .await
        .unwrap();
    let (primary_id, pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&primary).expect("primary session");
        (
            session.id(),
            session.window_at(0).unwrap().pane(1).unwrap().id(),
        )
    };
    let linked = session_name("frs-inherited-other");
    create_background_identity_session(&handler, linked.clone()).await;
    let unrelated = session_name("frs-inherited-unrelated");
    create_background_identity_session(&handler, unrelated.clone()).await;
    let unrelated_id = {
        let state = handler.state.lock().await;
        state.sessions.session(&unrelated).unwrap().id()
    };
    let link = handler
        .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
            source: rmux_proto::WindowTarget::with_window(primary.clone(), 0),
            target: rmux_proto::WindowTarget::with_window(linked, 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)), "{link:?}");

    let request = rmux_proto::ResolveTargetRequest {
        target: None,
        target_type: rmux_proto::ResolveTargetType::Pane,
        window_index: false,
        prefer_unattached: false,
    };
    let context = crate::handler::target_support::RequesterEnvironmentContext {
        pane_id: Some(pane_id.as_u32()),
        session_id: Some(primary_id),
        source_depth: None,
    };
    let resolved = handler
        .resolve_target_for_requester_with_environment(
            request.clone(),
            context,
            None,
            Some(unrelated.clone()),
            |_, target| Ok(target),
        )
        .await
        .unwrap();
    assert_eq!(
        resolved,
        Target::Pane(PaneTarget::with_window(primary.clone(), 0, 1))
    );

    let stale_context = crate::handler::target_support::RequesterEnvironmentContext {
        pane_id: Some(pane_id.as_u32()),
        session_id: Some(unrelated_id),
        source_depth: None,
    };
    let error = handler
        .resolve_target_for_requester_with_environment(
            request,
            stale_context,
            None,
            Some(unrelated),
            |_, target| Ok(target),
        )
        .await
        .expect_err("a known but missing inherited occurrence must not fall back");
    assert!(
        error
            .to_string()
            .contains("is not present in inherited session"),
        "unexpected error: {error}"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn frs1_global_id_preserves_linked_current_occurrence() {
    let (handler, name) = setup("frs-linked-current").await;
    queue(&handler, "select-pane -t frs-linked-current:0.1")
        .await
        .unwrap();
    let pane_id = window(&handler, &name).await.pane(1).unwrap().id();
    let raw = pane_id.to_string();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::pane_io::AttachControl>();
    handler
        .register_attach(std::process::id(), name.clone(), tx)
        .await;
    let resolve_request = || rmux_proto::ResolveTargetRequest {
        target: Some(raw.clone()),
        target_type: rmux_proto::ResolveTargetType::Pane,
        window_index: false,
        prefer_unattached: false,
    };

    let unlinked = handler
        .handle(Request::ResolveTarget(resolve_request()))
        .await;
    let linked = session_name("frs-linked-other");
    create_background_identity_session(&handler, linked.clone()).await;
    let link = handler
        .handle(Request::LinkWindow(rmux_proto::LinkWindowRequest {
            source: rmux_proto::WindowTarget::with_window(name.clone(), 0),
            target: rmux_proto::WindowTarget::with_window(linked.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)), "{link:?}");

    // Two sessions hold this pane's window now. A bare `%PaneId` is not
    // ambiguous: tmux finds the pane in its one global tree and then chooses
    // among the sessions that hold its window - the most recently active of
    // them, the first by name on a tie (`cmd-find.c:498-512` through
    // `cmd_find_best_session_with_window` at 176-197), with no branch on the
    // asking context. Which session is the most recent is stated here rather
    // than left to whatever the commands above last touched.
    touch(&handler, &linked).await;
    let linked_resolve = handler
        .handle(Request::ResolveTarget(resolve_request()))
        .await;
    let action = handler
        .handle(Request::ResizePaneTargetAction(
            ResizePaneTargetActionRequest {
                target: Some(raw.clone()),
                adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 23 },
            },
        ))
        .await;
    let global_width = floating_rect(&window(&handler, &name).await, 1).cols();
    let qualified = handler
        .handle(Request::ResizePaneTargetAction(
            ResizePaneTargetActionRequest {
                target: Some("frs-linked-current:0.1".to_owned()),
                adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 24 },
            },
        ))
        .await;
    let final_width = floating_rect(&window(&handler, &name).await, 1).cols();
    queue(&handler, "select-pane -t frs-linked-current:0.0")
        .await
        .unwrap();
    // Changing the current pane changes nothing for a bare id: the choice is
    // the session, not the context. The last command addressed the primary
    // session, so the recency is stated again here, just before the reading.
    touch(&handler, &linked).await;
    let after_the_current_pane_changed = handler
        .handle(Request::ResolveTarget(resolve_request()))
        .await;
    let resolved_pane_id = resolved_pane(&handler, &after_the_current_pane_changed).await;
    cleanup(&handler).await;

    assert!(matches!(unlinked, Response::ResolveTarget(_)));
    assert!(matches!(qualified, Response::ResizePane(_)));
    assert_eq!(final_width, 24);
    assert!(
        matches!(linked_resolve, Response::ResolveTarget(ref response)
            if response.target
                == rmux_proto::Target::Pane(PaneTarget::with_window(linked.clone(), 1, 1))),
        "the most recently active session holding the window answers: {linked_resolve:?}"
    );
    assert!(
        matches!(action, Response::ResizePane(_)),
        "linked current float ID must still resize: {action:?}"
    );
    assert_eq!(global_width, 23);
    // The same id still resolves after the current pane moved, to the same
    // canonical pane, under the same stated recency.
    assert!(
        matches!(after_the_current_pane_changed, Response::ResolveTarget(ref response)
            if response.target
                == rmux_proto::Target::Pane(PaneTarget::with_window(linked, 1, 1))),
        "a bare id does not become ambiguous when the current pane changes: \
         {after_the_current_pane_changed:?}"
    );
    assert_eq!(
        resolved_pane_id,
        Some(pane_id),
        "and it is the same canonical pane, whichever occurrence names it"
    );
}

/// States which session is the most recently active one, without attaching.
async fn touch(handler: &RequestHandler, session: &SessionName) {
    handler
        .state
        .lock()
        .await
        .sessions
        .touch_activity(session)
        .expect("the session exists");
}

/// The canonical pane identity behind a resolved pane target.
async fn resolved_pane(
    handler: &RequestHandler,
    response: &Response,
) -> Option<rmux_proto::PaneId> {
    let Response::ResolveTarget(response) = response else {
        return None;
    };
    let rmux_proto::Target::Pane(target) = &response.target else {
        return None;
    };
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())?
        .window_at(target.window_index())?
        .pane(target.pane_index())
        .map(rmux_core::Pane::id)
}

#[tokio::test]
async fn frs1_relative_target_keeps_resolution_and_capture_atomic() {
    let (handler, name) = setup("frs-relative-exact").await;
    queue(
        &handler,
        "new-pane -d -t frs-relative-exact:0.0 -x18 -y5 -X2 -Y3",
    )
    .await
    .unwrap();
    let before = window(&handler, &name).await;
    let original = before.pane(1).unwrap().id();
    let replacement = before.pane(2).unwrap().id();

    let held = handler.state.lock().await;
    let mut resize = Box::pin(handler.handle_resize_pane_target_action(
        std::process::id(),
        ResizePaneTargetActionRequest {
            target: Some(":0.1".to_owned()),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 19 },
        },
    ));
    poll_fn(|cx| {
        assert!(resize.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut preference_bridge = Box::pin(handler.state.lock());
    poll_fn(|cx| {
        assert!(preference_bridge.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(held);
    poll_fn(|cx| {
        assert!(resize.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let bridge = preference_bridge.await;
    let mut interloper = Box::pin(handler.state.lock());
    poll_fn(|cx| {
        assert!(interloper.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(bridge);
    poll_fn(|cx| {
        assert!(resize.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;

    let mut state = interloper.await;
    let target = rmux_proto::WindowTarget::with_window(name.clone(), 0);
    super::frs1::swap_fixture_identities(&mut state.sessions, &target, 1, 2);
    drop(state);

    let response = resize.await;
    let after = window(&handler, &name).await;
    cleanup(&handler).await;
    assert_eq!(after.pane(1).unwrap().id(), replacement);
    assert_eq!(after.pane(2).unwrap().id(), original);
    assert_ne!(floating_rect(&after, 1).cols(), 19);
    assert!(matches!(response, Response::Error(_)));
}
