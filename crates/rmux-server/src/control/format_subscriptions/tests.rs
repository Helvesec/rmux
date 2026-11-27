use super::{
    control_escaped_len, ControlFormatDefinition, ControlFormatLimits, ControlFormatNotification,
    ControlFormatNotificationBatch, ControlFormatSelector, ControlFormatSubscription,
    ControlFormatSubscriptions,
};
use crate::handler::RequestHandler;
use rmux_proto::{
    NewSessionRequest, NewWindowRequest, Request, Response, SelectWindowRequest, SessionName,
    WindowTarget,
};

#[test]
fn notification_escaping_preserves_field_bytes_and_counts() {
    let cases = [
        ("back\\slash", r"back\\slash"),
        ("\\012\\n\\\\", r"\\012\\n\\\\"),
        ("\\\n\\\t\0", r"\\\n\\\t\000"),
        (
            "\0\x07\x08\t\n\x0b\x0c\r\x01\x1b\x7f",
            r"\000\a\b\t\n\v\f\r\001\033\177",
        ),
        ("texte café 日本 % : 012", "texte café 日本 % : 012"),
        ("", ""),
    ];
    for (input, expected) in cases {
        assert_eq!(
            control_escaped_len(input),
            expected.len(),
            "input={input:?}"
        );
        for in_name in [false, true] {
            let notification = ControlFormatNotification {
                name: if in_name { input } else { "e" }.to_owned(),
                session_id: "$0".to_owned(),
                window_id: Some("@0".to_owned()),
                window_index: Some(0),
                pane_id: Some("%0".to_owned()),
                value: if in_name { "value" } else { input }.to_owned(),
            };
            let expected_name = if in_name { expected } else { "e" };
            let expected_value = if in_name { "value" } else { expected };
            let expected_line =
                format!("%subscription-changed {expected_name} $0 @0 0 %0 : {expected_value}");
            let encoded_bytes = notification.encoded_bytes();
            let line = notification.into_line();
            assert_eq!(
                line.as_bytes(),
                expected_line.as_bytes(),
                "input={input:?}, in_name={in_name}"
            );
            assert_eq!(
                encoded_bytes,
                line.len() + 1,
                "input={input:?}, in_name={in_name}"
            );
        }
    }
}

#[test]
fn definition_parser_preserves_colons_inside_template() {
    assert_eq!(
        ControlFormatDefinition::parse("name:%7:#{pane_id}:tail"),
        ControlFormatDefinition::Upsert {
            name: "name".to_owned(),
            selector: ControlFormatSelector::PaneId(7),
            template: "#{pane_id}:tail".to_owned(),
        }
    );
    assert_eq!(
        ControlFormatDefinition::parse("name:%7"),
        ControlFormatDefinition::Remove("name".to_owned())
    );
}

#[test]
fn selector_parser_uses_current_fallback_and_measured_numeric_prefixes() {
    let cases = [
        ("", ControlFormatSelector::Current),
        ("%*", ControlFormatSelector::AllPanes),
        ("@*", ControlFormatSelector::AllWindows),
        ("%7", ControlFormatSelector::PaneId(7)),
        ("@9", ControlFormatSelector::WindowId(9)),
        ("%", ControlFormatSelector::Current),
        ("@", ControlFormatSelector::Current),
        ("%abc", ControlFormatSelector::Current),
        ("@abc", ControlFormatSelector::Current),
        ("abc", ControlFormatSelector::Current),
        ("%-0", ControlFormatSelector::PaneId(0)),
        ("@-0", ControlFormatSelector::WindowId(0)),
        ("%-000", ControlFormatSelector::PaneId(0)),
        ("@-000", ControlFormatSelector::WindowId(0)),
        ("%+007tail", ControlFormatSelector::PaneId(7)),
        ("@+009tail", ControlFormatSelector::WindowId(9)),
        ("%-1", ControlFormatSelector::Current),
        ("@-1", ControlFormatSelector::Current),
        ("%-0001", ControlFormatSelector::Current),
        ("@-0001", ControlFormatSelector::Current),
        ("%+7", ControlFormatSelector::PaneId(7)),
        ("@+9", ControlFormatSelector::WindowId(9)),
        ("%7tail", ControlFormatSelector::PaneId(7)),
        ("@9tail", ControlFormatSelector::WindowId(9)),
        ("% -0", ControlFormatSelector::PaneId(0)),
        ("@ -0", ControlFormatSelector::WindowId(0)),
    ];

    for (raw, expected) in cases {
        assert_eq!(ControlFormatSelector::parse(raw), expected, "raw={raw:?}");
    }

    assert_eq!(
        ControlFormatSelector::parse("%999999999999999999999999999999"),
        ControlFormatSelector::Missing
    );

    for whitespace in [" ", "\t", "\n", "\x0b", "\x0c", "\r"] {
        assert_eq!(
            ControlFormatSelector::parse(&format!("%{whitespace}0")),
            ControlFormatSelector::PaneId(0),
            "pane whitespace={whitespace:?}"
        );
        assert_eq!(
            ControlFormatSelector::parse(&format!("@{whitespace}0")),
            ControlFormatSelector::WindowId(0),
            "window whitespace={whitespace:?}"
        );
    }
}

#[tokio::test]
async fn nonnumeric_percent_and_at_selectors_evaluate_current_session() {
    let handler = RequestHandler::new();
    let session_name = SessionName::new("selector-fallback").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );

    let mut subscriptions = ControlFormatSubscriptions::default();
    subscriptions
        .apply_definitions(vec![
            "bare_pane:%:#{session_name}:#{pane_id}".into(),
            "bare_window:@:#{session_name}:#{pane_id}".into(),
            "pane_text:%abc:#{session_name}:#{pane_id}".into(),
            "window_text:@abc:#{session_name}:#{pane_id}".into(),
        ])
        .expect("subscriptions apply");

    assert_eq!(
        handler
            .changed_control_format_subscription_notifications(
                Some(&session_name),
                &mut subscriptions,
            )
            .await
            .expect("fallback selectors evaluate")
            .into_lines(),
        [
            "%subscription-changed bare_pane $0 - - - : selector-fallback:%0",
            "%subscription-changed bare_window $0 - - - : selector-fallback:%0",
            "%subscription-changed pane_text $0 - - - : selector-fallback:%0",
            "%subscription-changed window_text $0 - - - : selector-fallback:%0",
        ]
    );
}

#[tokio::test]
async fn signed_zero_and_c_whitespace_selectors_evaluate_id_zero() {
    let handler = RequestHandler::new();
    let session_name = SessionName::new("selector-signed-zero").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: None,
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: Some("current".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: Some(1),
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(session_name.clone(), 1),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    let mut subscriptions = ControlFormatSubscriptions::default();
    subscriptions
        .apply_definitions(vec![
            "current::#{window_id}|#{pane_id}".into(),
            "pane_minus_zero:%-0:#{window_id}|#{pane_id}".into(),
            "window_minus_zero:@-0:#{window_id}|#{pane_id}".into(),
            "pane_space:% 0:#{window_id}|#{pane_id}".into(),
            "window_tab:@\t0:#{window_id}|#{pane_id}".into(),
        ])
        .expect("subscriptions apply");

    assert_eq!(
        handler
            .changed_control_format_subscription_notifications(
                Some(&session_name),
                &mut subscriptions,
            )
            .await
            .expect("selectors evaluate")
            .into_lines(),
        [
            "%subscription-changed current $0 - - - : @1|%1",
            "%subscription-changed pane_minus_zero $0 @0 0 %0 : @0|%0",
            "%subscription-changed window_minus_zero $0 @0 0 - : @0|%0",
            "%subscription-changed pane_space $0 @0 0 %0 : @0|%0",
            "%subscription-changed window_tab $0 @0 0 - : @0|%0",
        ]
    );
}

#[test]
fn definitions_replace_and_remove_by_exact_name() {
    let mut subscriptions = ControlFormatSubscriptions::default();
    subscriptions
        .apply_definitions(vec![
            "one::#{session_name}".to_owned(),
            "two:@0:#{window_id}".to_owned(),
            "one:%0:#{pane_id}".to_owned(),
            "two".to_owned(),
        ])
        .expect("definitions apply");

    assert_eq!(subscriptions.entries.len(), 1);
    assert!(matches!(
        subscriptions.entries.as_slice(),
        [ControlFormatSubscription {
            name,
            selector: ControlFormatSelector::PaneId(0),
            template,
            last_value: None,
            ..
        }] if name == "one" && template == "#{pane_id}"
    ));
}

#[test]
fn definition_batches_are_bounded_before_registry_mutation() {
    let limits = ControlFormatLimits {
        max_definition_batch: 2,
        max_definition_batch_bytes: 10,
        max_subscriptions: 4,
        max_registry_bytes: 64,
        max_retained_value_bytes: 64,
        max_target_cache_entries: 4,
        max_notification_batch: 4,
        max_notification_bytes: 1024,
    };
    let mut subscriptions = ControlFormatSubscriptions::with_limits(limits);
    subscriptions
        .apply_definitions(vec!["one::1".to_owned()])
        .expect("initial definition applies");

    let err = subscriptions
        .apply_definitions(vec!["two::2".to_owned(), "three::3".to_owned()])
        .expect_err("oversized byte batch must be rejected");
    assert_eq!(
        err.message(),
        "control format subscription definition batch exceeds 10 bytes"
    );
    assert_eq!(subscriptions.entries.len(), 1);
    assert!(subscriptions.indexes.contains_key("one"));

    let err = subscriptions
        .apply_definitions(vec![
            "two::2".to_owned(),
            "three::3".to_owned(),
            "four::4".to_owned(),
        ])
        .expect_err("oversized count batch must be rejected");
    assert_eq!(
        err.message(),
        "control format subscription definition batch exceeds 2 entries"
    );
    assert_eq!(subscriptions.entries.len(), 1);
    assert!(subscriptions.indexes.contains_key("one"));
}

#[test]
fn registry_count_and_bytes_are_bounded_transactionally() {
    let limits = ControlFormatLimits {
        max_definition_batch: 4,
        max_definition_batch_bytes: 128,
        max_subscriptions: 2,
        max_registry_bytes: 8,
        max_retained_value_bytes: 64,
        max_target_cache_entries: 4,
        max_notification_batch: 4,
        max_notification_bytes: 1024,
    };
    let mut subscriptions = ControlFormatSubscriptions::with_limits(limits);
    subscriptions
        .apply_definitions(vec!["a::b".to_owned(), "c::d".to_owned()])
        .expect("two small definitions fit");

    let err = subscriptions
        .apply_definitions(vec!["e::f".to_owned()])
        .expect_err("third name must exceed registry count");
    assert_eq!(
        err.message(),
        "control format subscription registry exceeds 2 entries"
    );
    assert_eq!(subscriptions.entries.len(), 2);
    assert!(!subscriptions.indexes.contains_key("e"));

    let err = subscriptions
        .apply_definitions(vec!["c::too-wide".to_owned()])
        .expect_err("wide replacement must exceed registry bytes");
    assert_eq!(
        err.message(),
        "control format subscription registry exceeds 8 bytes"
    );
    assert!(matches!(
        subscriptions.entries.as_slice(),
        [_, ControlFormatSubscription { name, template, .. }] if name == "c" && template == "d"
    ));
}

#[test]
fn notification_batches_are_bounded_by_count_and_encoded_bytes() {
    let limits = ControlFormatLimits {
        max_definition_batch: 4,
        max_definition_batch_bytes: 128,
        max_subscriptions: 4,
        max_registry_bytes: 128,
        max_retained_value_bytes: 128,
        max_target_cache_entries: 4,
        max_notification_batch: 2,
        max_notification_bytes: usize::MAX,
    };
    let notification = ControlFormatNotification {
        name: "name".to_owned(),
        session_id: "$0".to_owned(),
        window_id: Some("@0".to_owned()),
        window_index: Some(0),
        pane_id: Some("%0".to_owned()),
        value: "\n".to_owned(),
    };
    assert_eq!(control_escaped_len("\n"), 2);
    let mut batch = ControlFormatNotificationBatch::new(limits);
    batch.ensure_can_push(&notification).expect("first fits");
    batch.push_unchecked(notification.clone());
    batch.ensure_can_push(&notification).expect("second fits");
    batch.push_unchecked(notification.clone());
    let err = batch
        .ensure_can_push(&notification)
        .expect_err("third notification exceeds count");
    assert_eq!(
        err.message(),
        "control format subscription notification batch exceeds 2 entries"
    );

    let limits = ControlFormatLimits {
        max_notification_batch: 4,
        max_notification_bytes: notification.encoded_bytes(),
        ..limits
    };
    let mut batch = ControlFormatNotificationBatch::new(limits);
    batch
        .ensure_can_push(&notification)
        .expect("exact bytes fit");
    batch.push_unchecked(notification.clone());
    let err = batch
        .ensure_can_push(&notification)
        .expect_err("second notification exceeds encoded bytes");
    assert_eq!(
        err.message(),
        format!(
            "control format subscription notification batch exceeds {} encoded bytes",
            notification.encoded_bytes()
        )
    );
}
