use super::*;
use rmux_proto::{OptionScopeSelector, SetOptionByNameRequest};

#[tokio::test]
async fn pane_state_events_indexed_window_unset_synchronizes_linked_aliases() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "indexed-unset-alpha").await;
    let beta = create_session(&handler, "indexed-unset-beta").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let alpha_target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let beta_target = PaneTarget::with_window(beta.clone(), 0, 0);
    let set = set_option(
        &handler,
        PaneTargetRef::slot(alpha_target.clone()),
        "status-format[1]",
        "keep",
    )
    .await;
    assert!(matches!(set, Response::PaneOptionSet(_)), "{set:?}");
    for target in [&alpha_target, &beta_target] {
        assert_eq!(
            get_option(
                &handler,
                PaneTargetRef::slot(target.clone()),
                "status-format[1]"
            )
            .await,
            Some("keep".into())
        );
    }
    let (alpha_subscription, pane_id, alpha_snapshot) =
        subscribe(&handler, 1201, alpha_target.clone()).await;
    let (beta_subscription, beta_pane_id, beta_snapshot) =
        subscribe(&handler, 1202, beta_target.clone()).await;
    assert_eq!(pane_id, beta_pane_id);

    let request = SetOptionByNameRequest {
        scope: OptionScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
        name: "status-format[0]".into(),
        value: None,
        mode: SetOptionMode::Replace,
        only_if_unset: false,
        unset: true,
        unset_pane_overrides: true,
        format: false,
        format_target: None,
    };
    let response = handler
        .handle(Request::SetOptionByName(Box::new(request.clone())))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    {
        let state = handler.state.lock().await;
        for target in [&alpha_target, &beta_target] {
            assert_eq!(
                state
                    .options
                    .explicit_value_by_name(
                        &OptionScopeSelector::Pane(target.clone()),
                        "status-format"
                    )
                    .unwrap()
                    .1,
                None
            );
        }
    }
    let mut revisions = Vec::new();
    for (connection, subscription, snapshot) in [
        (1201, alpha_subscription, alpha_snapshot),
        (1202, beta_subscription, beta_snapshot),
    ] {
        let response = read_cursor(&handler, connection, subscription, snapshot.revision).await;
        let Response::PaneStateCursor(response) = response else {
            panic!("{response:?}")
        };
        assert_eq!(response.events.len(), 1, "{:?}", response.events);
        match &response.events[0] {
            PaneStateEventDto::OptionUnset {
                pane_id: event_pane_id,
                name,
                old_value,
                revision,
                ..
            } => {
                assert_eq!(*event_pane_id, pane_id);
                assert_eq!(name, "status-format");
                assert_eq!(old_value.as_deref(), Some("keep"));
                revisions.push((connection, subscription, *revision));
            }
            event => panic!("expected one unset, got {event:?}"),
        }
    }
    let response = handler
        .handle(Request::SetOptionByName(Box::new(request)))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    for (connection, subscription, revision) in revisions {
        assert_no_events(read_cursor(&handler, connection, subscription, revision).await);
    }
}
