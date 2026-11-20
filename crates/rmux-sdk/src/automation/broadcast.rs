use rmux_proto::PaneBroadcastInputResponse;

use crate::{Result, RmuxError};

pub fn validate_broadcast_response(
    response: &PaneBroadcastInputResponse,
    targets: usize,
) -> Result<()> {
    let mut seen = vec![false; targets];
    for (index, outcome) in response
        .successes
        .iter()
        .map(|item| (item.target_index, "successful"))
        .chain(
            response
                .failures
                .iter()
                .map(|item| (item.target_index, "failed")),
        )
    {
        let entry = usize::try_from(index)
            .ok()
            .and_then(|index| seen.get_mut(index))
            .ok_or_else(|| {
                protocol_error(format!(
                "rmux daemon returned out-of-range {outcome} pane broadcast target index {index}"
            ))
            })?;
        if std::mem::replace(entry, true) {
            return Err(protocol_error(format!(
                "rmux daemon returned duplicate pane broadcast outcome for target index {index}"
            )));
        }
    }
    if let Some(index) = seen.iter().position(|seen| !seen) {
        return Err(protocol_error(format!(
            "rmux daemon omitted pane broadcast outcome for target index {index}"
        )));
    }
    Ok(())
}

fn protocol_error(message: String) -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::Server(message))
}
