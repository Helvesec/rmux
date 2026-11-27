use super::*;

#[tokio::test]
async fn indexed_scalar_rejection_preserves_sdk_outcomes_events_and_linked_aliases() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("scalar-index").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("scalar"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let alias = EnsureSession::named(session_name("alias"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let linked = rmux
        .cmd(["link-window", "-s", "scalar:0", "-t", "alias:1"])
        .await?;
    assert_eq!(linked.exit, Some(0), "{:?}", linked.stderr);
    let pane = session.pane(0, 0);
    let linked_pane = alias.pane(1, 0);
    let mut events = pane
        .state_events(PaneStateEventsOptions {
            include_title: false,
            include_options: true,
            include_foreground: false,
        })
        .await?;
    let Some(PaneStateEvent::Snapshot { mut revision, .. }) = events.next().await? else {
        panic!("expected initial snapshot")
    };
    for name in [
        "tree-mode-preview-format",
        "tree-mode-preview-style",
        "window-pane-status-format",
        "window-pane-current-status-format",
        "window-status-format",
        "window-status-style",
    ] {
        let indexed = format!("{name}[0]");
        let first = if name.ends_with("style") {
            "fg=red"
        } else {
            "BASE"
        };
        let next = if name.ends_with("style") {
            "fg=blue"
        } else {
            "NEXT"
        };
        for old in [None, Some(first)] {
            assert!(pane.set_option(&indexed, next).await.is_err(), "{indexed}");
            assert!(pane.unset_option(&indexed).await.is_err(), "{indexed}");
            assert_eq!(pane.option(name).await?.as_deref(), old);
            assert_eq!(linked_pane.option(name).await?.as_deref(), old);
            let value = if old.is_none() { first } else { next };
            let mutation = pane.set_option(name, value).await?;
            assert!(mutation.changed);
            assert_eq!(mutation.name, name);
            assert_eq!(mutation.old_value.as_deref(), old);
            assert_eq!(mutation.new_value.as_deref(), Some(value));
            let event = tokio::time::timeout(Duration::from_secs(5), events.next()).await??;
            assert!(
                matches!(event,
                    Some(PaneStateEvent::OptionSet { revision: actual, name: event_name, old_value, new_value, .. })
                    if actual == revision + 1 && event_name == name && old_value.as_deref() == old && new_value == value
                ),
                "valid mutation must follow rejection without hidden events or revisions"
            );
            revision += 1;
            assert_eq!(linked_pane.option(name).await?.as_deref(), Some(value));
        }
        let mutation = pane.unset_option(name).await?;
        assert!(mutation.changed);
        assert_eq!(mutation.old_value.as_deref(), Some(next));
        assert_eq!(mutation.new_value, None);
        let event = tokio::time::timeout(Duration::from_secs(5), events.next()).await??;
        assert!(matches!(event,
            Some(PaneStateEvent::OptionUnset { revision: actual, name: event_name, old_value, .. })
            if actual == revision + 1 && event_name == name && old_value.as_deref() == Some(next)
        ));
        revision += 1;
        assert_eq!(linked_pane.option(name).await?, None);
    }
    drop(events);
    harness.finish().await
}

#[tokio::test]
async fn named_options_keep_explicit_pane_semantics_and_cmd_window_inheritance() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("frozen-options").await?;
    let rmux = harness.rmux();
    let session = EnsureSession::named(session_name("frozen"))
        .create_only()
        .ensure(&rmux)
        .await?;
    let pane = session.pane(0, 0);
    let mut events = pane
        .state_events(PaneStateEventsOptions {
            include_title: false,
            include_options: true,
            include_foreground: false,
        })
        .await?;
    assert!(matches!(
        events.next().await?,
        Some(PaneStateEvent::Snapshot { .. })
    ));
    for name in [
        "tree-mode-preview-format",
        "tree-mode-preview-style",
        "window-pane-status-format",
        "window-pane-current-status-format",
    ] {
        let inherited = if name.ends_with("-style") {
            "fg=red"
        } else {
            "inherited"
        };
        let explicit = if name.ends_with("-style") {
            "fg=blue"
        } else {
            "explicit"
        };
        let set = rmux.cmd(["set", "-wg", name, inherited]).await?;
        assert_eq!(set.exit, Some(0), "{:?}", set.stderr);
        assert_eq!(pane.option(name).await?, None);
        let format = format!("#{{{name}}}");
        let shown = rmux
            .cmd(["display-message", "-p", "-t", "frozen:0.0", &format])
            .await?;
        assert_eq!(shown.stdout, format!("{inherited}\n").as_bytes());
        let mutation = pane.set_option(name, explicit).await?;
        assert!(mutation.changed);
        assert!(
            matches!(events.next().await?, Some(PaneStateEvent::OptionSet { name: event_name, new_value, .. }) if event_name == name && new_value == explicit)
        );
        assert_eq!(mutation.old_value, None);
        assert_eq!(pane.option(name).await?.as_deref(), Some(explicit));
        assert!(!pane.set_option(name, explicit).await?.changed);
        assert!(pane.unset_option(name).await?.changed);
        assert!(
            matches!(events.next().await?, Some(PaneStateEvent::OptionUnset { name: event_name, .. }) if event_name == name)
        );
        assert!(!pane.unset_option(name).await?.changed);
        assert_eq!(pane.option(name).await?, None);
        let shown = rmux
            .cmd(["display-message", "-p", "-t", "frozen:0.0", &format])
            .await?;
        assert_eq!(shown.stdout, format!("{inherited}\n").as_bytes());
    }
    pane.set_option("tree-mode-preview-style", "fg=green")
        .await?;
    assert!(matches!(
        events.next().await?,
        Some(PaneStateEvent::OptionSet { .. })
    ));
    assert!(pane
        .set_option("tree-mode-preview-style", "not-a-style")
        .await
        .is_err());
    assert_eq!(
        pane.option("tree-mode-preview-style").await?.as_deref(),
        Some("fg=green")
    );
    pane.unset_option("tree-mode-preview-style").await?;
    assert!(matches!(
        events.next().await?,
        Some(PaneStateEvent::OptionUnset { .. })
    ));
    drop(events);
    harness.finish().await
}
