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

pub(super) fn parse_details_line(line: &str) -> Result<LiveDetails> {
    if line.is_empty() {
        return Ok(LiveDetails::default());
    }
    // The trailing field is `#{pane_start_path}`, which is a filesystem
    // path. Tabs in such a path are valid bytes on Unix, so the parser
    // anchors the leading separators with `splitn` and treats the
    // remainder as the path verbatim instead of dropping characters past
    // an embedded tab.
    let fields: Vec<&str> = line.splitn(18, '\t').collect();
    if fields.len() < 18 {
        return Ok(LiveDetails::default());
    }

    Ok(LiveDetails {
        pane_id: parse_optional_pane_id(fields[0])?,
        pid: parse_optional_u32(fields[1]),
        dead: parse_truthy_flag(fields[2]),
        dead_status: parse_optional_i32(fields[3]),
        dead_signal: parse_optional_i32(fields[4]),
        cols: parse_optional_u16(fields[5]).unwrap_or(0),
        rows: parse_optional_u16(fields[6]).unwrap_or(0),
        cursor_x: parse_optional_u16(fields[7]).unwrap_or(0),
        cursor_y: parse_optional_u16(fields[8]).unwrap_or(0),
        cursor_visible: parse_truthy_flag_default(fields[9], true),
        cursor_style: parse_optional_u32(fields[10]).unwrap_or(0),
        history_bytes: parse_optional_u64(fields[11]).unwrap_or(0),
        history_size: parse_optional_u64(fields[12]).unwrap_or(0),
        start_command: decode_command_field(fields[13])?,
        generation: parse_optional_u64(fields[14]).unwrap_or(0),
        lifecycle_revision: parse_optional_u64(fields[15]).unwrap_or(0),
        output_sequence: parse_optional_u64(fields[16]).unwrap_or(0),
        current_path: optional_string(fields[17]),
    })
}

fn parse_session_line(line: &str) -> Result<ListedSession> {
    let mut fields = line.split('\t');
    let name = fields
        .next()
        .ok_or_else(|| parse_error("session info line omitted name"))?;
    let id = fields
        .next()
        .ok_or_else(|| parse_error("session info line omitted id"))?;
    if fields.next().is_some() {
        return Err(parse_error("session info line had trailing fields"));
    }
    Ok(ListedSession {
        name: rmux_proto::SessionName::new(name).map_err(RmuxError::protocol)?,
        id: parse_session_id(id)?,
    })
}

fn parse_pane_list_line(line: &str) -> Result<ListedPane> {
    let mut fields = line.split(':');
    let window_index = fields
        .next()
        .ok_or_else(|| parse_error("pane list line omitted window index"))?;
    let pane_index = fields
        .next()
        .ok_or_else(|| parse_error("pane list line omitted pane index"))?;
    let pane_id = fields
        .next()
        .ok_or_else(|| parse_error("pane list line omitted pane id"))?;
    if fields.next().is_some() {
        return Err(parse_error("pane list line had trailing fields"));
    }

    let window_index = parse_u32(window_index, "pane list window index")?;
    Ok(ListedPane {
        window_index,
        pane_index: parse_u32(pane_index, "pane index")?,
        pane_id: parse_pane_id(pane_id)?,
    })
}

fn parse_session_id(value: &str) -> Result<SessionId> {
    parse_prefixed_u32(value, '$', "session id").map(SessionId::new)
}

fn parse_window_id(value: &str) -> Result<WindowId> {
    parse_prefixed_u32(value, '@', "window id").map(WindowId::new)
}

fn parse_pane_id(value: &str) -> Result<PaneId> {
    parse_prefixed_u32(value, '%', "pane id").map(PaneId::new)
}

fn parse_optional_pane_id(value: &str) -> Result<Option<PaneId>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_pane_id(value).map(Some)
    }
}

fn parse_prefixed_u32(value: &str, prefix: char, field: &str) -> Result<u32> {
    let raw = value
        .strip_prefix(prefix)
        .ok_or_else(|| parse_error(format!("{field} `{value}` omitted `{prefix}` prefix")))?;
    parse_u32(raw, field)
}

fn parse_u32(value: &str, field: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|error| parse_error(format!("invalid {field} `{value}`: {error}")))
}

fn parse_truthy_flag(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

fn parse_truthy_flag_default(value: &str, default: bool) -> bool {
    if value.is_empty() {
        default
    } else {
        value != "0"
    }
}

fn parse_optional_u16(value: &str) -> Option<u16> {
    if value.is_empty() {
        None
    } else {
        value.parse::<u16>().ok()
    }
}

fn parse_optional_u32(value: &str) -> Option<u32> {
    if value.is_empty() {
        None
    } else {
        value.parse::<u32>().ok()
    }
}

fn parse_optional_u64(value: &str) -> Option<u64> {
    if value.is_empty() {
        None
    } else {
        value.parse::<u64>().ok()
    }
}

fn parse_optional_i32(value: &str) -> Option<i32> {
    if value.is_empty() {
        None
    } else {
        value.parse::<i32>().ok()
    }
}

fn optional_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn decode_command_field(value: &str) -> Result<Option<Vec<String>>> {
    if value.is_empty() {
        return Ok(None);
    }
    let mut command = value
        .split('\x1f')
        .map(percent_decode_string)
        .collect::<Result<Vec<_>>>()?;
    if command.len() == 1 {
        command[0] = unquote_tmux_shell_command(&command[0]);
    }
    Ok(Some(command))
}

fn unquote_tmux_shell_command(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return value.to_owned();
    };
    let mut unquoted = String::with_capacity(inner.len());
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            unquoted.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            unquoted.push(character);
        }
    }
    if escaped {
        unquoted.push('\\');
    }
    unquoted
}

fn percent_decode_string(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(parse_error("truncated percent escape in pane command"));
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|error| parse_error(format!("pane command was not utf-8: {error}")))
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(parse_error(format!(
            "invalid percent escape digit `{}` in pane command",
            char::from(byte)
        ))),
    }
}
