//! Dedicated ROPT1 regressions for pane option ownership across `rotate-window`.
//!
//! Pane overrides are keyed by `(session, window index, pane index)`, while a
//! rotation reorders the pane vector and reassigns 0..n by position, keeping
//! every `PaneId` and every live child. Ownership therefore has to be carried
//! explicitly: without the rekey on the success path of
//! `pane_terminals::window_movement::rotate_window`, an override written for
//! one pane is read by whichever pane rotated into its old slot, and a pane
//! that had no override at all inherits a neighbour's one.
//!
//! The discriminator throughout is IDENTITY, never position. Every read targets
//! a pane's *current* slot and asserts the `PaneId` the daemon resolved for it
//! alongside the value, so a test cannot pass by the panes failing to move; the
//! rotation of the `PaneId` vector itself is asserted separately. Both halves
//! of the contract are covered: presence follows the pane, and so does absence.

use super::pane_group_transfer_tests::{create_session, pane_ids};
use super::RequestHandler;
use rmux_core::PaneId;
use rmux_proto::{
    LinkWindowRequest, OptionName, OptionScopeSelector, PaneOptionGetRequest, PaneOptionSetRequest,
    PaneTarget, PaneTargetRef, Request, Response, RotateWindowDirection, RotateWindowRequest,
    ScopeSelector, SessionName, SetOptionMode, SetOptionRequest, ShowOptionsRequest,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, WindowTarget,
};

/// The user option the identity witnesses carry.
const TOKEN: &str = "@ropt-token";
/// A real table option that is settable at pane scope, so the fix is shown not
/// to be limited to user options.
const KNOWN: &str = "remain-on-exit";

// ---------------------------------------------------------------------------
// Fixture construction
// ---------------------------------------------------------------------------

/// Pins the quiet stdin-discard default command so a spawned child never writes
/// anything of its own while the fixtures hold panes open.
async fn pin_quiet_default_command(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: crate::test_shell::stdin_discard_command(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// Creates a detached fixture session whose first pane has finished starting.
async fn ropt_session(handler: &RequestHandler, name: &str) -> SessionName {
    pin_quiet_default_command(handler).await;
    let session_name = create_session(handler, name).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            0,
            0,
        ))
        .await;
    session_name
}

/// Splits one explicit pane and returns the settled new pane.
async fn ropt_split(handler: &RequestHandler, target: &PaneTarget) -> PaneTarget {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(target.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    let pane = match response {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&pane)
        .await;
    pane
}

/// Builds a settled three-pane window at indices 0/1/2 and returns its slots.
async fn three_pane_window(handler: &RequestHandler, name: &str) -> (SessionName, [PaneTarget; 3]) {
    let session_name = ropt_session(handler, name).await;
    let first = PaneTarget::with_window(session_name.clone(), 0, 0);
    let second = ropt_split(handler, &first).await;
    let third = ropt_split(handler, &second).await;
    assert_eq!(second.pane_index(), 1, "split lands after its source");
    assert_eq!(third.pane_index(), 2, "split lands after its source");
    (session_name, [first, second, third])
}

// ---------------------------------------------------------------------------
// Option plumbing
// ---------------------------------------------------------------------------

async fn set_pane_option(handler: &RequestHandler, target: &PaneTarget, name: &str, value: &str) {
    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: name.to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "set -p {name} on {target}: {response:?}"
    );
}

async fn unset_pane_option(handler: &RequestHandler, target: &PaneTarget, name: &str) {
    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: name.to_owned(),
            value: None,
            mode: SetOptionMode::Replace,
            unset: true,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "set -pu {name} on {target}: {response:?}"
    );
}

async fn set_window_option(
    handler: &RequestHandler,
    target: &WindowTarget,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(target.clone()),
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// The daemon's own pane-scoped lookup: it resolves the slot to the `PaneId`
/// that currently occupies it and returns that pane's explicit value.
async fn owner_of_slot(
    handler: &RequestHandler,
    target: &PaneTarget,
    name: &str,
) -> (PaneId, Option<String>) {
    let response = handler
        .handle(Request::PaneOptionGet(PaneOptionGetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: name.to_owned(),
        }))
        .await;
    match response {
        Response::PaneOptionGet(response) => (response.pane_id, response.value),
        response => panic!("expected pane-option-get success for {target}, got {response:?}"),
    }
}

/// The explicit value stored for exactly this slot, with no alias fallback.
///
/// `pane_explicit_option_value_by_name` deliberately consults the other aliases
/// of a linked window; the absence half of the contract has to be checked
/// without that fallback, one alias at a time.
async fn direct_explicit(
    handler: &RequestHandler,
    target: &PaneTarget,
    name: &str,
) -> Option<String> {
    let state = handler.state.lock().await;
    let (_, value) = state
        .options
        .explicit_value_by_name(&OptionScopeSelector::Pane(target.clone()), name)
        .expect("option name resolves");
    value
}

/// `show-options -p -A <name>` on one slot: an explicit value renders bare, an
/// inherited one carries the `*` marker.
async fn shown_pane_option(handler: &RequestHandler, target: &PaneTarget, name: &str) -> String {
    let response = handler
        .handle(Request::ShowOptions(ShowOptionsRequest {
            scope: OptionScopeSelector::Pane(target.clone()),
            name: Some(name.to_owned()),
            value_only: false,
            include_inherited: true,
            quiet: false,
            include_hooks: false,
        }))
        .await;
    let output = response
        .command_output()
        .expect("show-options returns command output");
    String::from_utf8(output.stdout().to_vec()).expect("show-options output is utf8")
}

// ---------------------------------------------------------------------------
// Identity plumbing
// ---------------------------------------------------------------------------

async fn window_pane_ids(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
) -> Vec<PaneId> {
    let state = handler.state.lock().await;
    pane_ids(&state, session_name, window_index)
}

/// Asserts that the pane vector is `original` rotated left by `shift`, and -
/// when the rotation is a real one - that it actually moved.
fn assert_rotated_left(observed: &[PaneId], original: &[PaneId], shift: usize, label: &str) {
    let effective = shift % original.len().max(1);
    let mut expected = original.to_vec();
    expected.rotate_left(effective);
    assert_eq!(
        observed, expected,
        "{label}: the pane identities must rotate by {shift}"
    );
    if effective != 0 {
        assert_ne!(
            observed, original,
            "{label}: the panes must really have moved, or nothing is under test"
        );
    }
}

async fn rotate(
    handler: &RequestHandler,
    target: WindowTarget,
    direction: RotateWindowDirection,
) -> Response {
    handler
        .handle(Request::RotateWindow(RotateWindowRequest {
            target,
            direction,
            restore_zoom: false,
        }))
        .await
}

async fn rotate_ok(
    handler: &RequestHandler,
    target: WindowTarget,
    direction: RotateWindowDirection,
) {
    let response = rotate(handler, target, direction).await;
    assert!(
        matches!(response, Response::RotateWindow(_)),
        "{direction:?}: {response:?}"
    );
}

/// Asserts that the pane now sitting at `slot` is `expected_id` and that it
/// still owns `expected_value`.
async fn assert_slot_owner(
    handler: &RequestHandler,
    slot: &PaneTarget,
    name: &str,
    expected_id: PaneId,
    expected_value: Option<&str>,
    label: &str,
) {
    let (pane_id, value) = owner_of_slot(handler, slot, name).await;
    assert_eq!(
        pane_id, expected_id,
        "{label}: {slot} must be occupied by the expected pane identity"
    );
    assert_eq!(
        value.as_deref(),
        expected_value,
        "{label}: {name} read at {slot} must belong to the pane now in it"
    );
}

// ---------------------------------------------------------------------------
// 1. A user override follows its pane, in both directions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotate_window_carries_user_pane_overrides_with_each_pane_in_both_directions() {
    let handler = RequestHandler::new();
    let (session_name, slots) = three_pane_window(&handler, "ropt-user-both").await;
    let window = WindowTarget::with_window(session_name.clone(), 0);

    for (slot, token) in slots.iter().zip(["A", "B", "C"]) {
        set_pane_option(&handler, slot, TOKEN, token).await;
    }
    let original = window_pane_ids(&handler, &session_name, 0).await;
    assert_eq!(original.len(), 3, "the fixture holds three panes");
    for (index, (slot, token)) in slots.iter().zip(["A", "B", "C"]).enumerate() {
        assert_slot_owner(
            &handler,
            slot,
            TOKEN,
            original[index],
            Some(token),
            "before any rotation",
        )
        .await;
    }

    // `-U` is `rotate_left(1)`: [A, B, C] becomes [B, C, A].
    rotate_ok(&handler, window.clone(), RotateWindowDirection::Up).await;
    let after_up = window_pane_ids(&handler, &session_name, 0).await;
    assert_rotated_left(&after_up, &original, 1, "after -U");
    for (index, token) in ["B", "C", "A"].iter().enumerate() {
        assert_slot_owner(
            &handler,
            &slots[index],
            TOKEN,
            after_up[index],
            Some(token),
            "after -U",
        )
        .await;
    }

    // `-D` is `rotate_right(1)`, so the window returns to its original order.
    rotate_ok(&handler, window, RotateWindowDirection::Down).await;
    let after_down = window_pane_ids(&handler, &session_name, 0).await;
    assert_eq!(
        after_down, original,
        "-D after -U restores the original pane order"
    );
    for (index, token) in ["A", "B", "C"].iter().enumerate() {
        assert_slot_owner(
            &handler,
            &slots[index],
            TOKEN,
            original[index],
            Some(token),
            "after -D",
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// 2. A known table option follows its pane too
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotate_window_carries_known_pane_overrides_with_each_pane() {
    let handler = RequestHandler::new();
    let (session_name, slots) = three_pane_window(&handler, "ropt-known").await;
    let window = WindowTarget::with_window(session_name.clone(), 0);

    let values = ["on", "failed", "off"];
    for (slot, value) in slots.iter().zip(values) {
        set_pane_option(&handler, slot, KNOWN, value).await;
    }
    let original = window_pane_ids(&handler, &session_name, 0).await;

    rotate_ok(&handler, window.clone(), RotateWindowDirection::Up).await;
    let after_up = window_pane_ids(&handler, &session_name, 0).await;
    assert_rotated_left(&after_up, &original, 1, "after -U");
    for (index, value) in ["failed", "off", "on"].iter().enumerate() {
        assert_slot_owner(
            &handler,
            &slots[index],
            KNOWN,
            after_up[index],
            Some(value),
            "after -U",
        )
        .await;
        assert_eq!(
            shown_pane_option(&handler, &slots[index], KNOWN).await,
            format!("{KNOWN} {value}\n"),
            "after -U: show-options -p must render the pane's own explicit value"
        );
    }

    rotate_ok(&handler, window, RotateWindowDirection::Down).await;
    let after_down = window_pane_ids(&handler, &session_name, 0).await;
    assert_eq!(after_down, original, "-D restores the original pane order");
    for (index, value) in values.iter().enumerate() {
        assert_slot_owner(
            &handler,
            &slots[index],
            KNOWN,
            original[index],
            Some(value),
            "after -D",
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// 3. Absence follows the pane: an inheriting pane keeps inheriting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotate_window_keeps_an_overrideless_pane_inheriting_its_window() {
    let handler = RequestHandler::new();
    let (session_name, slots) = three_pane_window(&handler, "ropt-absence").await;
    let window = WindowTarget::with_window(session_name.clone(), 0);

    // The window value is what the bare pane must keep resolving to; the two
    // neighbours carry explicit values that differ from it and from each other.
    set_window_option(&handler, &window, OptionName::RemainOnExit, "on").await;
    set_pane_option(&handler, &slots[0], KNOWN, "off").await;
    set_pane_option(&handler, &slots[2], KNOWN, "failed").await;
    let original = window_pane_ids(&handler, &session_name, 0).await;
    let bare = original[1];

    assert_eq!(direct_explicit(&handler, &slots[1], KNOWN).await, None);
    assert_eq!(
        shown_pane_option(&handler, &slots[1], KNOWN).await,
        format!("{KNOWN}* on\n"),
        "the middle pane inherits its window before the rotation"
    );

    rotate_ok(&handler, window.clone(), RotateWindowDirection::Up).await;
    let after_up = window_pane_ids(&handler, &session_name, 0).await;
    assert_rotated_left(&after_up, &original, 1, "after -U");
    // The bare pane rotated into index 0, which held an explicit "off".
    assert_eq!(after_up[0], bare, "the bare pane now occupies index 0");
    assert_slot_owner(&handler, &slots[0], KNOWN, bare, None, "after -U").await;
    assert_eq!(
        direct_explicit(&handler, &slots[0], KNOWN).await,
        None,
        "after -U: the bare pane must have no explicit pane-local override"
    );
    assert_eq!(
        shown_pane_option(&handler, &slots[0], KNOWN).await,
        format!("{KNOWN}* on\n"),
        "after -U: the bare pane must still resolve by inheritance, not take a neighbour's value"
    );
    assert_slot_owner(
        &handler,
        &slots[1],
        KNOWN,
        after_up[1],
        Some("failed"),
        "after -U",
    )
    .await;
    assert_slot_owner(
        &handler,
        &slots[2],
        KNOWN,
        after_up[2],
        Some("off"),
        "after -U",
    )
    .await;

    rotate_ok(&handler, window, RotateWindowDirection::Down).await;
    let after_down = window_pane_ids(&handler, &session_name, 0).await;
    assert_eq!(after_down, original, "-D restores the original pane order");
    assert_eq!(
        direct_explicit(&handler, &slots[1], KNOWN).await,
        None,
        "after -D: the bare pane is back at index 1 and still has no override"
    );
    assert_eq!(
        shown_pane_option(&handler, &slots[1], KNOWN).await,
        format!("{KNOWN}* on\n"),
        "after -D: the bare pane still inherits"
    );
    assert_slot_owner(
        &handler,
        &slots[0],
        KNOWN,
        original[0],
        Some("off"),
        "after -D",
    )
    .await;
    assert_slot_owner(
        &handler,
        &slots[2],
        KNOWN,
        original[2],
        Some("failed"),
        "after -D",
    )
    .await;
}

// ---------------------------------------------------------------------------
// 4. A linked window rotated through its non-canonical alias
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotate_window_through_a_non_canonical_alias_keeps_both_slots_coherent() {
    let handler = RequestHandler::new();
    let (alpha, slots) = three_pane_window(&handler, "ropt-alias-alpha").await;
    let beta = create_session(&handler, "ropt-alias-beta").await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 0),
            target: WindowTarget::with_window(beta.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    let alias: [PaneTarget; 3] = [
        PaneTarget::with_window(beta.clone(), 1, 0),
        PaneTarget::with_window(beta.clone(), 1, 1),
        PaneTarget::with_window(beta.clone(), 1, 2),
    ];

    for (slot, token) in slots.iter().zip(["A", "B", "C"]) {
        set_pane_option(&handler, slot, TOKEN, token).await;
    }
    let original = window_pane_ids(&handler, &alpha, 0).await;
    assert_eq!(
        window_pane_ids(&handler, &beta, 1).await,
        original,
        "both slots address the same live panes"
    );

    // The rotation is driven through the alias, not through the window's own
    // session, so the rekey has to reach the canonical slot as well.
    rotate_ok(
        &handler,
        WindowTarget::with_window(beta.clone(), 1),
        RotateWindowDirection::Up,
    )
    .await;
    let after_up = window_pane_ids(&handler, &alpha, 0).await;
    assert_rotated_left(&after_up, &original, 1, "after -U through the alias");
    assert_eq!(
        window_pane_ids(&handler, &beta, 1).await,
        after_up,
        "the aliases stay in step"
    );
    for (index, token) in ["B", "C", "A"].iter().enumerate() {
        assert_slot_owner(
            &handler,
            &slots[index],
            TOKEN,
            after_up[index],
            Some(token),
            "canonical slot after -U",
        )
        .await;
        assert_slot_owner(
            &handler,
            &alias[index],
            TOKEN,
            after_up[index],
            Some(token),
            "alias slot after -U",
        )
        .await;
        assert_eq!(
            direct_explicit(&handler, &slots[index], TOKEN).await,
            direct_explicit(&handler, &alias[index], TOKEN).await,
            "after -U: both aliases must store the same value for index {index}, with no fallback"
        );
    }

    // Hot mutation through the alias, on the pane that rotated into index 0.
    let rotated_in = after_up[0];
    set_pane_option(&handler, &alias[0], TOKEN, "B-hot").await;
    for slot in [&slots[0], &alias[0]] {
        assert_slot_owner(
            &handler,
            slot,
            TOKEN,
            rotated_in,
            Some("B-hot"),
            "after the hot mutation",
        )
        .await;
        assert_eq!(
            direct_explicit(&handler, slot, TOKEN).await,
            Some("B-hot".to_owned()),
            "the hot value is stored under both aliases"
        );
    }

    // And the unset of that same pane, driven through the canonical slot.
    unset_pane_option(&handler, &slots[0], TOKEN).await;
    for slot in [&slots[0], &alias[0]] {
        assert_eq!(
            direct_explicit(&handler, slot, TOKEN).await,
            None,
            "the unset must clear {slot}, not only the alias it was issued on"
        );
        assert_slot_owner(&handler, slot, TOKEN, rotated_in, None, "after the unset").await;
    }
    // The untouched neighbours kept their own values under both aliases.
    for (index, token) in [(1, "C"), (2, "A")] {
        for slot in [&slots[index], &alias[index]] {
            assert_slot_owner(
                &handler,
                slot,
                TOKEN,
                after_up[index],
                Some(token),
                "neighbour after the hot mutation and unset",
            )
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// 5. A mono-pane window
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rotate_window_on_a_mono_pane_window_leaves_options_and_indices_untouched() {
    let handler = RequestHandler::new();
    let session_name = ropt_session(&handler, "ropt-mono").await;
    let window = WindowTarget::with_window(session_name.clone(), 0);
    let only = PaneTarget::with_window(session_name.clone(), 0, 0);

    set_pane_option(&handler, &only, TOKEN, "solo").await;
    set_pane_option(&handler, &only, KNOWN, "failed").await;
    let original = window_pane_ids(&handler, &session_name, 0).await;
    assert_eq!(original.len(), 1, "the fixture holds exactly one pane");

    for direction in [RotateWindowDirection::Up, RotateWindowDirection::Down] {
        rotate_ok(&handler, window.clone(), direction).await;
        assert_eq!(
            window_pane_ids(&handler, &session_name, 0).await,
            original,
            "{direction:?}: rotating one pane cannot change its identity"
        );
        assert_slot_owner(
            &handler,
            &only,
            TOKEN,
            original[0],
            Some("solo"),
            "mono-pane window",
        )
        .await;
        assert_slot_owner(
            &handler,
            &only,
            KNOWN,
            original[0],
            Some("failed"),
            "mono-pane window",
        )
        .await;
        let state = handler.state.lock().await;
        assert!(
            state.pane_profile_in_window(&session_name, 0, 0).is_ok(),
            "{direction:?}: the single child stays live"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. A refused rotation
// ---------------------------------------------------------------------------

/// Every observable this test pins down, captured in one pass.
async fn refusal_snapshot(
    handler: &RequestHandler,
    session_name: &SessionName,
    slots: &[PaneTarget; 3],
) -> (Vec<PaneId>, Vec<(PaneId, Option<String>, Option<String>)>) {
    let ids = window_pane_ids(handler, session_name, 0).await;
    let mut rows = Vec::new();
    for slot in slots {
        let (pane_id, token) = owner_of_slot(handler, slot, TOKEN).await;
        let known = direct_explicit(handler, slot, KNOWN).await;
        rows.push((pane_id, token, known));
    }
    (ids, rows)
}

#[tokio::test]
async fn refused_rotate_window_leaves_pane_options_indices_and_runtime_intact() {
    let handler = RequestHandler::new();
    let (session_name, slots) = three_pane_window(&handler, "ropt-refusal").await;
    let window = WindowTarget::with_window(session_name.clone(), 0);

    set_window_option(&handler, &window, OptionName::RemainOnExit, "on").await;
    for (slot, token) in slots.iter().zip(["A", "B", "C"]) {
        set_pane_option(&handler, slot, TOKEN, token).await;
    }
    set_pane_option(&handler, &slots[0], KNOWN, "off").await;

    let before = refusal_snapshot(&handler, &session_name, &slots).await;

    // The refusals the existing rotation neighbours already produce: an
    // unknown window index, and an unknown session name. The first is refused
    // by the layout transaction, before the option commit; the second never
    // reaches it at all.
    let unknown_window = rotate(
        &handler,
        WindowTarget::with_window(session_name.clone(), 99),
        RotateWindowDirection::Up,
    )
    .await;
    assert!(
        matches!(unknown_window, Response::Error(_)),
        "{unknown_window:?}"
    );
    let missing = SessionName::new("ropt-refusal-absent").expect("valid session name");
    let unknown_session = rotate(
        &handler,
        WindowTarget::with_window(missing, 0),
        RotateWindowDirection::Down,
    )
    .await;
    assert!(
        matches!(unknown_session, Response::Error(_)),
        "{unknown_session:?}"
    );

    let after = refusal_snapshot(&handler, &session_name, &slots).await;
    assert_eq!(
        after, before,
        "a refused rotation must leave pane identities, indices and options untouched"
    );
    assert_eq!(
        shown_pane_option(&handler, &slots[1], KNOWN).await,
        format!("{KNOWN}* on\n"),
        "the inheriting pane still inherits after a refusal"
    );
    {
        let state = handler.state.lock().await;
        for index in 0..3 {
            assert!(
                state
                    .pane_profile_in_window(&session_name, 0, index)
                    .is_ok(),
                "pane {index} keeps a live runtime after the refusals"
            );
            assert!(
                state.transcript_handle(&slots[index as usize]).is_ok(),
                "pane {index} keeps its transcript after the refusals"
            );
        }
    }

    // The window is still rotatable afterwards, and ownership still follows.
    rotate_ok(&handler, window, RotateWindowDirection::Up).await;
    let after_up = window_pane_ids(&handler, &session_name, 0).await;
    assert_rotated_left(&after_up, &before.0, 1, "after the refusals");
    for (index, token) in ["B", "C", "A"].iter().enumerate() {
        assert_slot_owner(
            &handler,
            &slots[index],
            TOKEN,
            after_up[index],
            Some(token),
            "after the refusals",
        )
        .await;
    }
}
