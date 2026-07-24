use super::*;

use rmux_core::PaneId;
use rmux_proto::{PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage, SessionName};

use crate::pane_io::PaneBoundary;

fn pane() -> PaneOutputSubscriptionKey {
    PaneOutputSubscriptionKey::new(
        SessionName::new("raw-init").expect("valid session name"),
        PaneId::new(7),
    )
}

fn cached_raw_rebase() -> Arc<CachedRawRebase> {
    Arc::new(CachedRawRebase {
        boundary: PaneBoundary {
            generation: 1,
            next_output_sequence: 2,
            invalidation_revision: 3,
            process_exit_revision: 4,
        },
        rebase: PaneRawRebase {
            epoch: 1,
            generation: 1,
            invalidation_revision: 3,
            next_sequence: 2,
            cols: 80,
            rows: 24,
            keyframe: Vec::new(),
            alternate: false,
            coverage: PaneRecoveryCoverage {
                history_rows_total: 0,
                history_rows_included: 0,
                metadata_complete: true,
            },
            snapshot: None,
            reason: PaneRawRebaseReason::Initial,
        },
    })
}

#[tokio::test]
async fn one_raw_initializer_wakes_waiters_before_re_election() {
    let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
    let pane = pane();
    let RawInitializationRoute::Initialize { token } = state.raw_initialization_route(&pane, false)
    else {
        panic!("first caller must initialize");
    };
    let RawInitializationRoute::Wait(mut waiter) = state.raw_initialization_route(&pane, false)
    else {
        panic!("second caller must wait");
    };

    state.finish_raw_initialization(token);
    waiter.changed().await.expect("initializer completion");
    assert!(*waiter.borrow());
    assert!(matches!(
        state.raw_initialization_route(&pane, false),
        RawInitializationRoute::Initialize { .. }
    ));
}

#[test]
fn raw_initialization_reuses_only_a_sufficient_cached_projection() {
    let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
    let pane = pane();
    let cached = cached_raw_rebase();
    state.raw_rebases.insert(pane.clone(), Arc::clone(&cached));

    let RawInitializationRoute::Ready(found) = state.raw_initialization_route(&pane, false) else {
        panic!("keyframe-only caller should reuse the cached projection");
    };
    assert!(Arc::ptr_eq(&found, &cached));
    assert!(matches!(
        state.raw_initialization_route(&pane, true),
        RawInitializationRoute::Initialize { .. }
    ));
}

#[tokio::test]
async fn pane_removal_wakes_raw_initialization_waiters() {
    let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
    let pane = pane();
    assert!(matches!(
        state.raw_initialization_route(&pane, false),
        RawInitializationRoute::Initialize { .. }
    ));
    let RawInitializationRoute::Wait(mut waiter) = state.raw_initialization_route(&pane, false)
    else {
        panic!("second caller must wait");
    };

    state.remove_pane(&pane);
    waiter.changed().await.expect("pane-removal completion");
    assert!(*waiter.borrow());
}

#[tokio::test]
async fn raw_initialization_token_survives_a_pane_rekey() {
    let mut state = OutputSubscriptionState::new(SubscriptionLimits::default());
    let previous = pane();
    let current = PaneOutputSubscriptionKey::new(
        SessionName::new("raw-moved").expect("valid session name"),
        previous.pane_id(),
    );
    let RawInitializationRoute::Initialize { token } =
        state.raw_initialization_route(&previous, false)
    else {
        panic!("first caller must initialize");
    };
    let RawInitializationRoute::Wait(mut waiter) = state.raw_initialization_route(&previous, false)
    else {
        panic!("second caller must wait");
    };

    state.rekey_pane(&previous, current.clone());
    state.finish_raw_initialization(token);

    waiter
        .changed()
        .await
        .expect("rekeyed initializer completion");
    assert!(*waiter.borrow());
    assert!(!state.raw_initializations.contains_key(&previous));
    assert!(!state.raw_initializations.contains_key(&current));
}
