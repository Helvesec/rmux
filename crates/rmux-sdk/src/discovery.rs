//! SDK-side discovery helpers for existing rmux-managed terminals.
//!
//! Discovery is deliberately an inventory layer, not a metadata database.
//! It queries the daemon's current session and pane lists, enriches matches
//! through existing pane info/title accessors, and returns regular [`Pane`]
//! handles that callers can use immediately.
//!
//! P3 discovery is SDK-side and performs one or more daemon round-trips per
//! discovered pane. Scope queries with [`PaneFinder::session`] when possible
//! for large workspaces.

use std::collections::HashSet;

use crate::handles::session::unexpected_response;
use crate::{
    Pane, PaneId, PaneInfo, PaneProcessState, PaneSet, Result, Rmux, RmuxError, Session, SessionId,
    SessionName, WindowId,
};
use rmux_proto::{ListPanesRequest, Request, Response};

const PANE_DISCOVERY_FORMAT: &str = "#{window_index}:#{pane_index}:#{pane_id}";

/// One pane discovered from the daemon's current inventory.
#[derive(Debug, Clone)]
pub struct DiscoveredPane {
    /// Owning session name.
    pub session_name: SessionName,
    /// Owning session id.
    pub session_id: SessionId,
    /// Owning window id.
    pub window_id: WindowId,
    /// Current window index inside the owning session.
    pub window_index: u32,
    /// Stable daemon pane id.
    pub pane_id: PaneId,
    /// Current pane index inside the owning window.
    pub pane_index: u32,
    /// Current pane title, when the daemon exposes one.
    pub title: Option<String>,
    /// Spawned process argv recorded by the daemon.
    pub command: Option<Vec<String>>,
    /// Process working directory recorded by the daemon.
    pub working_directory: Option<String>,
    /// Tags recorded on the pane/window/session info surfaces.
    pub tags: Vec<String>,
    /// Current pane process state.
    pub process: PaneProcessState,
    /// Stable pane handle addressed by pane id.
    pub pane: Pane,
}

/// One session discovered from the daemon inventory.
#[derive(Debug)]
pub struct DiscoveredSession {
    /// Session name.
    pub name: SessionName,
    /// Live session handle.
    pub session: Session,
}

/// Query builder for rmux-managed sessions.
#[derive(Debug)]
#[must_use = "session discovery queries do nothing unless all() or one() is awaited"]
pub struct SessionFinder<'a> {
    rmux: &'a Rmux,
    name: Option<String>,
}

impl<'a> SessionFinder<'a> {
    pub(crate) const fn new(rmux: &'a Rmux) -> Self {
        Self { rmux, name: None }
    }

    /// Restricts discovery to one exact session name.
    pub fn name(mut self, name: impl AsRef<str>) -> Self {
        self.name = Some(name.as_ref().to_owned());
        self
    }

    /// Returns every session matching this query.
    pub async fn all(self) -> Result<Vec<DiscoveredSession>> {
        let exact_name = match &self.name {
            Some(name) => Some(SessionName::new(name).map_err(RmuxError::protocol)?),
            None => None,
        };
        let rmux = self.rmux.begin_operation_handle().await?;
        let names = match exact_name {
            Some(name) => {
                if rmux.has_session(name.clone()).await? {
                    vec![name]
                } else {
                    Vec::new()
                }
            }
            None => rmux.list_sessions().await?,
        };
        let mut sessions = Vec::new();
        for name in names {
            let session = rmux.session(name.clone()).await?;
            sessions.push(DiscoveredSession { name, session });
        }
        Ok(sessions)
    }

    /// Returns the single session matching this query.
    pub async fn one(self) -> Result<Session> {
        let matches = self.all().await?;
        match matches.len() {
            1 => Ok(matches
                .into_iter()
                .next()
                .expect("single match length guarantees one entry")
                .session),
            count => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
                "strict session discovery violation: expected 1 match, found {count}"
            )))),
        }
    }
}

/// Query builder for panes already managed by rmux.
#[derive(Debug)]
#[must_use = "pane discovery queries do nothing unless all(), one(), or collect_paneset() is awaited"]
pub struct PaneFinder<'a> {
    rmux: &'a Rmux,
    filters: PaneFilters,
}

#[derive(Debug, Default, Clone)]
struct PaneFilters {
    session: Option<String>,
    title: Option<String>,
    title_prefix: Option<String>,
    command_contains: Option<String>,
    cwd_contains: Option<String>,
    running: Option<bool>,
    window_index: Option<u32>,
}

impl<'a> PaneFinder<'a> {
    pub(crate) fn new(rmux: &'a Rmux) -> Self {
        Self {
            rmux,
            filters: PaneFilters::default(),
        }
    }

    /// Restricts discovery to one session.
    pub fn session(mut self, session_name: impl AsRef<str>) -> Self {
        self.filters.session = Some(session_name.as_ref().to_owned());
        self
    }

    /// Restricts discovery to panes with this exact title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.filters.title = Some(title.into());
        self
    }

    /// Restricts discovery to panes whose title starts with `prefix`.
    pub fn title_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.filters.title_prefix = Some(prefix.into());
        self
    }

    /// Restricts discovery to panes whose recorded argv contains `needle`.
    pub fn command_contains(mut self, needle: impl Into<String>) -> Self {
        self.filters.command_contains = Some(needle.into());
        self
    }

    /// Restricts discovery to panes whose recorded cwd contains `needle`.
    pub fn cwd_contains(mut self, needle: impl Into<String>) -> Self {
        self.filters.cwd_contains = Some(needle.into());
        self
    }

    /// Restricts discovery to one window index.
    pub const fn window_index(mut self, index: u32) -> Self {
        self.filters.window_index = Some(index);
        self
    }

    /// Restricts discovery to panes whose process is currently running.
    pub const fn running(mut self) -> Self {
        self.filters.running = Some(true);
        self
    }

    /// Restricts discovery to panes whose process has exited.
    pub const fn exited(mut self) -> Self {
        self.filters.running = Some(false);
        self
    }

    /// Returns every pane matching this query, deduplicated by session and pane id.
    pub async fn all(self) -> Result<Vec<DiscoveredPane>> {
        discover_panes(self.rmux, &self.filters).await
    }

    /// Returns the single pane matching this query.
    ///
    /// A zero-match or multi-match result is reported as a strict discovery
    /// violation with the query summary in the diagnostic text.
    pub async fn one(self) -> Result<Pane> {
        let filters = self.filters.clone();
        let matches = discover_panes(self.rmux, &filters).await?;
        match matches.len() {
            1 => Ok(matches
                .into_iter()
                .next()
                .expect("single match length guarantees one entry")
                .pane),
            count => Err(strict_discovery_error(count, &filters)),
        }
    }

    /// Collects matching panes into a [`PaneSet`].
    pub async fn collect_paneset(self) -> Result<PaneSet> {
        let panes = self
            .all()
            .await?
            .into_iter()
            .map(|discovered| discovered.pane);
        Ok(PaneSet::new(panes))
    }
}

async fn discover_panes(rmux: &Rmux, filters: &PaneFilters) -> Result<Vec<DiscoveredPane>> {
    let exact_session = match &filters.session {
        Some(session_name) => Some(SessionName::new(session_name).map_err(RmuxError::protocol)?),
        None => None,
    };
    let rmux = rmux.begin_operation_handle().await?;
    let deadline = rmux
        .operation_deadline()
        .expect("operation facade always carries a deadline scope");
    let session_names = match exact_session {
        Some(session_name) => vec![session_name],
        None => rmux.list_sessions().await?,
    };

    let mut seen = HashSet::new();
    let mut discovered = Vec::new();
    for session_name in session_names {
        if !rmux.has_session(session_name.clone()).await? {
            continue;
        }
        let session = rmux.session(session_name.clone()).await?;
        let operation_session = session.with_operation_deadline(deadline);
        for listed in list_session_panes(&operation_session).await? {
            if !seen.insert((session_name.clone(), listed.pane_id)) {
                continue;
            }
            let pane = operation_session.pane_by_id(listed.pane_id).await?;
            let operation_pane = pane.with_operation_deadline(deadline);
            let snapshot = operation_pane.info().await?;
            let Some(info) = snapshot.pane(listed.pane_id).cloned() else {
                continue;
            };
            let title = operation_pane.title().await?;
            let Some(entry) =
                discovered_from_info(session_name.clone(), listed, pane, info, title, &snapshot)
            else {
                continue;
            };
            if filters.matches(&entry) {
                discovered.push(entry);
            }
        }
    }

    Ok(discovered)
}

fn discovered_from_info(
    session_name: SessionName,
    listed: ListedPane,
    pane: Pane,
    info: PaneInfo,
    title: Option<String>,
    snapshot: &crate::InfoSnapshot,
) -> Option<DiscoveredPane> {
    let window = snapshot.window(info.window_id)?;
    let tags = merge_tags(
        &info.tags,
        &window.tags,
        snapshot
            .session(info.session_id)
            .map(|session| &session.tags),
    );
    Some(DiscoveredPane {
        session_name,
        session_id: info.session_id,
        window_id: info.window_id,
        window_index: listed.window_index,
        pane_id: info.id,
        pane_index: listed.pane_index,
        title,
        command: info.command,
        working_directory: info.working_directory,
        tags,
        process: info.process,
        pane,
    })
}

fn merge_tags(pane: &[String], window: &[String], session: Option<&Vec<String>>) -> Vec<String> {
    let mut tags = Vec::new();
    for tag in pane
        .iter()
        .chain(window.iter())
        .chain(session.into_iter().flatten())
    {
        if !tags.iter().any(|seen| seen == tag) {
            tags.push(tag.clone());
        }
    }
    tags
}

impl PaneFilters {
    fn matches(&self, pane: &DiscoveredPane) -> bool {
        if let Some(title) = &self.title {
            if pane.title.as_deref() != Some(title.as_str()) {
                return false;
            }
        }
        if let Some(prefix) = &self.title_prefix {
            if !pane
                .title
                .as_deref()
                .is_some_and(|title| title.starts_with(prefix))
            {
                return false;
            }
        }
        if let Some(needle) = &self.command_contains {
            if !pane
                .command
                .as_ref()
                .is_some_and(|argv| argv.iter().any(|arg| arg.contains(needle)))
            {
                return false;
            }
        }
        if let Some(needle) = &self.cwd_contains {
            if !pane
                .working_directory
                .as_deref()
                .is_some_and(|cwd| cwd.contains(needle))
            {
                return false;
            }
        }
        if let Some(running) = self.running {
            if !pane.process_matches(running) {
                return false;
            }
        }
        if let Some(window_index) = self.window_index {
            if pane.window_index != window_index {
                return false;
            }
        }
        true
    }
}

impl DiscoveredPane {
    fn process_matches(&self, running: bool) -> bool {
        matches!(
            (running, &self.process),
            (true, PaneProcessState::Running { .. }) | (false, PaneProcessState::Exited)
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct ListedPane {
    window_index: u32,
    pane_index: u32,
    pane_id: PaneId,
}

async fn list_session_panes(session: &crate::Session) -> Result<Vec<ListedPane>> {
    let response = session
        .transport()
        .request(Request::ListPanes(Box::new(ListPanesRequest {
            target: session.name().clone(),
            target_window_index: None,
            format: Some(PANE_DISCOVERY_FORMAT.to_owned()),
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
        .map(parse_listed_pane)
        .collect()
}

fn parse_listed_pane(line: &str) -> Result<ListedPane> {
    let mut fields = line.split(':');
    let window_index = fields
        .next()
        .ok_or_else(|| parse_error("pane discovery line omitted window index"))?;
    let pane_index = fields
        .next()
        .ok_or_else(|| parse_error("pane discovery line omitted pane index"))?;
    let pane_id = fields
        .next()
        .ok_or_else(|| parse_error("pane discovery line omitted pane id"))?;
    if fields.next().is_some() {
        return Err(parse_error("pane discovery line had trailing fields"));
    }
    Ok(ListedPane {
        window_index: parse_u32(window_index, "window index")?,
        pane_index: parse_u32(pane_index, "pane index")?,
        pane_id: parse_pane_id(pane_id)?,
    })
}

fn parse_pane_id(value: &str) -> Result<PaneId> {
    let raw = value
        .strip_prefix('%')
        .ok_or_else(|| parse_error(format!("pane id `{value}` omitted `%` prefix")))?;
    parse_u32(raw, "pane id").map(PaneId::new)
}

fn parse_u32(value: &str, field: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|error| parse_error(format!("invalid {field} `{value}`: {error}")))
}

fn strict_discovery_error(count: usize, filters: &PaneFilters) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "strict pane discovery violation: expected 1 match, found {count}; query: {}",
        filters.describe()
    )))
}

impl PaneFilters {
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(session) = &self.session {
            parts.push(format!("session={session}"));
        }
        if let Some(title) = &self.title {
            parts.push(format!("title={title:?}"));
        }
        if let Some(prefix) = &self.title_prefix {
            parts.push(format!("title_prefix={prefix:?}"));
        }
        if let Some(needle) = &self.command_contains {
            parts.push(format!("command_contains={needle:?}"));
        }
        if let Some(needle) = &self.cwd_contains {
            parts.push(format!("cwd_contains={needle:?}"));
        }
        if let Some(running) = self.running {
            parts.push(
                if running {
                    "running=true"
                } else {
                    "exited=true"
                }
                .to_owned(),
            );
        }
        if let Some(window_index) = self.window_index {
            parts.push(format!("window_index={window_index}"));
        }
        if parts.is_empty() {
            "<all panes>".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

fn parse_error(message: impl Into<String>) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(message.into()))
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
