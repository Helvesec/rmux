//! Implementation of [`Pane::split`].
//!
//! Kept in its own partial so `pane.rs` stays close to its public surface
//! while the wire-level RPC details — request shape, response decoding,
//! error mapping — live next to the other lifecycle helpers.

use crate::handles::session::unexpected_response;
use crate::handles::split::SplitDirection;
use crate::transport::TransportClient;
use std::path::PathBuf;

use crate::{PaneId, PaneRef, ProcessSpec, Result, RmuxError};
use rmux_proto::{
    DisplayMessageRequest, Request, Response, SplitWindowTargetActionRequest, Target,
};

const SPLIT_RESULT_FORMAT: &str = "#{pane_index}\t#{pane_id}";

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
    crate::capabilities::require_process_command_if_present(client, process_command.as_ref())
        .await?;
    let response = match client
        .request(Request::SplitWindowTargetAction(Box::new(
            SplitWindowTargetActionRequest {
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
        )))
        .await?
    {
        Response::SplitWindow(response) => response,
        response => return Err(unexpected_response("split-window", response)),
    };

    normalize_split_result(client, response.pane).await
}

async fn normalize_split_result(
    client: &TransportClient,
    raw_target: rmux_proto::PaneTarget,
) -> Result<SplitPaneOutcome> {
    let response = client
        .request(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(raw_target.clone())),
            print: true,
            message: Some(SPLIT_RESULT_FORMAT.to_owned()),
            empty_target_context: false,
        }))
        .await?;
    let output = match response {
        Response::DisplayMessage(response) => response
            .output
            .ok_or_else(|| invalid_split_result("display-message returned no output"))?,
        response => return Err(unexpected_response("display-message", response)),
    };
    let rendered = std::str::from_utf8(output.stdout())
        .map_err(|error| invalid_split_result(format!("result was not UTF-8: {error}")))?;
    let rendered = rendered.strip_suffix('\n').unwrap_or(rendered);
    let (visible_index, pane_id) = rendered
        .split_once('\t')
        .ok_or_else(|| invalid_split_result("result omitted the pane identity separator"))?;
    if pane_id.contains(['\t', '\n']) {
        return Err(invalid_split_result("result contained trailing fields"));
    }
    let visible_index = visible_index.parse::<u32>().map_err(|error| {
        invalid_split_result(format!(
            "invalid visible pane index `{visible_index}`: {error}"
        ))
    })?;
    let pane_id = pane_id
        .strip_prefix('%')
        .ok_or_else(|| invalid_split_result(format!("pane id `{pane_id}` omitted `%` prefix")))?
        .parse::<u32>()
        .map(PaneId::new)
        .map_err(|error| invalid_split_result(format!("invalid pane id `{pane_id}`: {error}")))?;

    Ok(SplitPaneOutcome {
        target: PaneRef::new(
            raw_target.session_name().clone(),
            raw_target.window_index(),
            visible_index,
        ),
        pane_id,
    })
}

fn invalid_split_result(reason: impl Into<String>) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "invalid split-window result: {}",
        reason.into()
    )))
}
