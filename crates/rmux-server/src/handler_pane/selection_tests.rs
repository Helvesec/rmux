use super::*;
use crate::handler::stable_target_identity::{
    with_expected_stable_target_identities, StableTargetIdentity,
};
use crate::pane_terminals::HandlerState;
use rmux_proto::*;

fn name(s: &str) -> SessionName {
    SessionName::new(s).unwrap()
}
fn target(pane: u32) -> PaneTarget {
    PaneTarget::with_window(name("B"), 7, pane)
}
fn request(pane: u32, preserve_zoom: bool) -> SelectPaneRequest {
    SelectPaneRequest {
        target: target(pane),
        preserve_zoom,
        title: None,
        input_disabled: None,
        style: None,
    }
}
async fn fixture(zoom: bool) -> RequestHandler {
    let h = RequestHandler::new();
    *h.state.lock().await = HandlerState::window_selection_fixture(zoom);
    h
}
fn success(response: Response, pane: u32) {
    match response {
        Response::SelectPane(s) => assert_eq!(s.target, target(pane)),
        r => panic!("{r:?}"),
    }
}
fn failure(response: Response, error: RmuxError) {
    match response {
        Response::Error(e) => assert_eq!(e.error, error),
        r => panic!("{r:?}"),
    }
}
fn no_terminal() -> RmuxError {
    RmuxError::Server("failed to roll back window runtime for B:7 after server error: missing pane terminals for session A: server error: missing pane terminals for session A".into())
}
fn queue(state: &mut HandlerState) {
    let pending = state.take_applied_window_resizes();
    assert_eq!(pending.len(), 1);
    let (target, prepared, _) = pending.into_iter().next().unwrap().into_parts();
    assert_eq!(
        (target, prepared),
        (WindowTarget::with_window(name("B"), 0), true)
    );
}

#[tokio::test]
async fn reselect_aliases_without_terminal() {
    for zoom in [false, true] {
        for preserve in [false, true] {
            let h = fixture(zoom).await;
            let before_b = h
                .state
                .lock()
                .await
                .sessions
                .session(&name("B"))
                .unwrap()
                .test_snapshot();
            success(h.handle_select_pane(request(1, preserve)).await, 1);
            let mut state = h.state.lock().await;
            let a = state.sessions.session(&name("A")).unwrap();
            let b = state.sessions.session(&name("B")).unwrap();
            let c = state.sessions.session(&name("C")).unwrap();
            assert_eq!(a.window(), b.window_at(7).unwrap());
            assert_eq!(a.window(), c.window());
            assert!(
                std::ptr::eq(a.window(), b.window_at(7).unwrap()),
                "aliases share canonical window identity"
            );
            assert_eq!(b.window_at(7).unwrap().active_pane_index(), 1);
            assert_eq!(b.window_at(7).unwrap().last_pane_index(), Some(0));
            assert_eq!(b.window_at(7).unwrap().is_zoomed(), zoom);
            assert_eq!(
                b.window_at(7)
                    .unwrap()
                    .panes()
                    .iter()
                    .map(|p| (p.id(), p.geometry()))
                    .collect::<Vec<_>>(),
                before_b
                    .window_at(7)
                    .unwrap()
                    .panes()
                    .iter()
                    .map(|p| (p.id(), p.geometry()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(b.window_at(0), before_b.window_at(0));
            assert_eq!(b.active_window_index(), 0);
            assert_eq!(b.last_window_index(), before_b.last_window_index());
            assert_eq!(a.winlink_alert_flags(0), rmux_core::WINDOW_BELL);
            assert_eq!(b.winlink_alert_flags(7), rmux_core::WINDOW_ACTIVITY);
            assert_eq!(c.winlink_alert_flags(0), rmux_core::WINDOW_SILENCE);
            for session in ["A", "B", "C"] {
                assert_eq!(
                    state
                        .options
                        .show_options_lines_filtered(
                            &OptionScopeSelector::Session(name(session)),
                            Some("@local"),
                            true
                        )
                        .unwrap(),
                    vec![session]
                );
            }
            assert_eq!(state.window_runtime_resize_count_for_test(), 0);
            let _ = state.window_selection_snapshot();
            queue(&mut state);
        }
    }
}

#[tokio::test]
async fn changed_selection_restores_snapshot_and_old_queue() {
    for preserve in [false, true] {
        let h = fixture(true).await;
        let before = h.state.lock().await.window_selection_snapshot();
        failure(
            h.handle_select_pane(request(0, preserve)).await,
            no_terminal(),
        );
        let mut state = h.state.lock().await;
        assert_eq!(state.window_runtime_resize_count_for_test(), 2);
        assert_eq!(state.window_selection_snapshot(), before);
        queue(&mut state);
    }
}

#[tokio::test]
async fn invalid_target_and_style_ordering() {
    for (t, e) in [
        (
            PaneTarget::with_window(name("Z"), 9, 99),
            RmuxError::SessionNotFound("Z".into()),
        ),
        (
            PaneTarget::with_window(name("B"), 9, 99),
            RmuxError::invalid_target("B:9", "window index does not exist in session"),
        ),
        (
            target(99),
            RmuxError::invalid_target("B:7.99", "pane index does not exist in session"),
        ),
    ] {
        let h = fixture(false).await;
        let before = h.state.lock().await.window_selection_snapshot();
        let mut r = request(1, false);
        r.target = t;
        failure(h.handle_select_pane(r.clone()).await, e);
        r.style = Some("fg=not-a-colour".into());
        failure(
            h.handle_select_pane(r).await,
            RmuxError::InvalidSetOption("invalid style: fg=not-a-colour".into()),
        );
        let state = h.state.lock().await;
        assert_eq!(state.window_runtime_resize_count_for_test(), 0);
        assert_eq!(state.window_selection_snapshot(), before);
    }
}

#[tokio::test]
async fn stale_identity_precedes_style_validation() {
    let h = fixture(false).await;
    let identity = {
        let mut state = h.state.lock().await;
        let identity = StableTargetIdentity::capture(&mut state, Target::Pane(target(1))).unwrap();
        let target = WindowTarget::with_window(name("B"), 7);
        let retained = state
            .sessions
            .window(&target)
            .unwrap()
            .pane(0)
            .unwrap()
            .id();
        state.sessions.respawn_window(&target, retained).unwrap();
        identity
    };
    let before = h.state.lock().await.window_selection_snapshot();
    let mut r = request(1, false);
    r.style = Some("fg=not-a-colour".into());
    let response =
        with_expected_stable_target_identities(vec![identity], None, None, h.handle_select_pane(r))
            .await;
    failure(
        response,
        RmuxError::invalid_target(
            "B:7.1",
            "target identity changed before queued mutation: queued target was replaced",
        ),
    );
    let state = h.state.lock().await;
    assert_eq!(state.window_selection_snapshot(), before);
    assert_eq!(state.window_runtime_resize_count_for_test(), 0);
}

#[tokio::test]
async fn title_input_and_style_branches() {
    let h = fixture(false).await;
    let before = h.state.lock().await.window_selection_snapshot();
    let mut r = request(0, false);
    r.title = Some("new title".into());
    r.input_disabled = Some(true);
    failure(
        h.handle_select_pane(r).await,
        RmuxError::Server("missing pane terminal for B:7.0".into()),
    );
    assert_eq!(h.state.lock().await.window_selection_snapshot(), before);
    let mut r = request(0, false);
    r.input_disabled = Some(true);
    success(h.handle_select_pane(r).await, 0);
    {
        let state = h.state.lock().await;
        assert!(state.pane_input_is_disabled(PaneId::new(99)));
        assert!(!state.pane_input_is_disabled(PaneId::new(0)));
        assert_eq!(
            state
                .sessions
                .session(&name("B"))
                .unwrap()
                .window_at(7)
                .unwrap()
                .active_pane_index(),
            1
        );
    }
    let mut r = request(1, false);
    r.style = Some("fg=blue".into());
    success(h.handle_select_pane(r).await, 1);
    let state = h.state.lock().await;
    for (s, w) in [("A", 0), ("B", 7), ("C", 0)] {
        let pane = PaneTarget::with_window(name(s), w, 1);
        let options = state
            .options
            .show_options_lines_filtered(
                &OptionScopeSelector::Pane(pane),
                Some("window-style"),
                true,
            )
            .unwrap();
        assert!(format!("{options:?}").contains("fg=blue"));
    }
    assert_eq!(state.window_runtime_resize_count_for_test(), 0);
}

#[tokio::test]
async fn by_id_selection_preserves_reselect_and_restores_changed_selection() {
    for zoom in [false, true] {
        let h = fixture(zoom).await;
        let before_b = h
            .state
            .lock()
            .await
            .sessions
            .session(&name("B"))
            .unwrap()
            .test_snapshot();
        success(
            h.handle_pane_select_ref(PaneSelectRequest {
                target: PaneTargetRef::by_id(name("B"), PaneId::new(0)),
                title: None,
            })
            .await,
            1,
        );
        let mut state = h.state.lock().await;
        let a = state.sessions.session(&name("A")).unwrap();
        let b = state.sessions.session(&name("B")).unwrap();
        let c = state.sessions.session(&name("C")).unwrap();
        assert_eq!(a.window(), b.window_at(7).unwrap());
        assert_eq!(a.window(), c.window());
        assert!(
            std::ptr::eq(a.window(), b.window_at(7).unwrap()),
            "aliases share canonical window identity"
        );
        assert_eq!(b.window_at(7).unwrap().active_pane_index(), 1);
        assert_eq!(b.window_at(7).unwrap().last_pane_index(), Some(0));
        assert_eq!(b.window_at(7).unwrap().is_zoomed(), zoom);
        assert_eq!(b.window_at(0), before_b.window_at(0));
        assert_eq!(b.active_window_index(), 0);
        assert_eq!(state.window_runtime_resize_count_for_test(), 0);
        queue(&mut state);
    }

    let h = fixture(true).await;
    let before = h.state.lock().await.window_selection_snapshot();
    failure(
        h.handle_pane_select_ref(PaneSelectRequest {
            target: PaneTargetRef::by_id(name("B"), PaneId::new(404)),
            title: None,
        })
        .await,
        RmuxError::pane_not_found(name("B"), PaneId::new(404)),
    );
    assert_eq!(h.state.lock().await.window_selection_snapshot(), before);
    failure(
        h.handle_pane_select_ref(PaneSelectRequest {
            target: PaneTargetRef::by_id(name("B"), PaneId::new(0)),
            title: Some("renamed".into()),
        })
        .await,
        RmuxError::Server("missing pane terminal for B:7.1".into()),
    );
    {
        let state = h.state.lock().await;
        assert_eq!(state.window_runtime_resize_count_for_test(), 0);
        assert_eq!(state.window_selection_snapshot(), before);
    }
    failure(
        h.handle_pane_select_ref(PaneSelectRequest {
            target: PaneTargetRef::by_id(name("B"), PaneId::new(99)),
            title: None,
        })
        .await,
        no_terminal(),
    );
    let mut state = h.state.lock().await;
    assert_eq!(state.window_runtime_resize_count_for_test(), 2);
    assert_eq!(state.window_selection_snapshot(), before);
    queue(&mut state);
}

#[tokio::test]
async fn last_pane_preserves_error_order_and_toggles_input_without_selecting() {
    for (target, error) in [
        (
            WindowTarget::with_window(name("Z"), 9),
            RmuxError::SessionNotFound("Z".into()),
        ),
        (
            WindowTarget::with_window(name("B"), 9),
            RmuxError::invalid_target("B:9", "window index does not exist in session"),
        ),
        (
            WindowTarget::with_window(name("B"), 0),
            RmuxError::Server("no last pane".into()),
        ),
    ] {
        let h = fixture(false).await;
        let before = h.state.lock().await.window_selection_snapshot();
        failure(
            h.handle_last_pane(LastPaneRequest {
                target,
                preserve_zoom: false,
                input_disabled: Some(true),
            })
            .await,
            error,
        );
        let mut state = h.state.lock().await;
        assert_eq!(state.window_runtime_resize_count_for_test(), 0);
        assert_eq!(state.window_selection_snapshot(), before);
        assert!(!state.pane_input_is_disabled(PaneId::new(99)));
        queue(&mut state);
    }

    // tmux 3.7b: -d/-e toggles the last pane's input and selects nothing, so
    // the window keeps its active pane and zoom and needs no live terminal.
    for zoom in [false, true] {
        for (initially_disabled, requested_disabled) in [(false, true), (true, false)] {
            let h = fixture(zoom).await;
            {
                let mut state = h.state.lock().await;
                state
                    .set_pane_input_disabled(&target(0), initially_disabled)
                    .unwrap();
            }
            let before = h.state.lock().await.window_selection_snapshot();
            match h
                .handle_last_pane(LastPaneRequest {
                    target: WindowTarget::with_window(name("B"), 7),
                    preserve_zoom: false,
                    input_disabled: Some(requested_disabled),
                })
                .await
            {
                Response::LastPane(response) => assert_eq!(response.target, target(0)),
                response => panic!("{response:?}"),
            }
            let mut state = h.state.lock().await;
            assert_eq!(state.window_runtime_resize_count_for_test(), 0);
            assert_eq!(state.window_selection_snapshot(), before);
            assert_eq!(
                state.pane_input_is_disabled(PaneId::new(99)),
                requested_disabled
            );
            queue(&mut state);
        }
    }
}

#[tokio::test]
async fn adjacent_selection_uses_anchor_and_resizes_only_when_active_changes() {
    let h = fixture(true).await;
    let before_b = h
        .state
        .lock()
        .await
        .sessions
        .session(&name("B"))
        .unwrap()
        .test_snapshot();
    success(
        h.handle_select_pane_adjacent(SelectPaneAdjacentRequest {
            target: target(0),
            direction: SelectPaneDirection::Right,
            preserve_zoom: false,
        })
        .await,
        1,
    );
    {
        let state = h.state.lock().await;
        let a = state.sessions.session(&name("A")).unwrap();
        let b = state.sessions.session(&name("B")).unwrap();
        let c = state.sessions.session(&name("C")).unwrap();
        assert_eq!(a.window(), b.window_at(7).unwrap());
        assert_eq!(a.window(), c.window());
        assert_eq!(b.window_at(7).unwrap().active_pane_index(), 1);
        assert_eq!(b.window_at(7).unwrap().last_pane_index(), Some(0));
        assert!(b.window_at(7).unwrap().is_zoomed());
        assert_eq!(b.window_at(0), before_b.window_at(0));
        assert_eq!(state.window_runtime_resize_count_for_test(), 0);
    }
    let before = h.state.lock().await.window_selection_snapshot();
    failure(
        h.handle_select_pane_adjacent(SelectPaneAdjacentRequest {
            target: target(99),
            direction: SelectPaneDirection::Left,
            preserve_zoom: false,
        })
        .await,
        RmuxError::invalid_target("B:7.99", "pane index does not exist in session"),
    );
    assert_eq!(h.state.lock().await.window_selection_snapshot(), before);
    failure(
        h.handle_select_pane_adjacent(SelectPaneAdjacentRequest {
            target: target(1),
            direction: SelectPaneDirection::Left,
            preserve_zoom: false,
        })
        .await,
        no_terminal(),
    );
    let mut state = h.state.lock().await;
    assert_eq!(state.window_runtime_resize_count_for_test(), 2);
    assert_eq!(state.window_selection_snapshot(), before);
    queue(&mut state);
}
