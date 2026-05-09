//! Daemon-backed pane handle.
//!
//! The handle never caches a `PaneId`. Every operation re-reads the
//! daemon's current view of the addressed `(session, window, pane)` slot,
//! which is what keeps linked windows and grouped sessions returning the
//! same stable `%N` identity through every sibling view, and what makes
//! stale handles behave the same way as stale [`Window`](super::Window)
//! handles: the typed empty/`None` results carry the original target
//! verbatim instead of erroring out.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use crate::handles::session::unexpected_response;
use crate::transport::TransportClient;
use crate::{
    InfoSnapshot, PaneCell, PaneCursor, PaneExitState, PaneGlyph, PaneId, PaneInfo,
    PaneProcessState, PaneRef, PaneSnapshot, Result, RmuxEndpoint, RmuxError, SessionId,
    SessionInfo, TerminalSizeSpec, WindowId, WindowInfo,
};
use rmux_proto::{
    CapturePaneRequest, DisplayMessageRequest, ListPanesRequest, ListSessionsRequest,
    ListWindowsRequest, Request, ResizePaneAdjustment, ResizePaneRequest, Response,
    SendKeysExtRequest, SendKeysRequest, Target,
};

const SESSION_INFO_FORMAT: &str = "#{session_name}\t#{session_id}";
const PANE_LIST_FORMAT: &str = "#{window_index}:#{pane_index}:#{pane_id}";
const PANE_INFO_FORMAT: &str =
    "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\
     \t#{pane_width}\t#{pane_height}\t#{cursor_x}\t#{cursor_y}\t#{cursor_flag}\
     \t#{cursor_shape}\t#{history_bytes}\t#{history_size}\t#{pane_start_command}\
     \t#{pane_lifecycle_generation}\t#{pane_lifecycle_revision}\t#{pane_output_sequence}\
     \t#{pane_start_path}";

/// Opaque handle for one daemon pane slot.
///
/// A pane handle addresses a `(session, window, pane)` triple rather than
/// caching a `PaneId`. Every operation resolves that slot against the
/// daemon's current state, so:
///
/// * linked windows and grouped sessions keep returning the same stable
///   `%N` identity through every sibling view, and
/// * stale handles for an already-closed pane resolve to typed
///   `None`/empty results — never to a panic and never to a `PaneId` from
///   a prior epoch.
///
/// The handle deliberately exposes no `current_revision()` accessor.
/// Revision values are only observable through
/// [`PaneSnapshot::revision`] on a freshly captured snapshot, or through
/// the revision-carrying [`PaneEvent`](crate::PaneEvent) variants emitted
/// over a control-mode subscription.
#[derive(Clone)]
pub struct Pane {
    target: PaneRef,
    endpoint: RmuxEndpoint,
    default_timeout: Option<Duration>,
    transport: TransportClient,
}

impl Pane {
    pub(crate) fn new(
        target: PaneRef,
        endpoint: RmuxEndpoint,
        default_timeout: Option<Duration>,
        transport: TransportClient,
    ) -> Self {
        Self {
            target,
            endpoint,
            default_timeout,
            transport,
        }
    }

    /// Returns the exact protocol-owned pane target addressed by this
    /// handle.
    #[must_use]
    pub const fn target(&self) -> &PaneRef {
        &self.target
    }

    /// Returns the endpoint that was resolved when this handle was created.
    #[must_use]
    pub const fn endpoint(&self) -> &RmuxEndpoint {
        &self.endpoint
    }

    /// Returns the default timeout configured on the parent facade.
    #[must_use]
    pub const fn configured_default_timeout(&self) -> Option<Duration> {
        self.default_timeout
    }

    /// Returns the live daemon pane identity for this slot, when it is
    /// currently listed.
    ///
    /// Returns `Ok(None)` (rather than an error) for a stale slot, mirroring
    /// the [`Window`](super::Window)-handle stale-slot semantics.
    pub async fn id(&self) -> Result<Option<PaneId>> {
        Ok(current_pane_entry(&self.transport, &self.target)
            .await?
            .map(|entry| entry.pane_id))
    }

    /// Checks whether this exact pane slot is currently listed by the
    /// daemon.
    pub async fn exists(&self) -> Result<bool> {
        Ok(self.id().await?.is_some())
    }

    /// Returns a sticky info snapshot scoped to this pane's session,
    /// window, and pane.
    ///
    /// The snapshot is assembled from live `list-sessions`,
    /// `list-windows`, `list-panes`, and `display-message -p` responses so
    /// pane process state — running pid, exit state, geometry — reflects
    /// the daemon's current view rather than any handle-cached value.
    /// Stale slots return what is still observable: a session-only
    /// snapshot when the window or pane is gone, or an empty snapshot
    /// when the session itself is gone.
    pub async fn info(&self) -> Result<InfoSnapshot> {
        pane_info_snapshot(&self.transport, &self.target).await
    }

    /// Captures the live pane grid as a [`PaneSnapshot`].
    ///
    /// The captured grid mirrors the daemon's terminal/transcript state for
    /// this pane: dimensions come from the live `pane_width`/`pane_height`
    /// fields, the cursor row/col/visibility/style come from the live
    /// `cursor_*` fields, and the row-major cells come from a
    /// `capture-pane -p` of the visible viewport. The snapshot's
    /// [`revision`](PaneSnapshot::revision) is derived from the captured
    /// pane state and changes whenever the daemon's view of the pane
    /// mutates — output, resize, clear, exit. Stale slots resolve to a
    /// default empty snapshot whose revision is `0`, distinct from any
    /// prior live revision.
    pub async fn snapshot(&self) -> Result<PaneSnapshot> {
        pane_snapshot(&self.transport, &self.target).await
    }

    /// Sends literal UTF-8 text bytes to this pane through the daemon.
    ///
    /// The payload is not interpreted as key names, does not expand tmux
    /// formats, and does not receive an implicit trailing newline. Use
    /// [`send_key`](Self::send_key) when a tmux key token such as `Enter`
    /// should be interpreted as a key press.
    pub async fn send_text(&self, text: impl AsRef<str>) -> Result<()> {
        send_text(&self.transport, &self.target, text.as_ref()).await
    }

    /// Sends one tmux-compatible key token to this pane through the daemon.
    ///
    /// Tokens keep the daemon's existing `send-keys` semantics: known key
    /// names such as `Enter` are encoded as keys, while ordinary text tokens
    /// are forwarded as their bytes by the server.
    pub async fn send_key(&self, key: impl Into<String>) -> Result<()> {
        send_key(&self.transport, &self.target, key.into()).await
    }

    /// Requests an absolute pane size through the daemon.
    ///
    /// Only dimensions that differ from the daemon's current pane details are
    /// sent. The daemon still applies normal `resize-pane` layout rules, so
    /// linked panes, borders, and neighboring panes can constrain the final
    /// geometry. No pane identity is cached by this handle.
    pub async fn resize(&self, size: TerminalSizeSpec) -> Result<()> {
        resize_to_size(&self.transport, &self.target, size).await
    }
}

impl fmt::Debug for Pane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pane")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct ListedPane {
    pane_index: u32,
    pane_id: PaneId,
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
struct LiveDetails {
    pane_id: Option<PaneId>,
    pid: Option<u32>,
    dead: bool,
    dead_status: Option<i32>,
    dead_signal: Option<i32>,
    cols: u16,
    rows: u16,
    cursor_x: u16,
    cursor_y: u16,
    cursor_visible: bool,
    cursor_style: u32,
    history_bytes: u64,
    history_size: u64,
    start_command: Option<Vec<String>>,
    generation: u64,
    lifecycle_revision: u64,
    output_sequence: u64,
    current_path: Option<String>,
}

async fn pane_info_snapshot(client: &TransportClient, target: &PaneRef) -> Result<InfoSnapshot> {
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

    let details = fetch_live_details_or_default(client, target).await?;
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

fn pane_size_from_details(details: &LiveDetails, fallback: &TerminalSizeSpec) -> TerminalSizeSpec {
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

fn derive_process_state(details: &LiveDetails) -> PaneProcessState {
    if details.dead {
        PaneProcessState::Exited
    } else if let Some(pid) = details.pid {
        PaneProcessState::Running { pid: Some(pid) }
    } else {
        PaneProcessState::Unknown
    }
}

fn derive_exit_state(details: &LiveDetails) -> Option<PaneExitState> {
    if !details.dead {
        return None;
    }
    Some(PaneExitState {
        code: details.dead_status,
        signal: details.dead_signal.filter(|signal| *signal != 0),
        message: None,
    })
}

async fn pane_snapshot(client: &TransportClient, target: &PaneRef) -> Result<PaneSnapshot> {
    if current_pane_entry(client, target).await?.is_none() {
        return Ok(PaneSnapshot::default());
    }

    // The pane was listed at the start of this call, but the daemon can still
    // close it between the existence check and the capture. Treat the
    // already-closed protocol errors emitted in that window as a "vanished
    // mid-snapshot" signal and degrade to a default snapshot, while genuine
    // transport or protocol errors still propagate.
    let details = fetch_live_details_or_default(client, target).await?;
    let captured = match capture_pane_bytes_or_already_closed(client, target).await? {
        Some(captured) => captured,
        None => return Ok(PaneSnapshot::default()),
    };
    Ok(build_snapshot(&details, &captured))
}

async fn send_text(client: &TransportClient, target: &PaneRef, text: &str) -> Result<()> {
    let response = client
        .request(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.into()),
            keys: vec![text.to_owned()],
            expand_formats: false,
            hex: false,
            literal: true,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await?;

    match response {
        Response::SendKeys(_) => Ok(()),
        response => Err(unexpected_response("send-keys", response)),
    }
}

async fn send_key(client: &TransportClient, target: &PaneRef, key: String) -> Result<()> {
    let response = client
        .request(Request::SendKeys(SendKeysRequest {
            target: target.into(),
            keys: vec![key],
        }))
        .await?;

    match response {
        Response::SendKeys(_) => Ok(()),
        response => Err(unexpected_response("send-keys", response)),
    }
}

async fn resize_to_size(
    client: &TransportClient,
    target: &PaneRef,
    requested: TerminalSizeSpec,
) -> Result<()> {
    let current = live_pane_size(client, target).await?;
    let mut sent_non_noop_adjustment = false;

    if current.cols != requested.cols {
        request_resize_pane(
            client,
            target,
            ResizePaneAdjustment::AbsoluteWidth {
                columns: requested.cols,
            },
        )
        .await?;
        sent_non_noop_adjustment = true;
    }

    if current.rows != requested.rows {
        request_resize_pane(
            client,
            target,
            ResizePaneAdjustment::AbsoluteHeight {
                rows: requested.rows,
            },
        )
        .await?;
        sent_non_noop_adjustment = true;
    }

    if !sent_non_noop_adjustment {
        request_resize_pane(client, target, ResizePaneAdjustment::NoOp).await?;
    }

    Ok(())
}

async fn live_pane_size(client: &TransportClient, target: &PaneRef) -> Result<TerminalSizeSpec> {
    let details = fetch_live_details_or_default(client, target).await?;
    Ok(TerminalSizeSpec::new(details.cols, details.rows))
}

async fn request_resize_pane(
    client: &TransportClient,
    target: &PaneRef,
    adjustment: ResizePaneAdjustment,
) -> Result<()> {
    let response = client
        .request(Request::ResizePane(ResizePaneRequest {
            target: target.into(),
            adjustment,
        }))
        .await?;

    match response {
        Response::ResizePane(_) => Ok(()),
        response => Err(unexpected_response("resize-pane", response)),
    }
}

fn build_snapshot(details: &LiveDetails, captured: &[u8]) -> PaneSnapshot {
    let cols = details.cols;
    let rows = details.rows;
    let cursor = PaneCursor::new(
        details.cursor_y,
        details.cursor_x,
        details.cursor_visible,
        details.cursor_style,
    );
    let cells = build_cells(captured, cols, rows);
    let snapshot = PaneSnapshot {
        cols,
        rows,
        cells,
        cursor,
        revision: 0,
    };
    let revision = compute_revision(&snapshot, details, captured);
    snapshot.with_revision(revision)
}

fn build_cells(captured: &[u8], cols: u16, rows: u16) -> Vec<PaneCell> {
    let cols_usize = usize::from(cols);
    let rows_usize = usize::from(rows);
    let total = cols_usize.saturating_mul(rows_usize);
    let mut cells = Vec::with_capacity(total);
    if cols_usize == 0 || rows_usize == 0 {
        return cells;
    }

    let text = String::from_utf8_lossy(captured);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    for row in 0..rows_usize {
        let raw_line = lines.get(row).copied().unwrap_or("");
        let mut row_cells = Vec::with_capacity(cols_usize);
        for ch in raw_line.chars().filter(|character| *character != '\r') {
            if row_cells.len() == cols_usize {
                break;
            }
            row_cells.push(PaneCell::new(PaneGlyph::new(ch.to_string(), 1)));
        }
        while row_cells.len() < cols_usize {
            row_cells.push(PaneCell::blank());
        }
        cells.extend(row_cells);
    }
    cells
}

fn compute_revision(snapshot: &PaneSnapshot, details: &LiveDetails, captured: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    snapshot.cols.hash(&mut hasher);
    snapshot.rows.hash(&mut hasher);
    snapshot.cursor.row.hash(&mut hasher);
    snapshot.cursor.col.hash(&mut hasher);
    snapshot.cursor.visible.hash(&mut hasher);
    snapshot.cursor.style.hash(&mut hasher);
    captured.hash(&mut hasher);
    details.pane_id.hash(&mut hasher);
    details.dead.hash(&mut hasher);
    details.dead_status.hash(&mut hasher);
    details.dead_signal.hash(&mut hasher);
    details.history_bytes.hash(&mut hasher);
    details.history_size.hash(&mut hasher);
    let raw = hasher.finish();
    if raw == 0 {
        0xFFFF_FFFF_FFFF_FFFF
    } else {
        raw
    }
}

fn revision_from_details(details: &LiveDetails) -> u64 {
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

    for line in String::from_utf8_lossy(&output).lines() {
        let session = parse_session_line(line)?;
        if &session.name == session_name {
            return Ok(Some(session));
        }
    }

    Ok(None)
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
        .request(Request::ListWindows(ListWindowsRequest {
            target: session_name.clone(),
            format: None,
        }))
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

async fn current_pane_entry(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Option<ListedPane>> {
    match list_pane_entries(client, target).await {
        Ok(entries) => Ok(entries
            .into_iter()
            .find(|entry| entry.pane_index == target.pane_index)),
        Err(error) if is_already_closed_error(&error, target) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn list_pane_entries(client: &TransportClient, target: &PaneRef) -> Result<Vec<ListedPane>> {
    let response = client
        .request(Request::ListPanes(ListPanesRequest {
            target: target.session_name.clone(),
            target_window_index: Some(target.window_index),
            format: Some(PANE_LIST_FORMAT.to_owned()),
        }))
        .await?;

    let output = match response {
        Response::ListPanes(response) => response.output.stdout,
        response => return Err(unexpected_response("list-panes", response)),
    };

    String::from_utf8_lossy(&output)
        .lines()
        .map(|line| parse_pane_list_line(target, line))
        .collect()
}

async fn fetch_live_details_or_default(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<LiveDetails> {
    match fetch_live_details(client, target).await {
        Ok(details) => Ok(details),
        Err(error) if is_already_closed_error(&error, target) => Ok(LiveDetails::default()),
        Err(error) => Err(error),
    }
}

async fn fetch_live_details(client: &TransportClient, target: &PaneRef) -> Result<LiveDetails> {
    let response = client
        .request(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target.into())),
            print: true,
            message: Some(PANE_INFO_FORMAT.to_owned()),
        }))
        .await?;

    let output = match response {
        Response::DisplayMessage(response) => response.output,
        response => return Err(unexpected_response("display-message", response)),
    };

    let bytes = output.map(|out| out.stdout).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let line = text.lines().next().unwrap_or("");
    parse_details_line(line)
}

fn parse_details_line(line: &str) -> Result<LiveDetails> {
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

    let pane_id = parse_optional_pane_id(fields[0])?;
    let pid = parse_optional_u32(fields[1]);
    let dead = parse_truthy_flag(fields[2]);
    let dead_status = parse_optional_i32(fields[3]);
    let dead_signal = parse_optional_i32(fields[4]);
    let cols = parse_optional_u16(fields[5]).unwrap_or(0);
    let rows = parse_optional_u16(fields[6]).unwrap_or(0);
    let cursor_x = parse_optional_u16(fields[7]).unwrap_or(0);
    let cursor_y = parse_optional_u16(fields[8]).unwrap_or(0);
    let cursor_visible = parse_truthy_flag_default(fields[9], true);
    let cursor_style = parse_optional_u32(fields[10]).unwrap_or(0);
    let history_bytes = parse_optional_u64(fields[11]).unwrap_or(0);
    let history_size = parse_optional_u64(fields[12]).unwrap_or(0);
    let start_command = decode_command_field(fields[13])?;
    let generation = parse_optional_u64(fields[14]).unwrap_or(0);
    let lifecycle_revision = parse_optional_u64(fields[15]).unwrap_or(0);
    let output_sequence = parse_optional_u64(fields[16]).unwrap_or(0);
    let current_path = optional_string(fields[17]);

    Ok(LiveDetails {
        pane_id,
        pid,
        dead,
        dead_status,
        dead_signal,
        cols,
        rows,
        cursor_x,
        cursor_y,
        cursor_visible,
        cursor_style,
        history_bytes,
        history_size,
        start_command,
        generation,
        lifecycle_revision,
        output_sequence,
        current_path,
    })
}

async fn capture_pane_bytes_or_already_closed(
    client: &TransportClient,
    target: &PaneRef,
) -> Result<Option<Vec<u8>>> {
    match capture_pane_bytes(client, target).await {
        Ok(captured) => Ok(Some(captured)),
        Err(error) if is_already_closed_error(&error, target) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn capture_pane_bytes(client: &TransportClient, target: &PaneRef) -> Result<Vec<u8>> {
    let response = client
        .request(Request::CapturePane(CapturePaneRequest {
            target: target.into(),
            start: None,
            end: None,
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: true,
            do_not_trim_spaces: true,
            pending_input: false,
            quiet: true,
            start_is_absolute: false,
            end_is_absolute: false,
        }))
        .await?;

    match response {
        Response::CapturePane(response) => {
            Ok(response.output.map(|out| out.stdout).unwrap_or_default())
        }
        response => Err(unexpected_response("capture-pane", response)),
    }
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

fn parse_pane_list_line(target: &PaneRef, line: &str) -> Result<ListedPane> {
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
    if window_index != target.window_index {
        return Err(parse_error(format!(
            "list-panes returned window index {window_index} for target {}",
            target.to_proto()
        )));
    }

    Ok(ListedPane {
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
    value
        .split('\x1f')
        .map(percent_decode_string)
        .collect::<Result<Vec<_>>>()
        .map(Some)
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

fn parse_error(message: impl Into<String>) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(message.into()))
}

fn is_already_closed_error<T: TargetSelector>(error: &RmuxError, target: &T) -> bool {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::SessionNotFound(session),
        } => session == target.session_name().as_str(),
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::InvalidTarget { value, reason },
        } => target.matches_invalid_target(value, reason),
        _ => false,
    }
}

trait TargetSelector {
    fn session_name(&self) -> &rmux_proto::SessionName;
    fn matches_invalid_target(&self, value: &str, reason: &str) -> bool;
}

impl TargetSelector for PaneRef {
    fn session_name(&self) -> &rmux_proto::SessionName {
        &self.session_name
    }

    fn matches_invalid_target(&self, value: &str, reason: &str) -> bool {
        let pane_target = self.to_proto().to_string();
        let window_target = format!("{}:{}", self.session_name, self.window_index);
        let mismatched_index_reason = matches!(
            reason,
            "window index does not exist in session" | "pane index does not exist in session"
        );
        mismatched_index_reason && (value == pane_target || value == window_target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details_with(history_bytes: u64) -> LiveDetails {
        LiveDetails {
            cols: 80,
            rows: 24,
            history_bytes,
            ..LiveDetails::default()
        }
    }

    #[test]
    fn revision_from_details_changes_with_history_bytes() {
        let r1 = revision_from_details(&details_with(10));
        let r2 = revision_from_details(&details_with(11));
        assert_ne!(r1, r2);
    }

    #[test]
    fn revision_from_details_is_never_zero() {
        assert_ne!(revision_from_details(&LiveDetails::default()), 0);
    }

    #[test]
    fn build_snapshot_pads_short_lines_with_blanks() {
        let details = LiveDetails {
            cols: 5,
            rows: 2,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"hi\n");
        assert_eq!(snapshot.cols, 5);
        assert_eq!(snapshot.rows, 2);
        assert_eq!(snapshot.cells.len(), 10);
        assert_eq!(snapshot.cells[0].text(), "h");
        assert_eq!(snapshot.cells[1].text(), "i");
        assert_eq!(snapshot.cells[2].text(), " ");
        assert_eq!(snapshot.cells[5].text(), " ");
    }

    #[test]
    fn build_snapshot_truncates_overlong_lines() {
        let details = LiveDetails {
            cols: 3,
            rows: 1,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"abcdef");
        assert_eq!(snapshot.cells.len(), 3);
        assert_eq!(snapshot.cells[0].text(), "a");
        assert_eq!(snapshot.cells[1].text(), "b");
        assert_eq!(snapshot.cells[2].text(), "c");
    }

    #[test]
    fn build_snapshot_handles_zero_dimensions() {
        let details = LiveDetails::default();
        let snapshot = build_snapshot(&details, b"");
        assert_eq!(snapshot.cells.len(), 0);
        assert!(snapshot.is_row_major_shape());
    }

    #[test]
    fn build_snapshot_revision_changes_after_output() {
        let details = LiveDetails {
            cols: 4,
            rows: 1,
            ..LiveDetails::default()
        };
        let first = build_snapshot(&details, b"abcd");
        let second = build_snapshot(&details, b"abce");
        assert_ne!(first.revision, second.revision);
    }

    #[test]
    fn build_snapshot_revision_changes_after_resize() {
        let small = LiveDetails {
            cols: 4,
            rows: 1,
            ..LiveDetails::default()
        };
        let large = LiveDetails {
            cols: 5,
            rows: 1,
            ..LiveDetails::default()
        };
        let s1 = build_snapshot(&small, b"abcd");
        let s2 = build_snapshot(&large, b"abcde");
        assert_ne!(s1.revision, s2.revision);
    }

    #[test]
    fn build_snapshot_revision_changes_after_exit() {
        let alive = LiveDetails {
            cols: 4,
            rows: 1,
            history_bytes: 4,
            ..LiveDetails::default()
        };
        let dead = LiveDetails {
            cols: 4,
            rows: 1,
            history_bytes: 4,
            dead: true,
            dead_status: Some(0),
            ..LiveDetails::default()
        };
        let s1 = build_snapshot(&alive, b"abcd");
        let s2 = build_snapshot(&dead, b"abcd");
        assert_ne!(s1.revision, s2.revision);
    }

    #[test]
    fn parse_details_line_handles_empty_optional_fields() {
        let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\t\t0\t0\t0\t/tmp";
        let details = parse_details_line(line).expect("parses");
        assert_eq!(details.pane_id.unwrap().to_string(), "%2");
        assert_eq!(details.pid, Some(1234));
        assert!(!details.dead);
        assert_eq!(details.dead_status, None);
        assert_eq!(details.dead_signal, None);
        assert_eq!(details.cols, 80);
        assert_eq!(details.rows, 24);
        assert_eq!(details.cursor_x, 10);
        assert_eq!(details.cursor_y, 5);
        assert!(details.cursor_visible);
        assert_eq!(details.history_bytes, 128);
        assert_eq!(details.history_size, 4);
        assert_eq!(details.current_path.as_deref(), Some("/tmp"));
    }

    #[test]
    fn parse_details_line_returns_default_for_blank_or_short_input() {
        assert_eq!(
            parse_details_line("").expect("blank"),
            LiveDetails::default()
        );
        assert_eq!(
            parse_details_line("only\tone\ttwo").expect("short"),
            LiveDetails::default()
        );
    }

    #[test]
    fn parse_details_line_preserves_tabs_inside_current_path() {
        let line =
            "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\t\t0\t0\t0\t/tmp/odd\tdir\twith\ttabs";
        let details = parse_details_line(line).expect("parses");
        assert_eq!(
            details.current_path.as_deref(),
            Some("/tmp/odd\tdir\twith\ttabs")
        );
    }

    #[test]
    fn parse_details_line_decodes_sticky_lifecycle_fields_without_env() {
        let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\tprintf\x1falpha%09beta%25\
             \t3\t5\t7\t/tmp/start";
        let details = parse_details_line(line).expect("parses");
        assert_eq!(
            details.start_command.as_deref(),
            Some(["printf".to_owned(), "alpha\tbeta%".to_owned()].as_slice())
        );
        assert_eq!(details.generation, 3);
        assert_eq!(details.lifecycle_revision, 5);
        assert_eq!(details.output_sequence, 7);
        assert_eq!(details.current_path.as_deref(), Some("/tmp/start"));
    }

    #[test]
    fn parse_details_line_rejects_malformed_encoded_command() {
        let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\tbad%XX\t1\t1\t1\t/tmp";
        assert!(parse_details_line(line).is_err());
    }

    #[test]
    fn build_snapshot_strips_trailing_carriage_returns() {
        let details = LiveDetails {
            cols: 4,
            rows: 1,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"abcd\r");
        assert_eq!(snapshot.cells[0].text(), "a");
        assert_eq!(snapshot.cells[1].text(), "b");
        assert_eq!(snapshot.cells[2].text(), "c");
        assert_eq!(snapshot.cells[3].text(), "d");
    }

    #[test]
    fn build_snapshot_handles_lossy_utf8_without_panicking() {
        let details = LiveDetails {
            cols: 2,
            rows: 1,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"\xff\xfe");
        assert_eq!(snapshot.cells.len(), 2);
        assert!(snapshot.is_row_major_shape());
    }

    #[test]
    fn build_snapshot_revision_is_stable_across_identical_inputs() {
        let details = LiveDetails {
            cols: 4,
            rows: 1,
            history_bytes: 2,
            ..LiveDetails::default()
        };
        let first = build_snapshot(&details, b"abcd");
        let second = build_snapshot(&details, b"abcd");
        assert_eq!(first, second);
        assert_eq!(first.revision, second.revision);
    }

    #[test]
    fn build_snapshot_revision_changes_when_pane_id_at_slot_changes() {
        let mut alpha = LiveDetails {
            cols: 4,
            rows: 1,
            history_bytes: 0,
            ..LiveDetails::default()
        };
        alpha.pane_id = Some(PaneId::new(7));
        let mut beta = alpha.clone();
        beta.pane_id = Some(PaneId::new(8));
        let s1 = build_snapshot(&alpha, b"abcd");
        let s2 = build_snapshot(&beta, b"abcd");
        assert_ne!(
            s1.revision, s2.revision,
            "slot reuse with a different pane id must bump the revision"
        );
    }

    #[test]
    fn build_snapshot_revision_changes_with_cursor_movement() {
        let mut details = LiveDetails {
            cols: 4,
            rows: 2,
            ..LiveDetails::default()
        };
        let baseline = build_snapshot(&details, b"abcd\nefgh");
        details.cursor_x = 2;
        let moved = build_snapshot(&details, b"abcd\nefgh");
        assert_ne!(baseline.revision, moved.revision);
    }

    #[test]
    fn revision_from_details_changes_when_pane_id_changes() {
        let mut alpha = LiveDetails {
            cols: 80,
            rows: 24,
            ..LiveDetails::default()
        };
        alpha.pane_id = Some(PaneId::new(1));
        let mut beta = alpha.clone();
        beta.pane_id = Some(PaneId::new(2));
        assert_ne!(revision_from_details(&alpha), revision_from_details(&beta));
    }

    #[test]
    fn pane_ref_target_selector_recognizes_session_invalidation() {
        let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 3, 1);
        assert!(target.matches_invalid_target("alpha:3.1", "pane index does not exist in session"));
        assert!(target.matches_invalid_target("alpha:3", "window index does not exist in session"));
        assert!(!target.matches_invalid_target("alpha:3.1", "pane index does not exist in window"));
        assert!(!target.matches_invalid_target("alpha:9", "window index does not exist in session"));
    }

    #[test]
    fn is_already_closed_error_matches_session_not_found_for_target_session() {
        let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 0, 0);
        let error = RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound("alpha".to_owned()));
        assert!(is_already_closed_error(&error, &target));
    }

    #[test]
    fn is_already_closed_error_does_not_match_session_not_found_for_other_session() {
        let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 0, 0);
        let error = RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound("beta".to_owned()));
        assert!(!is_already_closed_error(&error, &target));
    }

    #[test]
    fn is_already_closed_error_matches_invalid_window_or_pane_target() {
        let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 5, 2);
        let pane_invalid = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
            value: "alpha:5.2".to_owned(),
            reason: "pane index does not exist in session".to_owned(),
        });
        let window_invalid = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
            value: "alpha:5".to_owned(),
            reason: "window index does not exist in session".to_owned(),
        });
        assert!(is_already_closed_error(&pane_invalid, &target));
        assert!(is_already_closed_error(&window_invalid, &target));
    }

    #[test]
    fn is_already_closed_error_ignores_unrelated_protocol_errors() {
        let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 0, 0);
        let error = RmuxError::protocol(rmux_proto::RmuxError::Server(
            "daemon malfunction".to_owned(),
        ));
        assert!(!is_already_closed_error(&error, &target));
    }

    #[test]
    fn is_already_closed_error_ignores_invalid_target_for_other_slot() {
        let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 5, 2);
        let foreign = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
            value: "beta:0.0".to_owned(),
            reason: "pane index does not exist in session".to_owned(),
        });
        assert!(!is_already_closed_error(&foreign, &target));
    }

    #[test]
    fn build_snapshot_blank_capture_preserves_grid_dimensions() {
        let details = LiveDetails {
            cols: 4,
            rows: 3,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"");
        assert_eq!(snapshot.cells.len(), 12);
        assert!(snapshot.cells.iter().all(|cell| cell.text() == " "));
        assert!(snapshot.is_row_major_shape());
        assert_ne!(snapshot.revision, 0);
    }

    #[test]
    fn build_snapshot_revision_changes_for_each_individual_dead_field() {
        let alive = LiveDetails {
            cols: 4,
            rows: 1,
            ..LiveDetails::default()
        };
        let dead_no_status = LiveDetails {
            dead: true,
            ..alive.clone()
        };
        let dead_with_status = LiveDetails {
            dead: true,
            dead_status: Some(0),
            ..alive.clone()
        };
        let dead_with_signal = LiveDetails {
            dead: true,
            dead_signal: Some(15),
            ..alive.clone()
        };
        let s_alive = build_snapshot(&alive, b"abcd");
        let s_dead_no_status = build_snapshot(&dead_no_status, b"abcd");
        let s_dead_status = build_snapshot(&dead_with_status, b"abcd");
        let s_dead_signal = build_snapshot(&dead_with_signal, b"abcd");
        assert_ne!(s_alive.revision, s_dead_no_status.revision);
        assert_ne!(s_dead_no_status.revision, s_dead_status.revision);
        assert_ne!(s_dead_no_status.revision, s_dead_signal.revision);
        assert_ne!(s_dead_status.revision, s_dead_signal.revision);
    }

    #[test]
    fn build_snapshot_revision_changes_for_history_size_alone() {
        let baseline = LiveDetails {
            cols: 4,
            rows: 1,
            history_bytes: 0,
            history_size: 0,
            ..LiveDetails::default()
        };
        let only_size_changed = LiveDetails {
            history_size: 17,
            ..baseline.clone()
        };
        let s1 = build_snapshot(&baseline, b"abcd");
        let s2 = build_snapshot(&only_size_changed, b"abcd");
        assert_ne!(s1.revision, s2.revision);
    }

    #[test]
    fn derive_exit_state_treats_signal_zero_as_absent() {
        let details = LiveDetails {
            dead: true,
            dead_status: Some(7),
            dead_signal: Some(0),
            ..LiveDetails::default()
        };
        let exit = derive_exit_state(&details).expect("dead pane has exit state");
        assert_eq!(exit.code, Some(7));
        assert!(exit.signal.is_none());
    }

    #[test]
    fn derive_exit_state_returns_none_for_live_pane() {
        let details = LiveDetails {
            dead: false,
            dead_status: Some(7),
            dead_signal: Some(15),
            ..LiveDetails::default()
        };
        assert!(derive_exit_state(&details).is_none());
    }

    #[test]
    fn derive_process_state_running_carries_pid_when_present() {
        let details = LiveDetails {
            pid: Some(42),
            ..LiveDetails::default()
        };
        match derive_process_state(&details) {
            PaneProcessState::Running { pid: Some(42) } => {}
            other => panic!("expected Running with pid 42, got {other:?}"),
        }
    }

    #[test]
    fn derive_process_state_unknown_when_pid_missing_and_alive() {
        assert!(matches!(
            derive_process_state(&LiveDetails::default()),
            PaneProcessState::Unknown
        ));
    }

    #[test]
    fn pane_size_falls_back_to_window_when_details_are_zero() {
        let details = LiveDetails::default();
        let fallback = TerminalSizeSpec::new(80, 24);
        assert_eq!(pane_size_from_details(&details, &fallback), fallback);
    }

    #[test]
    fn pane_size_uses_details_when_present() {
        let details = LiveDetails {
            cols: 132,
            rows: 50,
            ..LiveDetails::default()
        };
        let fallback = TerminalSizeSpec::new(80, 24);
        assert_eq!(
            pane_size_from_details(&details, &fallback),
            TerminalSizeSpec::new(132, 50)
        );
    }

    #[test]
    fn build_cells_filters_embedded_carriage_returns_within_a_line() {
        let details = LiveDetails {
            cols: 4,
            rows: 1,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"\rok\r");
        assert_eq!(snapshot.cells.len(), 4);
        assert_eq!(snapshot.cells[0].text(), "o");
        assert_eq!(snapshot.cells[1].text(), "k");
        assert_eq!(snapshot.cells[2].text(), " ");
        assert_eq!(snapshot.cells[3].text(), " ");
    }

    #[test]
    fn build_cells_strips_crlf_line_endings() {
        let details = LiveDetails {
            cols: 4,
            rows: 2,
            ..LiveDetails::default()
        };
        let snapshot = build_snapshot(&details, b"abcd\r\nefgh\r\n");
        assert_eq!(snapshot.cells.len(), 8);
        assert_eq!(snapshot.cells[0].text(), "a");
        assert_eq!(snapshot.cells[3].text(), "d");
        assert_eq!(snapshot.cells[4].text(), "e");
        assert_eq!(snapshot.cells[7].text(), "h");
    }

    #[test]
    fn build_snapshot_revision_differentiates_blank_capture_from_visible_capture() {
        let details = LiveDetails {
            cols: 3,
            rows: 1,
            ..LiveDetails::default()
        };
        let blank = build_snapshot(&details, b"");
        let visible = build_snapshot(&details, b"abc");
        assert_ne!(blank.revision, visible.revision);
    }

    #[test]
    fn parse_details_line_rejects_malformed_pane_id_prefix() {
        let line = "no-prefix\t1\t0\t\t\t1\t1\t0\t0\t1\t0\t0\t0\t\t0\t0\t0\t/tmp";
        assert!(parse_details_line(line).is_err());
    }

    #[test]
    fn parse_details_line_treats_unset_cursor_visibility_as_visible() {
        let line = "%1\t1\t0\t\t\t1\t1\t0\t0\t\t0\t0\t0\t\t0\t0\t0\t/tmp";
        let details = parse_details_line(line).expect("parses");
        assert!(details.cursor_visible);
    }
}
