use crate::handles::session::unexpected_response;
use crate::transport::TransportClient;
use crate::{InfoSnapshot, PaneId, PaneRef, Result, SessionInfo, WindowInfo};
use rmux_proto::{ListPanesRequest, Request, Response};

use super::parse::parse_pane_info_location_line;
use super::{
    current_session_info_for_id, current_window_entry_for_id, is_already_closed_error,
    list_session_entries, pane_info_snapshot_at_target, pane_info_snapshot_for_absent_slot,
    PaneInfoLocation, PaneInfoParentIdentity, PANE_ID_RESOLUTION_SWEEPS,
};

const PANE_INFO_LOCATION_FORMAT: &str =
    "#{window_index}\t#{pane_index}\t#{pane_id}\t#{session_id}\t#{window_id}";

pub(in crate::handles::pane) async fn pane_info_snapshot_for_slot(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<InfoSnapshot> {
    let Some(location) = current_pane_info_location_at_slot(client, target).await? else {
        return pane_info_snapshot_for_absent_slot(client, target).await;
    };
    pane_info_snapshot_for_id_inner(
        client,
        &target.session_name,
        location.pane_id,
        Some(location.parent),
    )
    .await
}

pub(in crate::handles::pane) async fn pane_info_snapshot_for_id(
    client: &TransportClient,
    preferred_session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<InfoSnapshot> {
    pane_info_snapshot_for_id_inner(client, preferred_session_name, pane_id, None).await
}

async fn pane_info_snapshot_for_id_inner(
    client: &TransportClient,
    preferred_session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    mut fallback_parent: Option<PaneInfoParentIdentity>,
) -> Result<InfoSnapshot> {
    for _ in 0..PANE_ID_RESOLUTION_SWEEPS {
        let Some((target, parent)) =
            current_pane_info_location_for_id(client, preferred_session_name, pane_id).await?
        else {
            return pane_info_fallback_snapshot(client, fallback_parent).await;
        };
        fallback_parent = Some(parent);
        if let Some(snapshot) = pane_info_snapshot_at_target(client, &target, pane_id).await? {
            return Ok(snapshot);
        }
    }
    pane_info_fallback_snapshot(client, fallback_parent).await
}

async fn pane_info_fallback_snapshot(
    client: &TransportClient,
    fallback_parent: Option<PaneInfoParentIdentity>,
) -> Result<InfoSnapshot> {
    match fallback_parent {
        Some(parent) => pane_info_snapshot_for_parent_identity(client, parent).await,
        None => Ok(InfoSnapshot::default()),
    }
}

async fn pane_info_snapshot_for_parent_identity(
    client: &TransportClient,
    parent: PaneInfoParentIdentity,
) -> Result<InfoSnapshot> {
    for _ in 0..PANE_ID_RESOLUTION_SWEEPS {
        let Some(session) = current_session_info_for_id(client, parent.session_id).await? else {
            return Ok(InfoSnapshot::default());
        };
        let Some(window) =
            current_window_entry_for_id(client, &session.name, parent.window_id).await?
        else {
            let session_is_current = current_session_info_for_id(client, parent.session_id)
                .await?
                .is_some_and(|current| current.name == session.name);
            if session_is_current {
                return Ok(InfoSnapshot::new(
                    vec![SessionInfo::new(session.id, session.name)],
                    Vec::new(),
                    Vec::new(),
                ));
            }
            continue;
        };
        let session_is_current = current_session_info_for_id(client, parent.session_id)
            .await?
            .is_some_and(|current| current.name == session.name);
        if !session_is_current {
            continue;
        }
        let window_info = WindowInfo {
            id: window.id,
            session_id: session.id,
            index: window.index,
            name: window.name,
            size: window.size,
            ..WindowInfo::new(window.id, session.id)
        };
        return Ok(InfoSnapshot::new(
            vec![SessionInfo::new(session.id, session.name)],
            vec![window_info],
            Vec::new(),
        ));
    }
    Ok(InfoSnapshot::default())
}

async fn current_pane_info_location_for_id(
    client: &TransportClient,
    preferred_session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<Option<(PaneRef, PaneInfoParentIdentity)>> {
    for sweep in 0..PANE_ID_RESOLUTION_SWEEPS {
        if let Some(location) =
            current_pane_info_location_for_id_in_session(client, preferred_session_name, pane_id)
                .await?
        {
            return Ok(Some(location));
        }

        let mut sessions = list_session_entries(client).await?;
        if sweep > 0 {
            sessions.reverse();
        }
        for session in sessions {
            if &session.name == preferred_session_name {
                continue;
            }
            if let Some(location) =
                current_pane_info_location_for_id_in_session(client, &session.name, pane_id).await?
            {
                return Ok(Some(location));
            }
        }
    }
    Ok(None)
}

async fn current_pane_info_location_for_id_in_session(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<Option<(PaneRef, PaneInfoParentIdentity)>> {
    let mut locations = list_pane_info_locations(client, session_name, None).await?;
    locations.sort_by_key(|location| (location.window_index, location.pane_index));
    Ok(locations
        .into_iter()
        .find(|location| location.pane_id == pane_id)
        .map(|location| {
            (
                PaneRef::new(
                    session_name.clone(),
                    location.window_index,
                    location.pane_index,
                ),
                location.parent,
            )
        }))
}

async fn current_pane_info_location_at_slot(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Option<PaneInfoLocation>> {
    Ok(
        list_pane_info_locations(client, &target.session_name, Some(target.window_index))
            .await?
            .into_iter()
            .find(|location| {
                location.window_index == target.window_index
                    && location.pane_index == target.pane_index
            }),
    )
}

async fn list_pane_info_locations(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
    target_window_index: Option<u32>,
) -> Result<Vec<PaneInfoLocation>> {
    let missing_target = PaneRef::new(
        session_name.clone(),
        target_window_index.unwrap_or_default(),
        0,
    );
    let response = client
        .request(Request::ListPanes(Box::new(ListPanesRequest {
            target: session_name.clone(),
            target_window_index,
            format: Some(PANE_INFO_LOCATION_FORMAT.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;

    let output = match response {
        Ok(Response::ListPanes(response)) => response.output.stdout,
        Ok(response) => return Err(unexpected_response("list-panes", response)),
        Err(error) if is_already_closed_error(&error, &missing_target) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };

    String::from_utf8_lossy(&output)
        .lines()
        .map(parse_pane_info_location_line)
        .collect()
}
