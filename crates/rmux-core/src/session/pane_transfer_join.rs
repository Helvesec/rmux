use super::pane_transfer_borrowed::join_requested_size;
use super::target_error::invalid_pane_target;
use super::{PaneJoinOptions, SessionPaneTarget};
use crate::window::PaneInsertionTarget;
use crate::{Pane, Window};
use rmux_proto::{RmuxError, SessionName};

pub(super) fn join_between(
    source_window: &mut Window,
    source_name: &SessionName,
    source: SessionPaneTarget,
    target_window: &mut Window,
    target_name: &SessionName,
    target: SessionPaneTarget,
    options: PaneJoinOptions,
) -> Result<u32, RmuxError> {
    let source_pane = source_window
        .pane(source.pane_index)
        .cloned()
        .ok_or_else(|| {
            invalid_pane_target(
                source_name,
                source.window_index,
                source.pane_index,
                "pane index does not exist in session",
            )
        })?;
    let target_position = target_window
        .pane_position(target.pane_index)
        .ok_or_else(|| {
            invalid_pane_target(
                target_name,
                target.window_index,
                target.pane_index,
                "pane index does not exist in session",
            )
        })?;
    let target_pane_id = target_window
        .pane(target.pane_index)
        .expect("validated target pane must exist")
        .id();
    // A floating source keeps its identity across windows: `extract_pane`
    // drops its single placement and the destination inserts it as a tile.
    // Only a floating destination stays refused.
    if target_window.is_floating_pane_id(target_pane_id) {
        return Err(RmuxError::Server(
            "cannot join panes through a floating placement".to_owned(),
        ));
    }
    let requested_size = join_requested_size(
        target_window,
        target.pane_index,
        options.direction,
        options.full_size,
        options.size,
    )?;
    let transient_index = target_window
        .panes()
        .iter()
        .map(Pane::index)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut source_pane_for_validation = source_pane.clone();
    source_pane_for_validation.set_index(transient_index);

    // tmux reinserts a joined or moved pane immediately after its destination
    // in the pane list whatever `-b` and `-f` say; `-b` only picks the side of
    // the split and `-f` only picks the root over the named leaf.
    let insert_position = target_position + 1;
    let insertion_target = if options.full_size {
        PaneInsertionTarget::WindowRoot
    } else {
        PaneInsertionTarget::Leaf(target_pane_id)
    };
    target_window.ensure_accepts_pane(&source_pane_for_validation, None)?;
    // Validate the destination split before either window is unzoomed or the
    // source is extracted, so a window with no room refuses atomically instead
    // of rebuilding a named layout around a pane that already moved.
    let insertion = target_window.plan_pane_insertion(
        source_pane.id(),
        insertion_target,
        options.direction,
        options.before,
    )?;
    let target_active_before_id = target_window
        .active_pane()
        .map(Pane::id)
        .expect("validated target window must have an active pane");
    let target_last_before_id = Some(&*target_window).and_then(|window| {
        window
            .last_pane_index()
            .and_then(|pane_index| window.pane(pane_index).map(Pane::id))
    });

    target_window.auto_unzoom();
    source_window.auto_unzoom();
    let mut moved_pane = source_window
        .extract_pane(source.pane_index)
        .expect("validated source pane must extract");
    let moved_pane_id = moved_pane.id();
    moved_pane.set_index(transient_index);
    target_window.commit_pane_insertion(insertion, insert_position, moved_pane)?;
    if options.detached {
        target_window.renumber_panes_by_position(target_active_before_id, target_last_before_id);
    } else {
        target_window.renumber_panes_by_position_stamping(
            moved_pane_id,
            (target_active_before_id != moved_pane_id).then_some(target_active_before_id),
            Some(target_active_before_id),
        );
    }
    let moved_pane_index = target_window
        .panes()
        .iter()
        .find(|pane| pane.id() == moved_pane_id)
        .map(Pane::index)
        .expect("moved pane must survive cross-session join");
    if let Some(requested_size) = requested_size {
        if options.full_size {
            let _ =
                target_window.resize_pane_to(moved_pane_index, options.direction, requested_size);
        } else {
            let _ = target_window.resize_new_split_pane_to(
                moved_pane_index,
                options.direction,
                requested_size,
                options.before,
            );
        }
    }
    Ok(moved_pane_index)
}
