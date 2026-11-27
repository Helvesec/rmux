use std::sync::Arc;

use rmux_proto::{
    PaneBroadcastInputFailure, PaneBroadcastInputResponse, PaneBroadcastInputSuccess, PaneId,
    PaneTarget, PaneTargetRef, Response, RmuxError,
};

use super::super::RequestHandler;
use super::{
    encode_tokens_for_target, prepare_pane_input_write, resolve_pane_target_ref,
    write_bytes_to_target_io, PaneInputLiveness,
};
use crate::pane_terminals::HandlerState;

struct PreparedBroadcastWrite {
    target_index: u32,
    target: PaneTarget,
    pane_id: Option<PaneId>,
    write: super::pane_io_encoding::PaneInputWrite,
    bytes: Arc<[u8]>,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_broadcast_input(
        &self,
        request: rmux_proto::PaneBroadcastInputRequest,
    ) -> Response {
        let key_count = request.keys.len();
        let literal_payload = literal_broadcast_payload(&request);
        let (prepared, mut failures) = {
            let mut state = self.state.lock().await;
            prepare_broadcast_writes(&mut state, &request, literal_payload.as_ref())
        };

        let mut successes = Vec::new();
        for prepared in prepared {
            match write_bytes_to_target_io(prepared.write, prepared.bytes.as_ref().to_vec()).await {
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

fn literal_broadcast_payload(request: &rmux_proto::PaneBroadcastInputRequest) -> Option<Arc<[u8]>> {
    request.literal.then(|| {
        Arc::from(
            request
                .keys
                .iter()
                .flat_map(|key| key.as_bytes().iter().copied())
                .collect::<Vec<_>>(),
        )
    })
}

fn prepare_broadcast_writes(
    state: &mut HandlerState,
    request: &rmux_proto::PaneBroadcastInputRequest,
    literal_payload: Option<&Arc<[u8]>>,
) -> (Vec<PreparedBroadcastWrite>, Vec<PaneBroadcastInputFailure>) {
    let mut prepared = Vec::new();
    let mut failures = Vec::new();

    for (target_index, target) in request.targets.iter().enumerate() {
        let target_index = u32::try_from(target_index).unwrap_or(u32::MAX);
        match prepare_one_broadcast_write(state, target_index, target, request, literal_payload) {
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
    literal_payload: Option<&Arc<[u8]>>,
) -> Result<PreparedBroadcastWrite, RmuxError> {
    let target = resolve_pane_target_ref(state, target)?;
    let bytes = broadcast_payload_for_target(state, &target, request, literal_payload)?;
    let pane_id = pane_id_for_target(state, &target);
    let write = prepare_pane_input_write(state, &target, &bytes, PaneInputLiveness::TolerateDead)?;

    Ok(PreparedBroadcastWrite {
        target_index,
        target,
        pane_id,
        write,
        bytes,
    })
}

fn broadcast_payload_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    request: &rmux_proto::PaneBroadcastInputRequest,
    literal_payload: Option<&Arc<[u8]>>,
) -> Result<Arc<[u8]>, RmuxError> {
    match literal_payload {
        Some(payload) => Ok(Arc::clone(payload)),
        None => Ok(Arc::from(encode_tokens_for_target(
            state,
            target,
            &request.keys,
        )?)),
    }
}

fn pane_id_for_target(state: &HandlerState, target: &PaneTarget) -> Option<PaneId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rmux_proto::{PaneBroadcastInputRequest, PaneTarget, SessionName};

    use super::{broadcast_payload_for_target, literal_broadcast_payload, HandlerState};

    #[test]
    fn duplicate_literal_targets_share_the_materialized_payload() {
        let request = PaneBroadcastInputRequest {
            targets: Vec::new(),
            keys: vec!["large literal".to_owned()],
            literal: true,
        };
        let literal_payload = literal_broadcast_payload(&request).expect("literal payload");
        let state = HandlerState::default();
        let target = PaneTarget::new(SessionName::new("shared").expect("session name"), 0);

        let first = broadcast_payload_for_target(&state, &target, &request, Some(&literal_payload))
            .expect("first payload");
        let second =
            broadcast_payload_for_target(&state, &target, &request, Some(&literal_payload))
                .expect("second payload");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.as_ref(), b"large literal");
    }
}
