use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;

use tokio::sync::mpsc;

use super::attach_control::{AttachControl, QueuedAttachTarget};
use super::control::try_recv_attach_control;
use super::types::{AttachTarget, OpenAttachTarget, OverlayFrame};
use crate::outer_terminal::RenderedClientTitle;

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
                    .unwrap_or(false) =>
            {
                retained_controls.extend(salvaged_client_title_control(&next_target));
            }
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
                    .unwrap_or(false) =>
            {
                deferred_controls.extend(salvaged_client_title_control(&next_target));
            }
            AttachControl::Overlay(overlay)
                if overlay
                    .persistent_state_id
                    .is_some_and(|state_id| state_id < barrier_state_id) => {}
            other => deferred_controls.push_back(other),
        }
    }
}

/// The outer-terminal bytes a frame owes its client, whatever happens to the
/// frame itself.
///
/// OSC 0 and OSC 7 describe the terminal rather than the drawn screen — tmux
/// writes them from its server loop, outside any redraw — and the next render
/// deduplicates against them. Dropping them together with a stale frame would
/// leave the outer terminal on the previous title with nothing left to correct
/// it, which is the stranded-title half of issue #182.
pub(super) fn undelivered_client_title_bytes(target: &AttachTarget) -> Option<&[u8]> {
    target
        .client_title
        .as_ref()
        .map(RenderedClientTitle::bytes)
        .filter(|bytes| !bytes.is_empty())
}

/// Replaces a discarded stale switch with a write carrying only what its outer
/// terminal is still owed, in the same queue position.
fn salvaged_client_title_control(next_target: &QueuedAttachTarget) -> Option<AttachControl> {
    next_target
        .with_target(|target| undelivered_client_title_bytes(target).map(<[u8]>::to_vec))
        .flatten()
        .map(AttachControl::Write)
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

/// Replays the client-side overlay-barrier rules over everything a client
/// still has queued, and returns the byte payloads its outer terminal would
/// actually receive, oldest first.
///
/// This runs the production [`prime_persistent_overlay_barriers`] drain and
/// the production [`is_stale_persistent_switch`] rule the attach loop applies
/// before drawing a switch, so a frame this reports as delivered is one the
/// client really draws — and one it omits is really discarded.
#[cfg(test)]
pub(crate) fn replay_client_visible_payloads(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) -> Vec<Vec<u8>> {
    let control_backlog = AtomicUsize::new(0);
    let mut current_state_id = None;
    let mut deferred_controls = VecDeque::new();
    prime_persistent_overlay_barriers(
        &mut current_state_id,
        Some(control_rx),
        &mut deferred_controls,
        &control_backlog,
    );

    let mut payloads = Vec::new();
    for control in deferred_controls {
        match control {
            AttachControl::Switch(next_target) => {
                // The same choice `apply_pending_attach_controls` makes: draw
                // the frame, or drop it and hand over only what its outer
                // terminal is still owed.
                let payload = next_target.with_target(|target| {
                    if is_stale_persistent_switch(current_state_id, target) {
                        undelivered_client_title_bytes(target).map(<[u8]>::to_vec)
                    } else {
                        Some(target.render_frame.clone())
                    }
                });
                payloads.extend(payload.flatten());
            }
            AttachControl::Write(bytes) => payloads.push(bytes),
            _ => {}
        }
    }
    payloads
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

/// Diff only self-contained, ordered popup rows, against the last frame that
/// was actually emitted. Never cache a delta: switches and refresh-client must
/// still be able to restore the entire popup, including unchanged rows.
pub(super) fn popup_frame_delta(
    cache: Option<&[u8]>,
    visible: bool,
    overlay: &OverlayFrame,
) -> Option<Vec<u8>> {
    if !overlay.row_diff || !overlay.persistent || !visible {
        return None;
    }
    let before = popup_rows(cache?)?;
    let after = popup_rows(&overlay.frame)?;
    if before.len() != after.len() || before.iter().zip(&after).any(|(a, b)| a.0 != b.0) {
        return None;
    }
    Some(
        before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a.1 != b.1)
            .flat_map(|(_, row)| row.1.iter().copied())
            .collect(),
    )
}

/// The popup renderer saves/restores the cursor and resets SGR for each row.
/// Require one strictly increasing row at a fixed column. Borders, nested
/// menus, transient messages, and other command layouts conservatively fall
/// back to a full frame rather than skipping overlapping drawing operations.
fn popup_rows(frame: &[u8]) -> Option<Vec<(&[u8], &[u8])>> {
    let mut rest = frame;
    let mut rows = Vec::new();
    let mut last_y = 0;
    let mut column = None;
    while !rest.is_empty() {
        let position = rest.strip_prefix(b"\x1b7\x1b[0m\x1b[")?;
        let h = position.iter().position(|byte| *byte == b'H')?;
        let coordinates = std::str::from_utf8(&position[..h]).ok()?;
        let (y, x) = coordinates.split_once(';')?;
        let y: u16 = y.parse().ok()?;
        let x: u16 = x.parse().ok()?;
        if y <= last_y || column.is_some_and(|col| col != x) {
            return None;
        }
        last_y = y;
        column = Some(x);
        let end = rest.windows(2).position(|pair| pair == b"\x1b8")? + 2;
        let row = &rest[..end];
        if !row.ends_with(b"\x1b[0m\x1b8") {
            return None;
        }
        rows.push((&position[..=h], row));
        rest = &rest[end..];
    }
    (!rows.is_empty()).then_some(rows)
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

#[cfg(test)]
mod popup_delta_tests {
    use super::*;
    use crate::renderer::{render_popup_overlay, OverlayRect, PopupContent, PopupRenderSpec};
    use rmux_core::{input::InputParser, BoxLines, GridRenderOptions, Screen, Style};
    use rmux_proto::TerminalSize;

    fn popup(rows: &[&str]) -> OverlayFrame {
        OverlayFrame::persistent(
            render_popup_overlay(&PopupRenderSpec {
                rect: OverlayRect {
                    x: 2,
                    y: 2,
                    width: 20,
                    height: 3,
                },
                title: String::new(),
                style: Style::default(),
                border_style: Style::default(),
                border_lines: BoxLines::None,
                content: PopupContent::Surface(
                    rows.iter().map(|r| r.as_bytes().to_vec()).collect(),
                ),
            }),
            1,
            1,
        )
        .with_row_diff()
    }

    fn screen(frames: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut parser = InputParser::new();
        let mut screen = Screen::new(TerminalSize { cols: 40, rows: 10 }, 0);
        for frame in frames {
            parser.parse(frame, &mut screen);
        }
        (0..10)
            .map(|row| {
                screen
                    .render_visible_line_independent_with_default_style(
                        row,
                        GridRenderOptions {
                            with_sequences: true,
                            include_empty_cells: true,
                            trim_spaces: false,
                            ..GridRenderOptions::default()
                        },
                        &Style::default(),
                    )
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn popup_delta_preserves_screen_and_erases_shortened_coloured_wide_rows() {
        let before = popup(&["unchanged", "\x1b[31m긴 문자열 abcdef", "last"]);
        let after = popup(&["unchanged", "\x1b[32m짧음", "last"]);
        let delta = popup_frame_delta(Some(&before.frame), true, &after).unwrap();
        assert!(delta.len() < after.frame.len() / 2);
        assert!(!String::from_utf8_lossy(&delta).contains("unchanged"));
        assert_eq!(screen(&[&before.frame, &delta]), screen(&[&after.frame]));
        let cleared = popup(&["unchanged", "", "last"]);
        let delta = popup_frame_delta(Some(&after.frame), true, &cleared).unwrap();
        assert_eq!(screen(&[&after.frame, &delta]), screen(&[&cleared.frame]));
    }

    #[test]
    fn popup_delta_skips_identical_frames_but_caches_full_restorable_frame() {
        let before = popup(&["one", "two", "three"]);
        assert_eq!(
            popup_frame_delta(Some(&before.frame), true, &before),
            Some(vec![])
        );
        let after = popup(&["one", "changed", "three"]);
        let mut cache = Some(before.frame);
        let mut visible = true;
        let delta = popup_frame_delta(cache.as_deref(), visible, &after).unwrap();
        update_persistent_overlay_cache(&mut cache, &mut visible, &after);
        assert_ne!(cache.as_deref(), Some(delta.as_slice()));
        assert_eq!(cache, Some(after.frame));
    }

    #[test]
    fn popup_delta_falls_back_for_restore_resize_move_and_overlapping_menu() {
        let before = popup(&["one", "two", "three"]);
        assert!(popup_frame_delta(None, true, &before).is_none());
        assert!(popup_frame_delta(Some(&before.frame), false, &before).is_none());
        let moved = OverlayFrame::persistent(
            String::from_utf8(before.frame.clone())
                .unwrap()
                .replace("[3;3H", "[2;3H")
                .into_bytes(),
            1,
            2,
        )
        .with_row_diff();
        assert!(popup_frame_delta(Some(&before.frame), true, &moved).is_none());
        let mut resized = popup(&["one", "two", "three"]);
        let last_row_len = popup_rows(&resized.frame).unwrap().last().unwrap().1.len();
        resized.frame.truncate(resized.frame.len() - last_row_len);
        assert!(popup_frame_delta(Some(&before.frame), true, &resized).is_none());
        let mut nested = popup(&["one", "two", "three"]);
        nested.frame.extend_from_slice(&before.frame);
        assert!(popup_frame_delta(Some(&nested.frame), true, &nested).is_none());
        let explicit_refresh = OverlayFrame::persistent(before.frame.clone(), 1, 2);
        assert!(popup_frame_delta(Some(&before.frame), true, &explicit_refresh).is_none());
    }
}
