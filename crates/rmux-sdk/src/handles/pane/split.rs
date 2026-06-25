//! Implementation of [`Pane::split`].
//!
//! Kept in its own partial so `pane.rs` stays close to its public surface
//! while the wire-level RPC details — request shape, response decoding,
//! error mapping — live next to the other lifecycle helpers.

use crate::handles::session::unexpected_response;
use crate::handles::split::SplitDirection;
use crate::transport::TransportClient;
use std::path::PathBuf;

use crate::{PaneRef, ProcessSpec, Result};
use rmux_proto::{Request, Response, SplitWindowExtRequest, SplitWindowTarget};

/// Issues the `split-window` request that backs [`Pane::split`].
///
/// The returned [`PaneRef`] addresses the freshly spawned pane.
pub(super) async fn split_pane(
    client: &TransportClient,
    target: &PaneRef,
    direction: SplitDirection,
) -> Result<PaneRef> {
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
    target: &PaneRef,
    direction: SplitDirection,
    process: ProcessSpec,
    cwd: Option<PathBuf>,
    keep_alive_on_exit: Option<bool>,
) -> Result<PaneRef> {
    let (command, process_command, environment) = process.into_proto_parts();
    crate::capabilities::require_process_command_if_present(client, process_command.as_ref())
        .await?;
    match client
        .request(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(target.into()),
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
        })))
        .await?
    {
        Response::SplitWindow(response) => Ok(response.pane.into()),
        response => Err(unexpected_response("split-window", response)),
    }
}
