use super::pane_transfer_shared::adjusted_insert_position;
use super::target_error::invalid_pane_target;
use super::{PaneJoinOptions, PaneSwapOptions, SessionPaneTarget};
use crate::window::PaneInsertionTarget;
use crate::{Pane, Window};
use rmux_proto::{PaneSplitSize, RmuxError, SplitDirection};
pub(super) fn swap_within(
    window: &mut Window,
    name: &rmux_proto::SessionName,
    window_index: u32,
    source_pane_index: u32,
    target_pane_index: u32,
    options: PaneSwapOptions,
) -> Result<(), RmuxError> {
    if source_pane_index == target_pane_index {
        if window.pane(source_pane_index).is_none() {
            return Err(invalid_pane_target(
                name,
                window_index,
                source_pane_index,
                "pane index does not exist in session",
            ));
        }
        // tmux reaches its common zoom epilogue even when both targets are
        // the same pane. No pane exchange or selection update takes place.
        window.push_zoom(options.preserve_zoom);
        window.pop_zoom();
        return Ok(());
    }

    if window.pane(source_pane_index).is_none() {
        return Err(invalid_pane_target(
            name,
            window_index,
            source_pane_index,
            "pane index does not exist in session",
        ));
    }
    if window.pane(target_pane_index).is_none() {
        return Err(invalid_pane_target(
            name,
            window_index,
            target_pane_index,
            "pane index does not exist in session",
        ));
    }

    window.push_zoom(options.preserve_zoom);
    let active_before_id = window
        .active_pane()
        .expect("validated window must have an active pane before swap")
        .id();
    let source_pane_id = window
        .pane(source_pane_index)
        .expect("validated source pane must exist before swap")
        .id();
    let target_pane_id = window
        .pane(target_pane_index)
        .expect("validated target pane must exist before swap")
        .id();
    // Public swap refusal follows the ordinary push-zoom side effect. Do not
    // change the lower-level exchange primitive used by other transfers.
    if window.is_floating_pane_id(source_pane_id) || window.is_floating_pane_id(target_pane_id) {
        return Err(RmuxError::Server("cannot swap floating panes".to_owned()));
    }
    let swapped = window.swap_panes(source_pane_index, target_pane_index);
    debug_assert!(swapped, "validated in-window swap must succeed");

    if options.detached {
        let active_after_id =
            if active_before_id == source_pane_id || active_before_id == target_pane_id {
                source_pane_id
            } else {
                active_before_id
            };
        window.select_pane_by_id(active_after_id);
    } else {
        window.select_pane_by_id(target_pane_id);
    }
    window.pop_zoom();

    Ok(())
}

pub(super) fn join_within(
    window: &mut Window,
    name: &rmux_proto::SessionName,
    source: SessionPaneTarget,
    target: SessionPaneTarget,
    options: PaneJoinOptions,
) -> Result<(), RmuxError> {
    if source.pane_index == target.pane_index {
        return Err(RmuxError::Server(
            "source and target panes must be different".to_owned(),
        ));
    }

    let source_position = window.pane_position(source.pane_index).ok_or_else(|| {
        invalid_pane_target(
            name,
            source.window_index,
            source.pane_index,
            "pane index does not exist in session",
        )
    })?;
    let target_position = window.pane_position(target.pane_index).ok_or_else(|| {
        invalid_pane_target(
            name,
            target.window_index,
            target.pane_index,
            "pane index does not exist in session",
        )
    })?;
    let source_pane = window
        .pane(source.pane_index)
        .cloned()
        .expect("validated source pane must exist");
    let source_geometry = source_pane.geometry();
    let source_pane_id = source_pane.id();
    let target_pane = window
        .pane(target.pane_index)
        .expect("validated target pane must exist");
    let target_geometry = target_pane.geometry();
    let target_pane_id = target_pane.id();
    if window.is_floating_pane_id(target_pane_id) {
        return Err(RmuxError::Server(
            "cannot join panes through a floating placement".to_owned(),
        ));
    }
    // A floating source joins the tiling through its own primitive: it owns a
    // stack placement instead of a layout leaf. Everything else below - flags,
    // selection, renumbering and sizing - stays the inherited tiled rule.
    let source_is_floating = window.is_floating_pane_id(source_pane_id);
    window.ensure_accepts_pane(&source_pane, Some(source_position))?;
    let source_axis_size = join_axis_for_size(
        rmux_proto::TerminalSize {
            cols: source_geometry.cols(),
            rows: source_geometry.rows(),
        },
        options.direction,
    );
    let requested_size = join_requested_size(
        window,
        target.pane_index,
        options.direction,
        options.full_size,
        options.size,
    )?;
    let active_before_id = window
        .active_pane()
        .expect("validated window must have an active pane")
        .id();
    let last_before_id = window
        .last_pane_index()
        .and_then(|pane_index| window.pane(pane_index).map(|pane| pane.id()));
    let detached_active_after_removal_id = if active_before_id == source_pane_id {
        last_before_id
            .filter(|pane_id| *pane_id != source_pane_id)
            .or_else(|| {
                if source_position > 0 {
                    window.panes().get(source_position - 1).map(Pane::id)
                } else {
                    window.panes().get(source_position + 1).map(Pane::id)
                }
            })
    } else {
        Some(active_before_id)
    };
    let detached_last_after_removal_id = if active_before_id == source_pane_id {
        detached_active_after_removal_id
            .is_some_and(|pane_id| pane_id != target_pane_id)
            .then_some(target_pane_id)
    } else {
        last_before_id.filter(|pane_id| *pane_id != source_pane_id)
    };

    // tmux reinserts the moved pane immediately after its destination in the
    // pane list, whatever `-b` and `-f` say; `-b` only picks the side of the
    // split. Same-window transfers compensate for the source leaving the list.
    let insert_position = adjusted_insert_position(source_position, target_position);
    let moved_pane_id = if options.full_size {
        // `split_root` reads only the window rectangle, which extracting a pane
        // does not change, so validating here is exact and a window with no
        // room refuses before the source leaves its placement.
        let insertion = window.plan_pane_insertion(
            source_pane_id,
            PaneInsertionTarget::WindowRoot,
            options.direction,
            options.before,
        )?;
        window.auto_unzoom();
        let mut moved_pane = window
            .extract_pane(source.pane_index)
            .expect("validated source pane must extract");
        let moved_pane_id = moved_pane.id();
        let transient_index = window
            .panes()
            .iter()
            .map(|pane| pane.index())
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        moved_pane.set_index(transient_index);
        let list_position = window.list_position_after(target_pane_id).ok_or_else(|| {
            RmuxError::Server("target pane disappeared during a full-size join".to_owned())
        })?;
        window.commit_pane_insertion(insertion, list_position, moved_pane)?;
        moved_pane_id
    } else if source_is_floating {
        window.move_floating_pane_by_splitting_target(
            source_position,
            target_position,
            insert_position,
            options.direction,
            options.before,
        )?
    } else {
        window.move_pane_by_splitting_target(
            source_position,
            target_position,
            insert_position,
            options.direction,
            options.before,
        )?
    };
    if options.detached {
        window.renumber_panes_by_position(
            detached_active_after_removal_id
                .expect("detached pane move should leave an active pane"),
            detached_last_after_removal_id,
        );
    } else {
        let last_after_id = if active_before_id == moved_pane_id {
            last_before_id
        } else {
            Some(active_before_id)
        };
        window.renumber_panes_by_position_stamping(
            moved_pane_id,
            last_after_id,
            Some(active_before_id),
        );
    }
    let moved_pane_index = window
        .panes()
        .iter()
        .find(|pane| pane.id() == moved_pane_id)
        .map(|pane| pane.index())
        .expect("moved pane must survive in-window join");
    if let Some(requested_size) = requested_size {
        // The compensation below reads the source rectangle as a tiled cell
        // adjacent to the target. A floating rectangle is not a cell, so the
        // moved pane always receives `-l`/`-p` directly, as tmux 3.7b measures.
        let size_existing_target_side = !options.full_size
            && !options.before
            && !source_is_floating
            && same_axis_source_after_target(source_geometry, target_geometry, options.direction);
        let (sized_pane_index, size_to_apply) = if size_existing_target_side {
            window
                .panes()
                .iter()
                .find(|pane| pane.id() == target_pane_id)
                .map(|pane| pane.index())
                .map(|pane_index| {
                    (
                        pane_index,
                        source_axis_size.saturating_sub(requested_size).max(1),
                    )
                })
                .expect("target pane must survive in-window join")
        } else {
            (moved_pane_index, requested_size)
        };
        let _ = window.resize_pane_to(sized_pane_index, options.direction, size_to_apply);
    }

    Ok(())
}

pub(super) fn join_requested_size(
    target_window: &Window,
    target_pane_index: u32,
    direction: SplitDirection,
    full_size: bool,
    size: Option<PaneSplitSize>,
) -> Result<Option<u32>, RmuxError> {
    let Some(size) = size else {
        return Ok(None);
    };
    let base = if full_size {
        join_axis_for_size(target_window.size(), direction)
    } else {
        let pane = target_window.pane(target_pane_index).ok_or_else(|| {
            RmuxError::Server(format!(
                "cannot size missing target pane index {target_pane_index}"
            ))
        })?;
        join_axis_for_size(
            rmux_proto::TerminalSize {
                cols: pane.geometry().cols(),
                rows: pane.geometry().rows(),
            },
            direction,
        )
    };

    Ok(Some(match size {
        PaneSplitSize::Absolute(value) => value.max(1),
        PaneSplitSize::Percentage(value) => {
            let scaled = (base.saturating_mul(u32::from(value))) / 100;
            scaled.max(1)
        }
    }))
}

fn join_axis_for_size(size: rmux_proto::TerminalSize, direction: SplitDirection) -> u32 {
    match direction {
        SplitDirection::Vertical => u32::from(size.cols),
        SplitDirection::Horizontal => u32::from(size.rows),
    }
}

fn same_axis_source_after_target(
    source: crate::PaneGeometry,
    target: crate::PaneGeometry,
    direction: SplitDirection,
) -> bool {
    match direction {
        SplitDirection::Vertical => {
            source.y() == target.y() && source.rows() == target.rows() && source.x() > target.x()
        }
        SplitDirection::Horizontal => {
            source.x() == target.x() && source.cols() == target.cols() && source.y() > target.y()
        }
    }
}
