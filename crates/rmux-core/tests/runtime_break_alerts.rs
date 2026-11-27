// The supplied eight-case witness compares against the pre-migration server sequence.
use rmux_core::*;
use rmux_proto::{SessionName, TerminalSize, WindowTarget};
use std::collections::BTreeMap;
fn name(s: &str) -> SessionName {
    SessionName::new(s).unwrap()
}
fn target(s: &str, i: u32) -> WindowTarget {
    WindowTarget::with_window(name(s), i)
}
fn size() -> TerminalSize {
    TerminalSize::new(80, 24)
}
#[test]
fn runtime_break_retains_the_relocated_peer_occurrence_alerts() {
    let mut failures = Vec::new();
    for duplicate in [false, true] {
        for before in [false, true] {
            for detached in [false, true] {
                let mut old = SessionStore::new();
                let mut new = ManagedSessionStore::new();
                old.create_session(name("a"), size()).unwrap();
                new.create_session(name("a"), size()).unwrap();
                old.session_mut(&name("a"))
                    .unwrap()
                    .insert_window_with_initial_pane(4, size())
                    .unwrap();
                let _ = new
                    .insert_window_with_initial_pane(&name("a"), 4, size())
                    .unwrap();
                if duplicate {
                    let w = old
                        .session(&name("a"))
                        .unwrap()
                        .window_at(4)
                        .unwrap()
                        .clone();
                    old.session_mut(&name("a"))
                        .unwrap()
                        .link_window(9, w, false, false)
                        .unwrap();
                    let id = new.window(&target("a", 4)).unwrap().id();
                    new.link_window(&target("a", 9), id).unwrap();
                } else {
                    old.session_mut(&name("a"))
                        .unwrap()
                        .insert_window_with_initial_pane(9, size())
                        .unwrap();
                    let _ = new
                        .insert_window_with_initial_pane(&name("a"), 9, size())
                        .unwrap();
                }
                old.create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
                    .unwrap();
                new.create_grouped_session_with_base_index(name("peer"), size(), 0, name("a"))
                    .unwrap();
                old.session_mut(&name("peer"))
                    .unwrap()
                    .select_window(9)
                    .unwrap();
                new.select_window(&target("peer", 9)).unwrap();
                for (i, f) in [
                    (0, WINLINK_ACTIVITY),
                    (4, WINLINK_BELL),
                    (9, WINLINK_SILENCE),
                ] {
                    old.session_mut(&name("peer"))
                        .unwrap()
                        .add_winlink_alert_flags(i, f);
                    new.add_winlink_alert_flags(&target("peer", i), f).unwrap();
                }
                let ptr = std::ptr::from_ref(new.window(&target("a", 4)).unwrap());
                let anchor = if before { 0 } else { 7 };
                let dest = old
                    .session_mut(&name("a"))
                    .unwrap()
                    .break_pane(
                        SessionPaneTarget::new(4, 0),
                        BreakPaneOptions::new(Some(anchor), None, detached, false, before),
                    )
                    .unwrap();
                let source = old.session(&name("a")).unwrap().clone();
                let map = BTreeMap::from([
                    (0, if before { 1 } else { 0 }),
                    (4, dest),
                    (9, if before { 10 } else { 9 }),
                ]);
                old.session_mut(&name("peer"))
                    .unwrap()
                    .synchronize_group_from_with_window_selection_and_winlink_alert_maps(
                        &source,
                        &BTreeMap::from([(4, dest)]),
                        &map,
                    );
                let result = new
                    .break_pane_for_runtime_transfer(
                        &target("a", 4),
                        0,
                        &name("a"),
                        BreakPaneOptions::new(Some(anchor), None, detached, false, before),
                    )
                    .unwrap();
                assert_eq!(result.window_index, dest);
                assert_eq!(
                    std::ptr::from_ref(new.window(&target("peer", dest)).unwrap()),
                    ptr
                );
                let a = old.session(&name("peer")).unwrap();
                let b = new.session(&name("peer")).unwrap();
                let expected = a
                    .windows()
                    .keys()
                    .map(|i| (*i, a.winlink_alert_flags(*i)))
                    .collect::<Vec<_>>();
                let actual = b
                    .windows()
                    .map(|(i, _)| (i, b.winlink_alert_flags(i)))
                    .collect::<Vec<_>>();
                assert_eq!(a.winlink_alert_flags(dest), WINLINK_BELL);
                for i in [if before { 1 } else { 0 }, if before { 10 } else { 9 }] {
                    assert_eq!(b.winlink_alert_flags(i), a.winlink_alert_flags(i));
                }
                if actual != expected {
                    failures.push(format!("duplicate={duplicate} before={before} detached={detached}: moved peer:{dest}: expected={:?}, actual={:?}",a.winlink_alert_flags(dest),b.winlink_alert_flags(dest)));
                }
            }
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    assert!(
        failures.is_empty(),
        "{} relocated peer alert failures",
        failures.len()
    );
}
