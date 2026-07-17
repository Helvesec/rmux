use super::target_error::invalid_window_target_with_reason;
use super::{PaneSwapOptions, Session};
use crate::{Pane, PaneId, Window};
use rmux_proto::RmuxError;

pub(super) fn validate_swap_destination(
    destination_window: &Window,
    incoming_pane: &Pane,
    replaced_pane_index: u32,
) -> Result<(), RmuxError> {
    let replaced_position = destination_window
        .pane_position(replaced_pane_index)
        .expect("validated replaced pane must exist");
    for (position, existing) in destination_window.panes().iter().enumerate() {
        if position != replaced_position && existing.id() == incoming_pane.id() {
            return Err(RmuxError::Server(format!(
                "pane id {} already exists in window {}",
                incoming_pane.id().as_u32(),
                destination_window.id()
            )));
        }
    }
    Ok(())
}

pub(super) fn apply_swap_between_windows(
    source_window: &mut Window,
    source: SwapPaneEntry,
    target_window: &mut Window,
    target: SwapPaneEntry,
    options: PaneSwapOptions,
) -> Result<(), RmuxError> {
    let source_active_before = source_window.active_pane_index();
    let target_active_before = target_window.active_pane_index();
    let source_active_before_id = source_window
        .active_pane()
        .expect("source window has an active pane")
        .id();
    let target_active_before_id = target_window
        .active_pane()
        .expect("target window has an active pane")
        .id();
    let source_last_before_id = source_window
        .last_pane_index()
        .and_then(|index| source_window.pane(index).map(Pane::id));
    let target_last_before_id = target_window
        .last_pane_index()
        .and_then(|index| target_window.pane(index).map(Pane::id));
    let source_pane_id = source.pane.id();
    let target_pane_id = target.pane.id();
    let select_target_in_source = !options.detached || source_active_before == source.index;
    let select_source_in_target = !options.detached || target_active_before == target.index;
    source_window.push_zoom(options.preserve_zoom);
    target_window.push_zoom(options.preserve_zoom);
    source_window.replace_pane_for_swap(source.index, target.pane)?;
    target_window.replace_pane_for_swap(target.index, source.pane)?;

    let source_active_after_id = if select_target_in_source {
        target_pane_id
    } else {
        source_active_before_id
    };
    let target_active_after_id = if select_source_in_target {
        source_pane_id
    } else {
        target_active_before_id
    };
    let source_last_after_id = swapped_window_last_pane_id(
        source_active_before_id,
        source_last_before_id,
        source_pane_id,
        target_pane_id,
        select_target_in_source,
    );
    let target_last_after_id = swapped_window_last_pane_id(
        target_active_before_id,
        target_last_before_id,
        target_pane_id,
        source_pane_id,
        select_source_in_target,
    );

    if select_target_in_source {
        source_window.renumber_panes_by_position_stamping(
            source_active_after_id,
            source_last_after_id,
            Some(source_active_before_id),
        );
    } else {
        source_window.renumber_panes_by_position(source_active_after_id, source_last_after_id);
    }
    if select_source_in_target {
        target_window.renumber_panes_by_position_stamping(
            target_active_after_id,
            target_last_after_id,
            Some(target_active_before_id),
        );
    } else {
        target_window.renumber_panes_by_position(target_active_after_id, target_last_after_id);
    }
    source_window.recalculate_geometry();
    target_window.recalculate_geometry();
    source_window.pop_zoom();
    target_window.pop_zoom();
    Ok(())
}

fn swapped_window_last_pane_id(
    active_before_id: PaneId,
    last_before_id: Option<PaneId>,
    outgoing_pane_id: PaneId,
    _incoming_pane_id: PaneId,
    selects_incoming: bool,
) -> Option<PaneId> {
    if selects_incoming {
        if active_before_id == outgoing_pane_id {
            return last_before_id.filter(|pane_id| *pane_id != outgoing_pane_id);
        }
        return Some(active_before_id);
    }

    match last_before_id {
        Some(last_pane_id) if last_pane_id == outgoing_pane_id => None,
        other => other,
    }
}

pub(super) struct SwapPaneEntry {
    pub(super) index: u32,
    pub(super) pane: Pane,
}

pub(super) const fn adjusted_insert_position(
    source_position: usize,
    target_position: usize,
) -> usize {
    if source_position < target_position {
        target_position
    } else {
        target_position + 1
    }
}

pub(super) const fn adjusted_insert_position_before(
    source_position: usize,
    target_position: usize,
) -> usize {
    if source_position < target_position {
        target_position.saturating_sub(1)
    } else {
        target_position
    }
}

pub(super) fn resolve_break_destination_index(
    session: &Session,
    target_window_index: Option<u32>,
    allowed_occupied_index: Option<u32>,
) -> Result<u32, RmuxError> {
    match target_window_index {
        Some(target_window_index) => {
            if session.window_at(target_window_index).is_some()
                && allowed_occupied_index != Some(target_window_index)
            {
                return Err(invalid_window_target_with_reason(
                    session.name(),
                    target_window_index,
                    "window index already exists in session",
                ));
            }

            Ok(target_window_index)
        }
        None => session.lowest_available_window_index_at_or_above(0),
    }
}
