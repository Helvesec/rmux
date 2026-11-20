//! Moves of pane terminals and outputs between grouped runtime sessions, with their rollbacks.

use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName};

use super::super::super::HandlerState;

pub(super) fn move_grouped_runtime_pane(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    destination_runtime: &SessionName,
    pane_id: PaneId,
) -> Result<(), RmuxError> {
    state
        .terminals
        .move_panes_between_sessions(source_runtime, destination_runtime, &[pane_id])?;
    if let Err(error) =
        state.move_pane_outputs_between_sessions(source_runtime, destination_runtime, &[pane_id])
    {
        state.terminals.move_panes_between_sessions(
            destination_runtime,
            source_runtime,
            &[pane_id],
        )?;
        return Err(error);
    }
    Ok(())
}

pub(super) fn rollback_grouped_runtime_pane(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    destination_runtime: &SessionName,
    pane_id: PaneId,
) -> Result<(), RmuxError> {
    let output_result =
        state.move_pane_outputs_between_sessions(destination_runtime, source_runtime, &[pane_id]);
    let terminal_result = state.terminals.move_panes_between_sessions(
        destination_runtime,
        source_runtime,
        &[pane_id],
    );
    output_result?;
    terminal_result
}

pub(super) fn move_grouped_runtime_swap(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    source_pane_id: PaneId,
    target_runtime: &SessionName,
    target_pane_id: PaneId,
) -> Result<(), RmuxError> {
    state.terminals.swap_panes_between_sessions(
        source_runtime,
        &[source_pane_id],
        target_runtime,
        &[target_pane_id],
    )?;
    if let Err(error) = state.swap_pane_outputs_between_sessions(
        source_runtime,
        &[source_pane_id],
        target_runtime,
        &[target_pane_id],
    ) {
        state.terminals.swap_panes_between_sessions(
            source_runtime,
            &[target_pane_id],
            target_runtime,
            &[source_pane_id],
        )?;
        return Err(error);
    }
    Ok(())
}

pub(super) fn rollback_grouped_runtime_swap(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    source_pane_id: PaneId,
    target_runtime: &SessionName,
    target_pane_id: PaneId,
) -> Result<(), RmuxError> {
    let output_result = state.swap_pane_outputs_between_sessions(
        source_runtime,
        &[target_pane_id],
        target_runtime,
        &[source_pane_id],
    );
    let terminal_result = state.terminals.swap_panes_between_sessions(
        source_runtime,
        &[target_pane_id],
        target_runtime,
        &[source_pane_id],
    );
    output_result?;
    terminal_result
}
