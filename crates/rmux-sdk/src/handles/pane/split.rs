//! Implementation of [`Pane::split`].
//!
//! Kept in its own partial so `pane.rs` stays close to its public surface
//! while the wire-level RPC details — request shape, response decoding,
//! error mapping — live next to the other lifecycle helpers.

use crate::handles::session::unexpected_response;
use crate::handles::split::SplitDirection;
use crate::transport::TransportClient;
use std::path::PathBuf;

use crate::{PaneId, PaneRef, ProcessSpec, Result};
use rmux_proto::{
    Request, Response, SplitWindowIdentityRequest, SplitWindowTargetActionRequest,
    CAPABILITY_SDK_PANE_SPLIT_IDENTITY, CAPABILITY_SDK_PROCESS_COMMAND,
};

pub(super) struct SplitPaneOutcome {
    pub(super) target: PaneRef,
    pub(super) pane_id: PaneId,
}

/// Issues the `split-window` request that backs [`Pane::split`].
///
/// The returned target and stable id address the freshly spawned pane.
pub(super) async fn split_pane(
    client: &TransportClient,
    target: String,
    direction: SplitDirection,
) -> Result<SplitPaneOutcome> {
    split_pane_with_process(
        client,
        target,
        direction,
        ProcessSpec::default(),
        None,
        None,
    )
    .await
}

pub(super) async fn split_pane_with_process(
    client: &TransportClient,
    target: String,
    direction: SplitDirection,
    process: ProcessSpec,
    cwd: Option<PathBuf>,
    keep_alive_on_exit: Option<bool>,
) -> Result<SplitPaneOutcome> {
    let (command, process_command, environment) = process.into_proto_parts();
    let mut required_capabilities = vec![CAPABILITY_SDK_PANE_SPLIT_IDENTITY];
    if process_command.is_some() {
        required_capabilities.push(CAPABILITY_SDK_PROCESS_COMMAND);
    }
    crate::capabilities::require(client, &required_capabilities).await?;
    let response = match client
        .request(Request::SplitWindowIdentity(Box::new(
            SplitWindowIdentityRequest {
                action: SplitWindowTargetActionRequest {
                    target: Some(target),
                    direction: direction.axis(),
                    before: direction.before(),
                    environment,
                    command,
                    process_command,
                    start_directory: cwd,
                    keep_alive_on_exit,
                    detached: false,
                    size: None,
                    preserve_zoom: false,
                    full_size: false,
                    stdin_payload: None,
                },
            },
        )))
        .await?
    {
        Response::SplitWindowIdentity(response) => response,
        response => return Err(unexpected_response("split-window", response)),
    };

    Ok(SplitPaneOutcome {
        target: PaneRef::new(
            response.pane.session_name().clone(),
            response.pane.window_index(),
            response.pane.pane_index(),
        ),
        pane_id: response.pane_id,
    })
}
