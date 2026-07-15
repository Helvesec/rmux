use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::handles::session::unexpected_response;
use crate::transport::TransportClient;
use crate::{
    InfoSnapshot, PaneExitState, PaneId, PaneInfo, PaneProcessState, PaneRef, Result, RmuxError,
    SessionId, SessionInfo, TerminalSizeSpec, WindowId, WindowInfo,
};
use rmux_proto::{ListPanesRequest, ListSessionsRequest, ListWindowsRequest, Request, Response};

use super::target::{is_already_closed_error, parse_error};

#[path = "info/parse.rs"]
mod parse;

pub(super) use parse::parse_details_line;
use parse::{parse_pane_id, parse_pane_list_line, parse_session_line, parse_window_id};

const SESSION_INFO_FORMAT: &str = "#{session_name}\t#{session_id}";
const PANE_LIST_FORMAT: &str = "#{window_index}:#{pane_index}:#{pane_id}";
// A stable pane can move from a session already scanned to one not yet scanned.
// A second, reverse-order sweep closes that overlap without allowing an
// unbounded lookup loop when the pane is genuinely gone or keeps moving.
const PANE_ID_RESOLUTION_SWEEPS: usize = 2;
const PANE_INFO_FORMAT: &str =
    "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\
     \t#{pane_width}\t#{pane_height}\t#{cursor_x}\t#{cursor_y}\t#{cursor_flag}\
     \t#{cursor_shape}\t#{history_bytes}\t#{history_size}\t#{pane_start_command}\
     \t#{pane_lifecycle_generation}\t#{pane_lifecycle_revision}\t#{pane_output_sequence}\
     \t#{pane_start_path}";
const PANE_TITLE_FORMAT: &str = "#{pane_id}\t#{pane_title}";

#[derive(Debug, Clone)]
pub(super) struct ListedPane {
    pub(super) window_index: u32,
    pub(super) pane_index: u32,
    pub(super) pane_id: PaneId,
}

#[derive(Debug, Clone)]
struct ListedSession {
    name: rmux_proto::SessionName,
    id: SessionId,
}

#[derive(Debug, Clone)]
struct ListedWindow {
    index: u32,
    id: WindowId,
    name: Option<String>,
    size: TerminalSizeSpec,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LiveDetails {
    pub(super) pane_id: Option<PaneId>,
    pub(super) pid: Option<u32>,
    pub(super) dead: bool,
    pub(super) dead_status: Option<i32>,
    pub(super) dead_signal: Option<i32>,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) cursor_x: u16,
    pub(super) cursor_y: u16,
    pub(super) cursor_visible: bool,
    pub(super) cursor_style: u32,
    pub(super) history_bytes: u64,
    pub(super) history_size: u64,
    pub(super) start_command: Option<Vec<String>>,
    pub(super) generation: u64,
    pub(super) lifecycle_revision: u64,
    pub(super) output_sequence: u64,
    pub(super) current_path: Option<String>,
}

pub(super) async fn pane_info_snapshot(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<InfoSnapshot> {
    let session = match current_session_info(client, &target.session_name).await? {
        Some(session) => session,
        None => return Ok(InfoSnapshot::default()),
    };
    let session_id = session.id;

    let window_entry = current_window_entry(client, target).await?;
    let Some(window) = window_entry else {
        return Ok(InfoSnapshot::new(
            vec![SessionInfo::new(session_id, session.name.clone())],
            Vec::new(),
            Vec::new(),
        ));
    };
    let window_info = WindowInfo {
        id: window.id,
        session_id,
        index: window.index,
        name: window.name.clone(),
        size: window.size,
        ..WindowInfo::new(window.id, session_id)
    };

    let pane_entry = current_pane_entry(client, target).await?;
    let Some(pane) = pane_entry else {
        return Ok(InfoSnapshot::new(
            vec![SessionInfo::new(session_id, session.name.clone())],
            vec![window_info],
            Vec::new(),
        ));
    };

    let details = match fetch_live_details_by_id(client, &target.session_name, pane.pane_id).await {
        Ok(details) => details,
        Err(error) if is_already_closed_error(&error, target) => LiveDetails::default(),
        Err(error) => return Err(error),
    };
    let mut pane_info = PaneInfo::new(pane.pane_id, window.id, session_id);
    pane_info.index = target.pane_index;
    pane_info.size = pane_size_from_details(&details, &window.size);
    pane_info.process = derive_process_state(&details);
    pane_info.exit_state = derive_exit_state(&details);
    pane_info.command = details.start_command.clone();
    pane_info.working_directory = details.current_path.clone();
    pane_info.generation = details.generation;
    pane_info.revision = if details.lifecycle_revision == 0 {
        revision_from_details(&details)
    } else {
        details.lifecycle_revision
    };
    pane_info.output_sequence = details.output_sequence;

    Ok(InfoSnapshot::new(
        vec![SessionInfo::new(session_id, session.name.clone())],
        vec![window_info],
        vec![pane_info],
    ))
}

pub(super) fn pane_size_from_details(
    details: &LiveDetails,
    fallback: &TerminalSizeSpec,
) -> TerminalSizeSpec {
    if details.cols == 0 && details.rows == 0 {
        // A zero size here means the detail probe yielded no usable pane
        // dimensions (for example, the pane vanished after list-panes saw it).
        // Preserve the already-listed parent window size rather than
        // publishing a synthetic 0x0 pane in the sticky info snapshot.
        *fallback
    } else {
        TerminalSizeSpec::new(details.cols, details.rows)
    }
}

pub(super) fn derive_process_state(details: &LiveDetails) -> PaneProcessState {
    if details.dead {
        PaneProcessState::Exited
    } else if let Some(pid) = details.pid {
        PaneProcessState::Running { pid: Some(pid) }
    } else {
        PaneProcessState::Unknown
    }
}

pub(super) fn derive_exit_state(details: &LiveDetails) -> Option<PaneExitState> {
    if !details.dead {
        return None;
    }
    Some(PaneExitState {
        code: details.dead_status,
        signal: details.dead_signal.filter(|signal| *signal != 0),
        message: None,
    })
}

pub(super) fn revision_from_details(details: &LiveDetails) -> u64 {
    let mut hasher = DefaultHasher::new();
    details.pane_id.hash(&mut hasher);
    details.dead.hash(&mut hasher);
    details.dead_status.hash(&mut hasher);
    details.dead_signal.hash(&mut hasher);
    details.history_bytes.hash(&mut hasher);
    details.history_size.hash(&mut hasher);
    details.start_command.hash(&mut hasher);
    details.generation.hash(&mut hasher);
    details.lifecycle_revision.hash(&mut hasher);
    details.output_sequence.hash(&mut hasher);
    details.cols.hash(&mut hasher);
    details.rows.hash(&mut hasher);
    details.cursor_x.hash(&mut hasher);
    details.cursor_y.hash(&mut hasher);
    let raw = hasher.finish();
    if raw == 0 {
        1
    } else {
        raw
    }
}

async fn current_session_info(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
) -> Result<Option<ListedSession>> {
    Ok(list_session_entries(client)
        .await?
        .into_iter()
        .find(|session| &session.name == session_name))
}

async fn list_session_entries(client: &TransportClient) -> Result<Vec<ListedSession>> {
    let response = client
        .request(Request::ListSessions(ListSessionsRequest {
            format: Some(SESSION_INFO_FORMAT.to_owned()),
            filter: None,
            sort_order: Some("name".to_owned()),
            reversed: false,
        }))
        .await?;

    let output = match response {
        Response::ListSessions(response) => response.output.stdout,
        response => return Err(unexpected_response("list-sessions", response)),
    };

    let mut sessions = String::from_utf8_lossy(&output)
        .lines()
        .map(parse_session_line)
        .collect::<Result<Vec<_>>>()?;
    sessions.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    sessions.dedup_by(|left, right| left.name == right.name);
    Ok(sessions)
}

async fn current_window_entry(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Option<ListedWindow>> {
    match list_window_entries(client, &target.session_name).await {
        Ok(entries) => Ok(entries
            .into_iter()
            .find(|entry| entry.index == target.window_index)),
        Err(error) if is_already_closed_error(&error, target) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn list_window_entries(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
) -> Result<Vec<ListedWindow>> {
    match client
        .request(Request::ListWindows(Box::new(ListWindowsRequest {
            target: session_name.clone(),
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await?
    {
        Response::ListWindows(response) => response
            .windows
            .into_iter()
            .map(|entry| {
                Ok(ListedWindow {
                    index: entry.target.window_index(),
                    id: parse_window_id(&entry.window_id)?,
                    name: entry.name,
                    size: entry.size.into(),
                })
            })
            .collect(),
        response => Err(unexpected_response("list-windows", response)),
    }
}

pub(super) async fn current_pane_entry(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Option<ListedPane>> {
    match list_window_pane_entries(client, target).await {
        Ok(entries) => Ok(entries.into_iter().find(|entry| {
            entry.window_index == target.window_index && entry.pane_index == target.pane_index
        })),
        Err(error) if is_already_closed_error(&error, target) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) async fn current_pane_ref_for_id(
    client: &TransportClient,
    preferred_session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneRef>> {
    for sweep in 0..PANE_ID_RESOLUTION_SWEEPS {
        // Preserve the originating alias whenever the pane is visible there,
        // including when it appears between the two bounded sweeps.
        if let Some(target) =
            current_pane_ref_for_id_in_session(client, preferred_session_name, pane_id).await?
        {
            return Ok(Some(target));
        }

        // Refresh the inventory on the retry so a move into a newly created
        // session is visible. Reverse the second sweep to revisit sessions
        // that may have received a pane after the first sweep passed them.
        let mut sessions = list_session_entries(client).await?;
        if sweep > 0 {
            sessions.reverse();
        }
        for session in sessions {
            if &session.name == preferred_session_name {
                continue;
            }
            if let Some(target) =
                current_pane_ref_for_id_in_session(client, &session.name, pane_id).await?
            {
                return Ok(Some(target));
            }
        }
    }
    Ok(None)
}

async fn current_pane_ref_for_id_in_session(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneRef>> {
    let target = PaneRef::new(session_name.clone(), 0, 0);
    match list_all_pane_entries(client, &target).await {
        Ok(mut entries) => {
            entries.sort_by_key(|entry| (entry.window_index, entry.pane_index));
            Ok(entries
                .into_iter()
                .find(|entry| entry.pane_id == pane_id)
                .map(|entry| {
                    PaneRef::new(session_name.clone(), entry.window_index, entry.pane_index)
                }))
        }
        Err(error) if is_already_closed_error(&error, &target) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn list_window_pane_entries(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Vec<ListedPane>> {
    list_pane_entries(client, target, Some(target.window_index)).await
}

async fn list_all_pane_entries(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Vec<ListedPane>> {
    list_pane_entries(client, target, None).await
}

async fn list_pane_entries(
    client: &TransportClient,
    target: &PaneRef,
    target_window_index: Option<u32>,
) -> Result<Vec<ListedPane>> {
    let response = client
        .request(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name.clone(),
            target_window_index,
            format: Some(PANE_LIST_FORMAT.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await?;

    let output = match response {
        Response::ListPanes(response) => response.output.stdout,
        response => return Err(unexpected_response("list-panes", response)),
    };

    String::from_utf8_lossy(&output)
        .lines()
        .map(parse_pane_list_line)
        .collect()
}

async fn fetch_live_details_by_id(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<LiveDetails> {
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

    for line in String::from_utf8_lossy(&output).lines() {
        let details = parse_details_line(line)?;
        if details.pane_id == Some(pane_id) {
            return Ok(details);
        }
    }
    Ok(LiveDetails::default())
}

pub(super) async fn pane_title_for_id(
    client: &TransportClient,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Result<Option<String>> {
    let response = client
        .request(Request::ListPanes(Box::new(ListPanesRequest {
            target: session_name.clone(),
            target_window_index: None,
            format: Some(PANE_TITLE_FORMAT.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))
        .await;
    let output = match response {
        Ok(Response::ListPanes(response)) => response.output.stdout,
        Ok(response) => return Err(unexpected_response("list-panes", response)),
        Err(RmuxError::Protocol {
            source: rmux_proto::RmuxError::SessionNotFound(missing),
        }) if missing == session_name.as_str() => return Ok(None),
        Err(error) => return Err(error),
    };

    for line in String::from_utf8_lossy(&output).lines() {
        let Some((raw_id, title)) = line.split_once('\t') else {
            return Err(parse_error("pane title line omitted title separator"));
        };
        if parse_pane_id(raw_id)? == pane_id {
            return Ok(Some(title.to_owned()));
        }
    }
    Ok(None)
}
