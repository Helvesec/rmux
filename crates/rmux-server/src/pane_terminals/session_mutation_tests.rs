#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use rmux_proto::{PaneTarget, SessionName, TerminalSize, WindowTarget};

use super::{HandlerState, PaneTransferGeometryContext};

#[test]
fn join_or_move_geometry_uses_requested_alias_not_hashmap_first() {
    let alpha = session_name("geometry-alias-alpha");
    let beta = session_name("geometry-alias-beta");
    let source = session_name("geometry-alias-source");
    let original_size = TerminalSize { cols: 80, rows: 24 };
    let resized = TerminalSize { cols: 79, rows: 24 };
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(alpha.clone(), original_size)
        .expect("create alpha");
    state
        .sessions
        .create_grouped_session_with_base_index(beta.clone(), original_size, 0, alpha.clone())
        .expect("create beta alias");
    state
        .sessions
        .create_session(source.clone(), original_size)
        .expect("create source");

    let shared_window_id = state
        .sessions
        .session(&alpha)
        .expect("alpha")
        .window_at(0)
        .expect("alpha window")
        .id();
    let hashmap_first_alias = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find_map(|(session_name, session)| {
            session
                .window_at(0)
                .is_some_and(|window| window.id() == shared_window_id)
                .then(|| session_name.clone())
        })
        .expect("shared alias");
    let requested_alias = if hashmap_first_alias == alpha {
        beta.clone()
    } else {
        alpha.clone()
    };
    let source_pane = PaneTarget::with_window(source, 0, 0);
    let requested_pane = PaneTarget::with_window(requested_alias.clone(), 0, 0);

    let context = PaneTransferGeometryContext::new(&source_pane, &requested_pane);
    state.mutate_join_or_move_and_record_window_geometry_changes(context, |state| {
        for session_name in [&alpha, &beta] {
            state
                .sessions
                .session_setup(session_name)
                .expect("shared session")
                .resize_window(0, resized)
                .expect("resize shared window");
        }
    });

    let targets = state
        .take_applied_window_resizes()
        .into_iter()
        .map(|resize| resize.into_parts().0)
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        vec![WindowTarget::with_window(requested_alias, 0)],
        "a join/move notification must retain the alias explicitly named by the operation"
    );
}

#[test]
fn join_or_move_geometry_orders_source_target_then_collateral() {
    let alpha = session_name("geometry-order-alpha");
    let beta = session_name("geometry-order-beta");
    let source = session_name("geometry-order-source");
    let original_size = TerminalSize { cols: 80, rows: 24 };
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(alpha.clone(), original_size)
        .expect("create alpha");
    state
        .sessions
        .session_setup(&alpha)
        .expect("alpha")
        .create_window(original_size)
        .expect("create collateral window");
    state
        .sessions
        .create_grouped_session_with_base_index(beta.clone(), original_size, 0, alpha.clone())
        .expect("create beta aliases");
    state
        .sessions
        .create_session(source.clone(), original_size)
        .expect("create source");

    let source_pane = PaneTarget::with_window(source.clone(), 0, 0);
    let target_pane = PaneTarget::with_window(beta.clone(), 0, 0);
    let context = PaneTransferGeometryContext::new(&source_pane, &target_pane);
    state.mutate_join_or_move_and_record_window_geometry_changes(context, |state| {
        state
            .sessions
            .session_setup(&source)
            .expect("source")
            .resize_window(0, TerminalSize { cols: 79, rows: 24 })
            .expect("resize source");
        for session_name in [&alpha, &beta] {
            let mut session = state
                .sessions
                .session_setup(session_name)
                .expect("shared session");
            session
                .resize_window(0, TerminalSize { cols: 78, rows: 24 })
                .expect("resize target");
            session
                .resize_window(1, TerminalSize { cols: 77, rows: 24 })
                .expect("resize collateral");
        }
    });

    let targets = state
        .take_applied_window_resizes()
        .into_iter()
        .map(|resize| resize.into_parts().0)
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        vec![
            WindowTarget::with_window(source, 0),
            WindowTarget::with_window(beta.clone(), 0),
            WindowTarget::with_window(beta, 1),
        ],
        "publication order and collateral rendering context must be operation-derived"
    );
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("test session name")
}
