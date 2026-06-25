use rmux_proto::{
    PaneBroadcastInputFailure, PaneBroadcastInputResponse, PaneBroadcastInputSuccess, PaneId,
    PaneTarget, PaneTargetRef, Response, RmuxError,
};

use super::super::RequestHandler;
use super::{
    encode_tokens_for_target, prepare_pane_input_write, resolve_pane_target_ref,
    write_bytes_to_target_io,
};
use crate::pane_terminals::HandlerState;

struct PreparedBroadcastWrite {
    target_index: u32,
    target: PaneTarget,
    pane_id: Option<PaneId>,
    write: super::pane_io_encoding::PaneInputWrite,
    bytes: Vec<u8>,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_broadcast_input(
        &self,
        request: rmux_proto::PaneBroadcastInputRequest,
    ) -> Response {
        let key_count = request.keys.len();
        let (prepared, mut failures) = {
            let mut state = self.state.lock().await;
            prepare_broadcast_writes(&mut state, &request)
        };

        let mut successes = Vec::new();
        for prepared in prepared {
            match write_bytes_to_target_io(prepared.write, prepared.bytes).await {
                Ok(()) => successes.push(PaneBroadcastInputSuccess {
                    target_index: prepared.target_index,
                    target: prepared.target,
                    pane_id: prepared.pane_id,
                }),
                Err(error) => failures.push(PaneBroadcastInputFailure {
                    target_index: prepared.target_index,
                    target: PaneTargetRef::from(prepared.target),
                    error,
                }),
            }
        }

        Response::PaneBroadcastInput(PaneBroadcastInputResponse {
            key_count,
            successes,
            failures,
        })
    }
}

fn prepare_broadcast_writes(
    state: &mut HandlerState,
    request: &rmux_proto::PaneBroadcastInputRequest,
) -> (Vec<PreparedBroadcastWrite>, Vec<PaneBroadcastInputFailure>) {
    let mut prepared = Vec::new();
    let mut failures = Vec::new();

    for (target_index, target) in request.targets.iter().enumerate() {
        let target_index = u32::try_from(target_index).unwrap_or(u32::MAX);
        match prepare_one_broadcast_write(state, target_index, target, request) {
            Ok(write) => prepared.push(write),
            Err(error) => failures.push(PaneBroadcastInputFailure {
                target_index,
                target: target.clone(),
                error,
            }),
        }
    }

    (prepared, failures)
}

fn prepare_one_broadcast_write(
    state: &mut HandlerState,
    target_index: u32,
    target: &PaneTargetRef,
    request: &rmux_proto::PaneBroadcastInputRequest,
) -> Result<PreparedBroadcastWrite, RmuxError> {
    let target = resolve_pane_target_ref(state, target)?;
    let bytes = if request.literal {
        request
            .keys
            .iter()
            .flat_map(|key| key.as_bytes().iter().copied())
            .collect::<Vec<_>>()
    } else {
        encode_tokens_for_target(state, &target, &request.keys)?
    };
    let pane_id = pane_id_for_target(state, &target);
    let write = prepare_pane_input_write(state, &target, &bytes)?;

    Ok(PreparedBroadcastWrite {
        target_index,
        target,
        pane_id,
        write,
        bytes,
    })
}

fn pane_id_for_target(state: &HandlerState, target: &PaneTarget) -> Option<PaneId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
}
