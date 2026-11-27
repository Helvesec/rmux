use super::*;
use crate::{
    TargetFindContext, TargetFindFlags as Flags, TargetFindType as Kind, UnresolvedTarget,
};
use rmux_proto::{PaneTarget, Target};

fn pane(session: &str, window: u32, index: u32) -> Target {
    Target::Pane(PaneTarget::with_window(name(session), window, index))
}

#[test]
fn target_finder_shared_engine_matches_standalone_matrix() {
    let mut standalone = SessionStore::new();
    let mut managed = ManagedSessionStore::new();
    for (session_name, window_name) in [
        ("alpha", "editor"),
        ("alpine", "editorial"),
        ("beta", "logs"),
    ] {
        standalone
            .create_session(name(session_name), size())
            .unwrap();
        managed.create_session(name(session_name), size()).unwrap();
        standalone
            .session_mut(&name(session_name))
            .unwrap()
            .rename_window(0, window_name.to_owned())
            .unwrap();
        let link = managed.target_link(&target(session_name, 0)).unwrap();
        managed
            .windows
            .resolve_mut(link)
            .unwrap()
            .set_name(window_name.to_owned());
    }
    standalone
        .session_mut(&name("alpha"))
        .unwrap()
        .touch_attached();
    managed.touch_attached(&name("alpha")).unwrap();
    let current = TargetFindContext::from_target(pane("alpha", 0, 0));
    let contexts = [
        TargetFindContext::new(None),
        current.clone(),
        current
            .clone()
            .with_marked_target(Some(pane("beta", 0, 0)))
            .with_mouse_target(Some(pane("alpine", 0, 0))),
        TargetFindContext::from_target(pane("missing", 0, 0)),
        TargetFindContext::from_target(pane("missing", 0, 0))
            .forbid_missing_current_target_fallback(),
        current.with_pane_base_indices(std::collections::HashMap::from([((name("alpha"), 0), 3)])),
    ];
    let raw_targets = [
        "",
        ".",
        "{active}",
        "{current}",
        "=",
        "{mouse}",
        "~",
        "{marked}",
        "alpha",
        "alp",
        "bet*",
        "=alp",
        "=alpha",
        "alpha:",
        ":",
        ":editor",
        ":edi",
        ":e*",
        ":=edi",
        "alpha:editor.0",
        "alpha:.0",
        ".0",
        ".3",
        ".!",
        ".+",
        ".-",
        ".{left}",
        ".{right}",
        ".{top}",
        ".{bottom}",
        ".{top-left}",
        ":+",
        ":-",
        ":+2",
        ":-2",
        ":!",
        ":^",
        ":$",
        ":7",
        ":4294967296",
        "$0",
        "$1:",
        "@0",
        "@1",
        "%0",
        "%1",
        "alpha:@0",
        "alpha:%0",
        "alpha:.%0",
        "$",
        "@",
        "%",
        "%999",
        "alpha:0.%0",
        "beta:0.%0",
        "missing",
        "alpha:0.99",
    ];
    for context in &contexts {
        for kind in [Kind::Session, Kind::Window, Kind::Pane] {
            for flags in [
                Flags::NONE,
                Flags::EXACT_SESSION,
                Flags::EXACT_WINDOW,
                Flags::WINDOW_INDEX,
                Flags::DEFAULT_MARKED,
                Flags::PREFER_UNATTACHED,
                Flags::QUIET.union(Flags::CANFAIL),
            ] {
                for raw in raw_targets
                    .iter()
                    .map(|raw| UnresolvedTarget::new(*raw))
                    .chain(std::iter::once(UnresolvedTarget::none()))
                {
                    let expected = standalone.resolve_unresolved_target(&raw, kind, flags, context);
                    let actual = managed.resolve_unresolved_target(&raw, kind, flags, context);
                    assert_eq!(
                        actual, expected,
                        "{raw:?}, {kind:?}, {flags:?}, {context:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn target_finder_canonical_grouped_and_local_alias_preferences() {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("alpha"), size()).unwrap();
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("alpha"))
        .unwrap();
    let id = store.session(&name("alpha")).unwrap().window().id();
    store.link_window(&target("alpha", 4), id).unwrap();
    // Group-coherent insertion now adds both slots. Preserve this resolver
    // fixture's deliberately asymmetric occurrences via internal test setup.
    unlink_fixture_occurrence(&mut store, &target("peer", 4));
    store.select_window(&target("alpha", 4)).unwrap();
    let current = TargetFindContext::from_target(pane("alpha", 4, 0));
    let window_id = format!("@{}", id.as_u32());
    let pane_id = format!(
        "%{}",
        store.window(&target("peer", 0)).unwrap().panes()[0]
            .id()
            .as_u32()
    );
    let resolve = |raw: &str, kind, context: &TargetFindContext| {
        store.resolve_unresolved_target(&UnresolvedTarget::new(raw), kind, Flags::NONE, context)
    };
    assert_eq!(
        resolve(&window_id, Kind::Window, &current).unwrap(),
        Target::Window(target("alpha", 0))
    );
    // A bare %id names one pane and then chooses among the sessions that hold
    // its window, by recency: `peer` was created after `alpha`, so it is the
    // more recently active of the two and the context on alpha:4 does not
    // outrank it (`cmd-find.c:498-512`, 176-197; measured on tmux 3.7b with a
    // client on the older member and the newer one still winning).
    assert_eq!(
        resolve(&pane_id, Kind::Pane, &current).unwrap(),
        pane("peer", 0, 0)
    );
    // Named explicitly, the session is kept - and inside it the ACTIVE link is
    // taken when it holds the window (`cmd-find.c:205-231`). alpha is looking
    // at its alias 4, so 4 is the answer, not the first index.
    assert_eq!(
        resolve(&format!("alpha:.{pane_id}"), Kind::Pane, &current).unwrap(),
        pane("alpha", 4, 0)
    );
    let peer = TargetFindContext::from_target(pane("peer", 0, 0));
    assert_eq!(
        resolve(&window_id, Kind::Window, &peer).unwrap(),
        Target::Window(target("peer", 0))
    );
    // With no context at all a @WindowId still has nothing to resolve against.
    assert!(resolve(&window_id, Kind::Pane, &TargetFindContext::new(None)).is_err());
    // A %PaneId does: it is one pane, and the session is chosen by recency.
    assert_eq!(
        resolve(&pane_id, Kind::Pane, &TargetFindContext::new(None)).unwrap(),
        pane("peer", 0, 0)
    );
    for (raw, index) in [
        (".", 4),
        (":!", 0),
        (":^", 0),
        (":$", 4),
        (":+", 0),
        (":-", 0),
    ] {
        assert_eq!(
            resolve(raw, Kind::Window, &current).unwrap(),
            Target::Window(target("alpha", index))
        );
    }
    // Change the actual owner through the peer: both alpha occurrences must
    // immediately observe the name and selected pane through the resolver.
    let link = store.target_link(&target("peer", 0)).unwrap();
    let owner = store.windows.resolve_mut(link).unwrap();
    owner.set_name("shared-editor".to_owned());
    owner.split_at_position_with_id_and_direction(
        0,
        PaneId::new(50),
        SplitDirection::Vertical,
        false,
    );
    store
        .window_selection_mut(&target("peer", 0))
        .unwrap()
        .select(1, true)
        .unwrap();
    for session in ["alpha", "peer"] {
        let view = store.session(&name(session)).unwrap();
        assert!(std::ptr::eq(
            view.window(),
            store.window(&target("peer", 0)).unwrap()
        ));
    }
    for raw in [
        "alpha:0",
        "alpha:4",
        "peer:shared-editor",
        "peer:shared",
        "peer:*editor",
    ] {
        let result = store
            .resolve_unresolved_target(
                &UnresolvedTarget::new(raw),
                Kind::Pane,
                Flags::NONE,
                &current,
            )
            .unwrap();
        let Target::Pane(result) = result else {
            panic!("expected pane")
        };
        assert_eq!(result.pane_index(), 1);
    }
    for raw in ["alpha:shared-editor", "alpha:shared", "alpha:*editor"] {
        assert!(store
            .resolve_unresolved_target(
                &UnresolvedTarget::new(raw),
                Kind::Window,
                Flags::NONE,
                &current
            )
            .is_err());
    }
    let context = current
        .with_marked_target(Some(pane("peer", 0, 1)))
        .with_mouse_target(Some(pane("alpha", 4, 1)));
    for (raw, expected) in [
        ("{marked}", pane("peer", 0, 1)),
        ("{mouse}", pane("alpha", 4, 1)),
        ("peer:0.+", pane("peer", 0, 0)),
        ("alpha:4.!", pane("alpha", 4, 0)),
    ] {
        assert_eq!(
            store
                .resolve_unresolved_target(
                    &UnresolvedTarget::new(raw),
                    Kind::Pane,
                    Flags::NONE,
                    &context
                )
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        store
            .resolve_unresolved_target(
                &UnresolvedTarget::new("alpha:"),
                Kind::Window,
                Flags::WINDOW_INDEX,
                &context
            )
            .unwrap(),
        Target::Window(target("alpha", 1))
    );
}

#[test]
fn target_finder_managed_defaults_follow_local_recency_and_attachment() {
    let mut store = ManagedSessionStore::new();
    store.create_session(name("alpha"), size()).unwrap();
    store
        .create_grouped_session_with_base_index(name("peer"), size(), 0, name("alpha"))
        .unwrap();
    let resolve = |store: &ManagedSessionStore, flags| {
        store
            .resolve_unresolved_target(
                &UnresolvedTarget::none(),
                Kind::Session,
                flags,
                &TargetFindContext::new(None),
            )
            .unwrap()
    };
    assert_eq!(resolve(&store, Flags::NONE), Target::Session(name("peer")));
    store.touch_attached(&name("alpha")).unwrap();
    assert_eq!(resolve(&store, Flags::NONE), Target::Session(name("alpha")));
    assert_eq!(
        resolve(&store, Flags::PREFER_UNATTACHED),
        Target::Session(name("peer"))
    );
    store
        .rename_session(&name("alpha"), name("renamed"))
        .unwrap();
    assert_eq!(
        resolve(&store, Flags::NONE),
        Target::Session(name("renamed"))
    );
    store.touch_activity(&name("peer")).unwrap();
    assert_eq!(resolve(&store, Flags::NONE), Target::Session(name("peer")));
}
