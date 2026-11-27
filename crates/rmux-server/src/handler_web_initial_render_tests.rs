use super::*;
use std::cell::Cell;
use std::rc::Rc;

fn observe_initial_render(
    handler: &RequestHandler,
    independent: &SessionName,
) -> Rc<Cell<Option<bool>>> {
    let result = Rc::new(Cell::new(None));
    let observed = result.clone();
    let state = handler.state.clone();
    let independent = independent.clone();
    crate::renderer::initial_render_observer::install(move || {
        observed.set(Some(
            state
                .try_lock()
                .is_ok_and(|state| state.sessions.session(&independent).is_some()),
        ));
    });
    result
}

#[tokio::test]
async fn web_initial_render_allows_independent_lookup_on_open_refresh_and_select() {
    let handler = RequestHandler::new();
    let session = new_session(&handler, "web-initial-render").await;
    let independent = new_session(&handler, "web-initial-independent").await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session.clone(),
                name: Some("selected".to_owned()),
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let target = {
        let state = handler.state.lock().await;
        WebSessionTarget::new(
            session.clone(),
            state.sessions.session(&session).unwrap().id(),
        )
    };
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session.clone())),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().expect("spectator URL"));
    let open = observe_initial_render(&handler, &independent);
    let WebShareStream::Session(stream) = handler.open_web_share(&token, None).await.expect("open")
    else {
        panic!("session stream required");
    };
    let refresh = observe_initial_render(&handler, &independent);
    handler
        .web_session_snapshot(&target)
        .await
        .expect("refresh");
    let select = observe_initial_render(&handler, &independent);
    let selection = handler
        .web_session_select_window_for_view(&target, stream.attach_pid(), 1)
        .await
        .expect("select")
        .expect("selected window");
    let selected_refresh = observe_initial_render(&handler, &independent);
    handler
        .web_session_snapshot_with_scrolls(&target, Some(selection), &HashMap::new())
        .await
        .expect("selected refresh");
    assert_eq!(selection.window_index(), 1);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&session)
            .unwrap()
            .active_window_index(),
        0,
        "spectator selection must preserve the canonical selection"
    );
    assert_eq!([open.get(), refresh.get(), select.get(), selected_refresh.get()], [Some(true); 4],
        "every initial renderer must allow an independent session lookup: open, refresh, select, selected refresh");
}

#[tokio::test]
async fn web_initial_render_preserves_captured_status_options() {
    let handler = RequestHandler::new();
    let session = new_session(&handler, "web-owned-status").await;
    let target = {
        let mut state = handler.state.lock().await;
        for (option, value) in [
            (OptionName::StatusLeft, "CAPTURED"),
            (OptionName::StatusRight, ""),
        ] {
            state
                .options
                .set(
                    ScopeSelector::Session(session.clone()),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .unwrap();
        }
        WebSessionTarget::new(
            session.clone(),
            state.sessions.session(&session).unwrap().id(),
        )
    };
    let state = handler.state.clone();
    crate::renderer::initial_render_observer::install(move || {
        state
            .try_lock()
            .expect("initial rendering released state")
            .options
            .set(
                ScopeSelector::Session(session),
                OptionName::StatusLeft,
                "REPLACED".to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
    });
    let snapshot = handler
        .web_session_snapshot(&target)
        .await
        .expect("captured snapshot");
    let bytes = snapshot.ansi_bytes();
    assert!(bytes
        .windows(b"CAPTURED".len())
        .any(|part| part == b"CAPTURED"));
    assert!(!bytes
        .windows(b"REPLACED".len())
        .any(|part| part == b"REPLACED"));
    let next = handler
        .web_session_snapshot(&target)
        .await
        .expect("next snapshot")
        .ansi_bytes();
    assert!(next
        .windows(b"REPLACED".len())
        .any(|part| part == b"REPLACED"));
}

#[tokio::test]
async fn web_initial_selection_cannot_overwrite_a_newer_attach_generation() {
    let handler = RequestHandler::new();
    let session = new_session(&handler, "web-selection-generation").await;
    let target = {
        let state = handler.state.lock().await;
        WebSessionTarget::new(
            session.clone(),
            state.sessions.session(&session).unwrap().id(),
        )
    };
    let created = create_share(&handler, share_request(WebShareScope::Session(session))).await;
    let token = token_from_url(created.spectator_url.as_deref().unwrap());
    let WebShareStream::Session(stream) = handler.open_web_share(&token, None).await.unwrap()
    else {
        panic!("session stream required");
    };
    let pid = stream.attach_pid();
    let active_attach = handler.active_attach.clone();
    let expected = Rc::new(Cell::new(0));
    let observed = expected.clone();
    crate::renderer::initial_render_observer::install(move || {
        let mut attached = active_attach
            .try_lock()
            .expect("attach lock is free during rendering");
        let active = attached.by_pid.get_mut(&pid).unwrap();
        active.render_generation += 1;
        observed.set(active.render_generation);
    });
    assert!(
        handler
            .web_session_select_window_for_view(&target, pid, 0)
            .await
            .is_err(),
        "a frame captured before a newer attach generation must not replace it"
    );
    assert_eq!(
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&pid)
            .unwrap()
            .render_generation,
        expected.get(),
        "rejection must not publish a Switch or advance its generation"
    );
}

#[tokio::test]
async fn web_initial_open_rejects_selection_changed_before_registration() {
    let handler = RequestHandler::new();
    let session = new_session(&handler, "web-open-selection-race").await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    let created = create_share(
        &handler,
        share_request(WebShareScope::Session(session.clone())),
    )
    .await;
    let token = token_from_url(created.spectator_url.as_deref().unwrap());
    let state = handler.state.clone();
    let selection = WindowTarget::with_window(session, 1);
    crate::renderer::initial_render_observer::install(move || {
        state
            .try_lock()
            .expect("initial rendering released state")
            .sessions
            .select_window(&selection)
            .expect("independent selection");
    });
    assert!(
        handler.open_web_share(&token, None).await.is_err(),
        "the initial target must be revalidated after registration closes the notification gap"
    );
    assert!(
        handler.active_attach.lock().await.by_pid.is_empty(),
        "a rejected initial capture must release its virtual attach"
    );
}
