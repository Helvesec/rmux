use super::*;
use crate::handler::RequestHandler;
use rmux_proto::{NewSessionRequest, Request, Response};

#[tokio::test]
async fn wildcard_targets_keep_independent_values_and_session_scope() {
    let handler = RequestHandler::new();
    let alpha = SessionName::new("alpha").unwrap();
    let beta = SessionName::new("beta").unwrap();
    for name in [&alpha, &beta] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: name.clone(),
                    detached: true,
                    size: None,
                    environment: None,
                }))
                .await,
        );
    }
    let mut subscriptions = ControlFormatSubscriptions::default();
    subscriptions
        .apply_definitions(vec!["panes:%*:same".into(), "windows:@*:same".into()])
        .expect("subscriptions apply");
    let first = handler
        .changed_control_format_subscription_notifications(Some(&alpha), &mut subscriptions)
        .await
        .expect("initial alpha changes evaluate")
        .into_lines();
    assert_eq!(
        first,
        [
            "%subscription-changed panes $0 @0 0 %0 : same",
            "%subscription-changed windows $0 @0 0 - : same",
        ]
    );
    assert!(handler
        .changed_control_format_subscription_notifications(Some(&alpha), &mut subscriptions)
        .await
        .expect("repeat alpha evaluates")
        .into_lines()
        .is_empty());
    assert_eq!(
        handler
            .changed_control_format_subscription_notifications(Some(&beta), &mut subscriptions)
            .await
            .expect("initial beta changes evaluate")
            .into_lines(),
        [
            "%subscription-changed panes $1 @1 0 %1 : same",
            "%subscription-changed windows $1 @1 0 - : same",
        ]
    );
    assert!(handler
        .changed_control_format_subscription_notifications(Some(&alpha), &mut subscriptions)
        .await
        .expect("return alpha evaluates")
        .into_lines()
        .is_empty());
    subscriptions
        .apply_definitions(vec!["panes:%*:same".into(), "windows".into()])
        .expect("replacement/removal applies");
    assert_eq!(
        handler
            .changed_control_format_subscription_notifications(Some(&alpha), &mut subscriptions)
            .await
            .expect("replacement evaluates")
            .into_lines(),
        [first[0].clone()]
    );
}

#[tokio::test]
async fn wildcard_changes_only_changed_targets_and_prunes_deleted_targets() {
    use rmux_proto::{KillWindowRequest, NewWindowRequest, RenameWindowRequest, WindowTarget};
    let handler = RequestHandler::new();
    let name = SessionName::new("multiple").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
        handler
            .handle(Request::RenameWindow(RenameWindowRequest {
                target: WindowTarget::new(name.clone()),
                name: "same".into(),
            }))
            .await,
        Response::RenameWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
                target: rmux_proto::SplitWindowTarget::Session(name.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: name.clone(),
            name: Some("same".into()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(created) = response else {
        panic!("{response:?}")
    };
    let mut subscriptions = ControlFormatSubscriptions::default();
    subscriptions
        .apply_definitions(vec![
            "panes:%*:#{window_name}:#{pane_id}".into(),
            "windows:@*:#{window_name}".into(),
        ])
        .expect("subscriptions apply");
    let initial = handler
        .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
        .await
        .expect("initial changes evaluate")
        .into_lines();
    assert_eq!(
        initial,
        [
            "%subscription-changed panes $0 @0 0 %0 : same:%0",
            "%subscription-changed panes $0 @0 0 %1 : same:%1",
            "%subscription-changed panes $0 @1 1 %2 : same:%2",
            "%subscription-changed windows $0 @0 0 - : same",
            "%subscription-changed windows $0 @1 1 - : same",
        ]
    );
    assert!(handler
        .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
        .await
        .expect("unchanged refresh evaluates")
        .into_lines()
        .is_empty());
    assert!(matches!(
        handler
            .handle(Request::RenameWindow(RenameWindowRequest {
                target: created.target.clone(),
                name: "changed".into(),
            }))
            .await,
        Response::RenameWindow(_)
    ));
    assert_eq!(
        handler
            .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
            .await
            .expect("changed window evaluates")
            .into_lines(),
        [
            "%subscription-changed panes $0 @1 1 %2 : changed:%2",
            "%subscription-changed windows $0 @1 1 - : changed",
        ]
    );
    assert!(matches!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: created.target,
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));
    // Pruning also runs when the client has no current session.
    assert!(handler
        .changed_control_format_subscription_notifications(None, &mut subscriptions)
        .await
        .expect("unattached prune evaluates")
        .into_lines()
        .is_empty());
    for entry in &subscriptions.entries {
        assert_eq!(
            entry.wildcard_values.len(),
            if entry.name == "panes" { 2 } else { 1 }
        );
    }
    assert!(handler
        .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
        .await
        .expect("post-prune refresh evaluates")
        .into_lines()
        .is_empty());
}

#[tokio::test]
async fn retained_values_are_bounded_without_storing_overflow_baseline() {
    let handler = RequestHandler::new();
    let name = SessionName::new("retained-bound").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    let limits = ControlFormatLimits {
        max_definition_batch: 4,
        max_definition_batch_bytes: 128,
        max_subscriptions: 4,
        max_registry_bytes: 128,
        max_retained_value_bytes: 3,
        max_target_cache_entries: 4,
        max_notification_batch: 4,
        max_notification_bytes: 1024,
    };
    let mut subscriptions = ControlFormatSubscriptions::with_limits(limits);
    subscriptions
        .apply_definitions(vec!["value::123".into()])
        .expect("small retained value definition applies");
    assert_eq!(
        handler
            .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
            .await
            .expect("exact retained value fits")
            .into_lines(),
        ["%subscription-changed value $0 - - - : 123"]
    );

    subscriptions
        .apply_definitions(vec!["value::1234".into()])
        .expect("replacement definition still fits registry");
    let err = handler
        .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
        .await
        .expect_err("oversized retained value must fail deterministically");
    assert_eq!(
        err.message(),
        "control format subscription retained values exceeds 3 encoded bytes"
    );
    assert!(subscriptions.entries[0].last_value.is_none());
}

#[tokio::test]
async fn wildcard_target_cache_is_bounded_without_storing_overflow_target() {
    let handler = RequestHandler::new();
    let name = SessionName::new("target-cache-bound").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
                target: rmux_proto::SplitWindowTarget::Session(name.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let limits = ControlFormatLimits {
        max_definition_batch: 4,
        max_definition_batch_bytes: 128,
        max_subscriptions: 4,
        max_registry_bytes: 128,
        max_retained_value_bytes: 1024,
        max_target_cache_entries: 1,
        max_notification_batch: 4,
        max_notification_bytes: 1024,
    };
    let mut subscriptions = ControlFormatSubscriptions::with_limits(limits);
    subscriptions
        .apply_definitions(vec!["panes:%*:#{pane_id}".into()])
        .expect("wildcard definition applies");

    let err = handler
        .changed_control_format_subscription_notifications(Some(&name), &mut subscriptions)
        .await
        .expect_err("second pane must exceed target cache");
    assert_eq!(
        err.message(),
        "control format subscription target cache exceeds 1 entries"
    );
    assert_eq!(
        subscriptions.entries[0].wildcard_values.len(),
        1,
        "overflowing target must not be inserted as a hidden baseline"
    );
}
