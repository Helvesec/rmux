use super::*;

fn set_pane(store: &mut OptionStore, pane: &PaneTarget, name: &str, value: &str) {
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane.clone()),
            name,
            Some(value.into()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .unwrap();
}

#[test]
fn window_unset_related_describes_complete_entries_and_preserves_other_scopes() {
    for (query, entries, expected) in [
        (
            "status-format[0]",
            vec![("status-format[0]", "first"), ("status-format[1]", "keep")],
            "first ,keep",
        ),
        (
            "status-format",
            vec![("status-format[0]", "first"), ("status-format[1]", "keep")],
            "first ,keep",
        ),
        ("window-style", vec![("window-style", "fg=red")], "fg=red"),
        ("@custom", vec![("@custom", "value")], "value"),
    ] {
        let mut store = OptionStore::new();
        let window = WindowTarget::with_window(session_name("alpha"), 0);
        let pane = PaneTarget::with_window(session_name("alpha"), 0, 0);
        let sibling = PaneTarget::with_window(session_name("alpha"), 0, 1);
        let empty = PaneTarget::with_window(session_name("alpha"), 0, 2);
        let other_window = PaneTarget::with_window(session_name("alpha"), 1, 0);
        let other_session = PaneTarget::with_window(session_name("beta"), 0, 0);
        for target in [&pane, &sibling, &other_window, &other_session] {
            for (name, value) in &entries {
                set_pane(&mut store, target, name, value);
            }
        }
        for target in [&pane, &empty] {
            set_pane(&mut store, target, "@unrelated", "untouched");
        }
        let canonical = query.split('[').next().unwrap();
        let before_other_window = store.panes[&other_window].clone();
        let before_other_session = store.panes[&other_session].clone();
        let outcome = store
            .set_by_name(
                OptionScopeSelector::Window(window.clone()),
                query,
                None,
                SetOptionMode::Replace,
                false,
                true,
                true,
            )
            .unwrap();
        assert!(!outcome.changed);
        assert_eq!(outcome.related.len(), 2, "{query}");
        for target in [&pane, &sibling] {
            let related = outcome
                .related
                .iter()
                .find(|r| r.scope == OptionScopeSelector::Pane(target.clone()))
                .unwrap();
            assert_eq!(related.name, canonical);
            assert_eq!(related.old_explicit.as_deref(), Some(expected), "{query}");
            assert_eq!(related.new_explicit, None);
            assert!(related.changed);
            assert!(related.related.is_empty());
            assert!(!store
                .panes
                .get(target)
                .is_some_and(|n| n.entries.contains_key(canonical)));
        }
        assert_eq!(
            store.panes[&pane].value("@unrelated", None),
            Some("untouched")
        );
        assert_eq!(
            store.panes[&empty].value("@unrelated", None),
            Some("untouched")
        );
        assert_eq!(store.panes[&other_window], before_other_window);
        assert_eq!(store.panes[&other_session], before_other_session);
        let before_noop = store.clone();
        let noop = store
            .set_by_name(
                OptionScopeSelector::Window(window),
                query,
                None,
                SetOptionMode::Replace,
                false,
                true,
                true,
            )
            .unwrap();
        assert!(!noop.changed);
        assert!(noop.related.is_empty());
        assert_eq!(store, before_noop);
    }
}

#[test]
fn indexed_window_unset_reports_whole_pane_entry_when_requested_index_is_absent() {
    let mut store = OptionStore::new();
    let window = WindowTarget::with_window(session_name("alpha"), 0);
    let pane = PaneTarget::with_window(session_name("alpha"), 0, 0);
    store
        .set_by_name(
            OptionScopeSelector::Pane(pane.clone()),
            "status-format[1]",
            Some("keep".into()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .unwrap();

    let outcome = store
        .set_by_name(
            OptionScopeSelector::Window(window),
            "status-format[0]",
            None,
            SetOptionMode::Replace,
            false,
            true,
            true,
        )
        .unwrap();

    // The existing operation removes the whole pane entry, including index 1.
    assert_eq!(store.pane_value(&pane, OptionName::StatusFormat), None);
    assert!(!outcome.changed);
    assert_eq!(outcome.old_explicit, None);
    assert_eq!(outcome.new_explicit, None);
    assert_eq!(outcome.related.len(), 1);
    let related = &outcome.related[0];
    assert_eq!(related.name, "status-format");
    assert_eq!(related.scope, OptionScopeSelector::Pane(pane));
    assert_eq!(related.old_explicit.as_deref(), Some("keep"));
    assert_eq!(related.new_explicit, None);
    assert!(related.changed);
    assert!(related.related.is_empty());
}
