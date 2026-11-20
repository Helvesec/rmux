//! Input writes for focus transitions already committed under the state and attach locks.

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError};

#[cfg(test)]
use super::PaneInputSink;
use super::{
    pane_id_for_existing_target, prepare_resolved_pane_input_write, PaneInputLiveness,
    PaneInputWrite,
};
use crate::pane_terminals::HandlerState;

/// Prepares bytes emitted by a focus transition that was already committed
/// under the state and attach locks.
///
/// This route deliberately does not inherit a queued command's target
/// authority: the command may have removed its own target before the focus
/// aggregate selected another runtime. The runtime identity supplied by the
/// focus delta remains mandatory, so this is not a fallback writer and cannot
/// make a retired or replaced command target live again.
///
/// The bytes keep the sink the resolved preparation chose, which without a
/// paste envelope is this pane's own PTY. A focus report is the one input a
/// Windows console answers for itself: `DECSET 1004` is a mode of the
/// *application*, and the host asks for its own focus by writing `1004h` right
/// after an application has disabled it, so a `1004h` seen in a pane's output
/// is not evidence that the program in it wants `CSI I`
/// (`microsoft/terminal@0319ece` `adaptDispatch.cpp` and `_stream.cpp`;
/// measured on this host in the FKEY1 R6 attribution, where the eight bytes
/// following the producer's own `1004l` were exactly that `1004h`). On the raw
/// pipe the console's input state machine reads `CSI I` as a focus event and
/// delivers it through the privileged path, which reaches the application only
/// when the application really did ask for focus reports; as console text
/// records the same bytes are handed to whatever reader is there, which is how
/// `[I` reached a plain line reader. Nothing else of the resolved preparation
/// changes here: identity, generation, liveness and every non-PTY outcome are
/// the ones it chose.
pub(in crate::handler) fn prepare_committed_focus_input_write(
    state: &mut HandlerState,
    target: &PaneTarget,
    pane_id: PaneId,
    output_generation: u64,
    bytes: &[u8],
) -> Result<PaneInputWrite, RmuxError> {
    let pane_id = pane_id_for_committed_focus_target(state, target, pane_id, output_generation)?;
    let write = prepare_resolved_pane_input_write(
        state,
        target,
        bytes,
        PaneInputLiveness::TolerateDead,
        None,
        pane_id,
    )?;
    #[cfg(test)]
    if matches!(write.sink, PaneInputSink::CapturedForTest) {
        state.attribute_captured_focus_input_for_test(target, pane_id, output_generation, bytes);
    }
    Ok(write)
}

fn pane_id_for_committed_focus_target(
    state: &HandlerState,
    target: &PaneTarget,
    expected_pane_id: PaneId,
    expected_output_generation: u64,
) -> Result<PaneId, RmuxError> {
    let pane_id = pane_id_for_existing_target(state, target)?;
    let output_generation = state.pane_output_generation_for_target(target, pane_id);
    if pane_id == expected_pane_id && output_generation == expected_output_generation {
        return Ok(pane_id);
    }
    Err(RmuxError::invalid_target(
        target.to_string(),
        "committed focus runtime changed before its input effect",
    ))
}
