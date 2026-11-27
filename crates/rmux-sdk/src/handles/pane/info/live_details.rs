//! Mapping of the 18-field live details probe onto `PaneInfo`, shared by the
//! pane-scoped snapshot and the window-scoped decoration.

use std::collections::HashMap;

use crate::handles::session::unexpected_response;
use crate::transport::TransportClient;
use crate::{PaneId, PaneInfo, Result, TerminalSizeSpec};
use rmux_proto::{ListPanesRequest, Request, Response};

use super::{
    derive_exit_state, derive_process_state, pane_size_from_details, parse_details_line,
    revision_from_details, LiveDetails, PANE_INFO_FORMAT,
};

/// Publishes the live details probe into a pane info record; shared by the
/// pane-scoped and window-scoped snapshots so both derive identical state.
pub(super) fn fill_pane_info_from_details(
    info: &mut PaneInfo,
    details: &LiveDetails,
    window_size: &TerminalSizeSpec,
) {
    info.size = pane_size_from_details(details, window_size);
    info.process = derive_process_state(details);
    info.exit_state = derive_exit_state(details);
    info.command = details.start_command.clone();
    info.working_directory = details.current_path.clone();
    info.generation = details.generation;
    info.revision = if details.lifecycle_revision == 0 {
        revision_from_details(details)
    } else {
        details.lifecycle_revision
    };
    info.output_sequence = details.output_sequence;
}

/// Live details for every pane in a session, keyed by pane id — one
/// `list-panes` probe with the same [`PANE_INFO_FORMAT`] the pane-scoped
/// snapshot uses, so callers can decorate a whole window's panes without
/// one round-trip per pane.
async fn fetch_session_live_details(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
) -> Result<HashMap<PaneId, LiveDetails>> {
    let response = client
        .request(Request::ListPanes(Box::new(ListPanesRequest {
            target: session_name.clone(),
            target_window_index: None,
            format: Some(PANE_INFO_FORMAT.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await?;

    let output = match response {
        Response::ListPanes(response) => response.output.stdout,
        response => return Err(unexpected_response("list-panes", response)),
    };

    let mut details = HashMap::new();
    for line in String::from_utf8_lossy(&output).lines() {
        let parsed = parse_details_line(line)?;
        if let Some(pane_id) = parsed.pane_id {
            details.insert(pane_id, parsed);
        }
    }
    Ok(details)
}

/// Fill window-level [`PaneInfo`]s with the live per-pane details the
/// pane-scoped snapshot serves, through the same field mapping: process/exit
/// state, start command, working directory, geometry, and lifecycle counters.
/// Panes the probe could not resolve are left untouched (their process state
/// stays `Unknown`), so a pane that vanished mid-snapshot degrades exactly the
/// way the pane-scoped path's absent-slot snapshot does.
pub(in crate::handles) async fn apply_live_details(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
    panes: &mut [PaneInfo],
    window_size: TerminalSizeSpec,
) -> Result<()> {
    let details = fetch_session_live_details(client, session_name).await?;
    for info in panes.iter_mut() {
        let Some(live) = details.get(&info.id) else {
            continue;
        };
        fill_pane_info_from_details(info, live, &window_size);
    }
    Ok(())
}
