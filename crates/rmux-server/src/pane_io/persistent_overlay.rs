use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;

use tokio::sync::mpsc;

use super::attach_control::AttachControl;
use super::control::try_recv_attach_control;
use super::types::{AttachTarget, OpenAttachTarget, OverlayFrame};

pub(super) fn discard_stale_persistent_overlays(
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    deferred_controls: &mut VecDeque<AttachControl>,
    barrier_state_id: u64,
    control_backlog: &AtomicUsize,
) {
    let mut retained_controls = VecDeque::with_capacity(deferred_controls.len());
    while let Some(control) = deferred_controls.pop_front() {
        match control {
            AttachControl::Switch(next_target)
                if next_target
                    .with_target(|target| {
                        is_stale_persistent_switch(Some(barrier_state_id), target)
                    })
                    .unwrap_or(false) => {}
            AttachControl::Overlay(overlay)
                if overlay
                    .persistent_state_id
                    .is_some_and(|state_id| state_id < barrier_state_id) => {}
            other => retained_controls.push_back(other),
        }
    }
    *deferred_controls = retained_controls;

    let Some(control_rx) = attach_controls else {
        return;
    };
    while let Ok(control) = try_recv_attach_control(control_rx, control_backlog) {
        match control {
            AttachControl::Switch(next_target)
                if next_target
                    .with_target(|target| {
                        is_stale_persistent_switch(Some(barrier_state_id), target)
                    })
                    .unwrap_or(false) => {}
            AttachControl::Overlay(overlay)
                if overlay
                    .persistent_state_id
                    .is_some_and(|state_id| state_id < barrier_state_id) => {}
            other => deferred_controls.push_back(other),
        }
    }
}

pub(super) fn advance_persistent_overlay_state(
    current_state_id: &mut Option<u64>,
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    deferred_controls: &mut VecDeque<AttachControl>,
    barrier_state_id: u64,
    control_backlog: &AtomicUsize,
) {
    if barrier_state_id == 0 {
        return;
    }
    if current_state_id.is_some_and(|current| barrier_state_id < current) {
        return;
    }
    *current_state_id = Some(barrier_state_id);
    discard_stale_persistent_overlays(
        attach_controls,
        deferred_controls,
        barrier_state_id,
        control_backlog,
    );
}

pub(super) fn prime_persistent_overlay_barriers(
    current_state_id: &mut Option<u64>,
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    deferred_controls: &mut VecDeque<AttachControl>,
    control_backlog: &AtomicUsize,
) {
    let Some(control_rx) = attach_controls else {
        return;
    };

    while let Ok(control) = try_recv_attach_control(control_rx, control_backlog) {
        deferred_controls.push_back(control);
    }

    let mut latest_barrier = None::<u64>;
    let mut retained_controls = VecDeque::with_capacity(deferred_controls.len());
    while let Some(control) = deferred_controls.pop_front() {
        match control {
            AttachControl::AdvancePersistentOverlayState(state_id) => {
                latest_barrier =
                    Some(latest_barrier.map_or(state_id, |current| current.max(state_id)));
            }
            other => retained_controls.push_back(other),
        }
    }
    *deferred_controls = retained_controls;

    if let Some(barrier_state_id) = latest_barrier {
        advance_persistent_overlay_state(
            current_state_id,
            Some(control_rx),
            deferred_controls,
            barrier_state_id,
            control_backlog,
        );
    }
}

pub(super) fn is_stale_persistent_switch(
    current_state_id: Option<u64>,
    next_target: &AttachTarget,
) -> bool {
    match (current_state_id, next_target.persistent_overlay_state_id) {
        (Some(current_state_id), Some(incoming_state_id)) => incoming_state_id < current_state_id,
        _ => false,
    }
}

pub(super) fn accept_persistent_overlay_state(
    current_state_id: &mut Option<u64>,
    overlay: &OverlayFrame,
) -> bool {
    let Some(incoming_state_id) = overlay.persistent_state_id else {
        return true;
    };
    if current_state_id.is_some_and(|current| incoming_state_id < current) {
        return false;
    }
    *current_state_id = Some(incoming_state_id);
    true
}

pub(super) fn take_pending_persistent_overlay_for_state(
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    deferred_controls: &mut VecDeque<AttachControl>,
    expected_state_id: Option<u64>,
    render_generation: u64,
    current_overlay_generation: u64,
    control_backlog: &AtomicUsize,
) -> Option<OverlayFrame> {
    let expected_state_id = expected_state_id?;
    if let Some(control_rx) = attach_controls {
        while let Ok(control) = try_recv_attach_control(control_rx, control_backlog) {
            deferred_controls.push_back(control);
        }
    }

    let mut selected = None;
    let mut retained = VecDeque::with_capacity(deferred_controls.len());
    while let Some(control) = deferred_controls.pop_front() {
        match control {
            AttachControl::Overlay(overlay)
                if selected.is_none()
                    && overlay_matches_switch(
                        &overlay,
                        expected_state_id,
                        render_generation,
                        current_overlay_generation,
                    ) =>
            {
                selected = Some(overlay);
            }
            other => retained.push_back(other),
        }
    }
    *deferred_controls = retained;
    selected
}

fn overlay_matches_switch(
    overlay: &OverlayFrame,
    expected_state_id: u64,
    render_generation: u64,
    current_overlay_generation: u64,
) -> bool {
    overlay.persistent
        && !overlay.frame.is_empty()
        && overlay.persistent_state_id == Some(expected_state_id)
        && overlay.render_generation == render_generation
        && overlay.overlay_generation >= current_overlay_generation
}

pub(super) fn update_persistent_overlay_cache(
    cache: &mut Option<Vec<u8>>,
    visible: &mut bool,
    overlay: &OverlayFrame,
) {
    if !overlay.persistent {
        return;
    }
    if overlay.frame.is_empty() {
        *cache = None;
        *visible = false;
    } else {
        *cache = Some(overlay.frame.clone());
        *visible = true;
    }
}

pub(super) fn switch_requires_screen_clear(
    persistent_overlay_visible: bool,
    persistent_overlay_cached: bool,
    current_overlay_state_id: Option<u64>,
    current_target_state_id: Option<u64>,
    next_target_state_id: Option<u64>,
) -> bool {
    let had_persistent_overlay = persistent_overlay_visible || persistent_overlay_cached;
    let stale_persistent_overlay_on_screen = current_overlay_state_id != current_target_state_id;
    let leaving_persistent_overlay =
        current_target_state_id.is_some() && next_target_state_id.is_none();

    had_persistent_overlay || stale_persistent_overlay_on_screen || leaving_persistent_overlay
}

pub(super) fn clear_then_base_frame(current_target: &OpenAttachTarget) -> Vec<u8> {
    let mut frame = Vec::with_capacity(current_target.render_frame.len() + 10);
    frame.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    frame.extend_from_slice(&current_target.render_frame);
    frame
}

pub(super) fn replacement_persistent_overlay_frame(
    cache: &Option<Vec<u8>>,
    visible: bool,
    next_target: &AttachTarget,
) -> Option<Vec<u8>> {
    if !visible || next_target.persistent_overlay_state_id.is_none() {
        return None;
    }
    cache.clone()
}

pub(super) fn persistent_overlay_replacement_pending(
    controls: &VecDeque<AttachControl>,
    current_state_id: Option<u64>,
) -> bool {
    let Some(current_state_id) = current_state_id else {
        return false;
    };
    controls.iter().any(|control| match control {
        AttachControl::Switch(_) => true,
        AttachControl::Overlay(overlay) => {
            overlay.persistent
                && !overlay.frame.is_empty()
                && overlay
                    .persistent_state_id
                    .map(|state_id| state_id >= current_state_id)
                    .unwrap_or(true)
        }
        _ => false,
    })
}

pub(super) fn defer_persistent_clear(
    persistent_clear: bool,
    controls: &VecDeque<AttachControl>,
    current_state_id: Option<u64>,
) -> bool {
    persistent_clear && persistent_overlay_replacement_pending(controls, current_state_id)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::mpsc;

    use crate::pane_io::{AttachControl, OverlayFrame};

    use super::{
        advance_persistent_overlay_state, switch_requires_screen_clear,
        take_pending_persistent_overlay_for_state,
    };

    #[test]
    fn pending_overlay_for_state_is_removed_for_frame_composition() {
        let mut controls = VecDeque::from([
            AttachControl::Write(b"before".to_vec()),
            AttachControl::Overlay(OverlayFrame::persistent_with_state(
                b"MENU".to_vec(),
                2,
                4,
                9,
            )),
            AttachControl::Write(b"after".to_vec()),
        ]);

        let control_backlog = AtomicUsize::new(0);
        let overlay = take_pending_persistent_overlay_for_state(
            None,
            &mut controls,
            Some(9),
            2,
            0,
            &control_backlog,
        )
        .expect("matching overlay");

        assert_eq!(overlay.frame, b"MENU");
        assert_eq!(controls.len(), 2);
        assert!(matches!(
            controls.pop_front(),
            Some(AttachControl::Write(_))
        ));
        assert!(matches!(
            controls.pop_front(),
            Some(AttachControl::Write(_))
        ));
    }

    #[test]
    fn pending_overlay_for_state_keeps_nonmatching_controls() {
        let mut controls = VecDeque::from([AttachControl::Overlay(
            OverlayFrame::persistent_with_state(b"OLD".to_vec(), 1, 4, 8),
        )]);

        let control_backlog = AtomicUsize::new(0);
        let overlay = take_pending_persistent_overlay_for_state(
            None,
            &mut controls,
            Some(9),
            2,
            0,
            &control_backlog,
        );

        assert!(overlay.is_none());
        assert_eq!(controls.len(), 1);
    }

    #[test]
    fn plain_refresh_without_overlay_does_not_clear_the_screen() {
        assert!(!switch_requires_screen_clear(
            false, false, None, None, None,
        ));
    }

    #[test]
    fn zero_overlay_barrier_is_ignored_as_initial_sentinel() {
        let mut current_state_id = None;
        let mut controls = VecDeque::new();

        let control_backlog = AtomicUsize::new(0);
        advance_persistent_overlay_state(
            &mut current_state_id,
            None,
            &mut controls,
            0,
            &control_backlog,
        );

        assert_eq!(current_state_id, None);
    }

    #[test]
    fn priming_overlay_barriers_decrements_received_control_backlog() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(AttachControl::AdvancePersistentOverlayState(7))
            .expect("control send succeeds");
        let control_backlog = AtomicUsize::new(1);
        let mut current_state_id = None;
        let mut controls = VecDeque::new();

        super::prime_persistent_overlay_barriers(
            &mut current_state_id,
            Some(&mut rx),
            &mut controls,
            &control_backlog,
        );

        assert_eq!(current_state_id, Some(7));
        assert_eq!(control_backlog.load(Ordering::Acquire), 0);
    }

    #[test]
    fn leaving_or_replacing_persistent_overlay_clears_the_screen() {
        assert!(switch_requires_screen_clear(
            true,
            false,
            Some(7),
            Some(7),
            None,
        ));
        assert!(switch_requires_screen_clear(
            false,
            true,
            Some(7),
            Some(7),
            Some(8),
        ));
    }

    #[test]
    fn stale_persistent_overlay_state_clears_the_screen() {
        assert!(switch_requires_screen_clear(
            false,
            false,
            Some(8),
            Some(7),
            Some(8),
        ));
    }
}
