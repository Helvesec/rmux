use chrono::{DateTime, Datelike, Local, LocalResult, TimeZone};
use rmux_core::{formats::FormatContext, Utf8Config};
use rmux_proto::{OptionName, PaneTarget, RmuxError};
use std::collections::BTreeMap;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

use super::super::RequestHandler;
use super::mode_tree_customize_build::build_customize_items;
use super::mode_tree_filter::matches_mode_tree_filter;
use super::mode_tree_model::{
    ClientSnapshot, ModeTreeAction, ModeTreeBuild, ModeTreeClientState, ModeTreeItem, TreeDepth,
};
use super::mode_tree_order::{
    client_item_id, finalize_mode_tree, pane_item_id, session_item_id, window_item_id,
};
use super::mode_tree_render::mode_tree_list_rows;
use super::mode_tree_selection::{clamp_scroll, ensure_selected_visible, normalize_selection};
use super::mode_tree_sort::{sort_buffer_entries, sort_clients};
use super::mode_tree_tree_build::build_tree_items;
#[cfg(test)]
pub(super) use super::mode_tree_tree_build::tree_item_display_line;

const TMUX_CLIENT_DEFAULT_FORMAT: &str = "#{t/p:client_activity}: session #{client_session}";

impl RequestHandler {
    pub(super) async fn build_mode_tree(
        &self,
        mode: &mut ModeTreeClientState,
        attach_pid: u32,
    ) -> Result<ModeTreeBuild, RmuxError> {
        let client_snapshot = self.mode_tree_clients_snapshot().await;
        let mut state = self.state.lock().await;
        if matches!(mode.kind, super::mode_tree_model::ModeTreeKind::Tree) {
            state.ensure_live_window_link_occurrences();
        }
        let active_attach = self.active_attach.lock().await;
        let active_control = self.active_control.lock().await;
        let current_attach_session = active_attach
            .by_pid
            .get(&attach_pid)
            .map(|active| active.session_name.clone());
        let attached_counts = state
            .sessions
            .iter()
            .map(|(session_name, _)| {
                let mut count = active_attach.attached_count(session_name)
                    + active_control.attached_count(session_name);
                if count == 0 && current_attach_session.as_ref() == Some(session_name) {
                    count = 1;
                }
                (session_name.clone(), count)
            })
            .collect::<Vec<_>>();
        let build = match mode.kind {
            super::mode_tree_model::ModeTreeKind::Tree => {
                build_tree_items(mode, &state, &attached_counts)?
            }
            super::mode_tree_model::ModeTreeKind::Buffer => build_buffer_items(mode, &state),
            super::mode_tree_model::ModeTreeKind::Client => {
                build_client_items(mode, &state, &client_snapshot)
            }
            super::mode_tree_model::ModeTreeKind::Customize => {
                build_customize_items(mode, &state, attach_pid)?
            }
        };
        let visible = if build.visible.is_empty() {
            None
        } else {
            Some(build)
        };
        let build = if let Some(build) = visible {
            build
        } else if mode
            .filter_text
            .as_deref()
            .is_some_and(|filter| !filter.is_empty())
        {
            let original_filter = mode.filter_text.take();
            let mut fallback = match mode.kind {
                super::mode_tree_model::ModeTreeKind::Tree => {
                    build_tree_items(mode, &state, &attached_counts)?
                }
                super::mode_tree_model::ModeTreeKind::Buffer => build_buffer_items(mode, &state),
                super::mode_tree_model::ModeTreeKind::Client => {
                    build_client_items(mode, &state, &client_snapshot)
                }
                super::mode_tree_model::ModeTreeKind::Customize => {
                    build_customize_items(mode, &state, attach_pid)?
                }
            };
            mode.filter_text = original_filter;
            fallback.no_matches = true;
            fallback
        } else {
            ModeTreeBuild {
                items: BTreeMap::new(),
                roots: Vec::new(),
                order: Vec::new(),
                visible: Vec::new(),
                no_matches: false,
            }
        };

        // tmux parity: tags live on the rebuilt items, so a tag whose item no
        // longer exists dies with the item. Without pruning, stale tagged ids
        // make the tagged set non-empty while intersecting with no built
        // item, which permanently disables the fallback-to-current-selection
        // in selected_items — Enter and tree-kill silently do nothing after
        // a tagged item is killed or its session is renamed.
        mode.tagged.retain(|id| build.items.contains_key(id));

        if build.visible.is_empty() {
            mode.selected_id = None;
            mode.scroll = 0;
        } else if mode
            .selected_id
            .as_ref()
            .map(|id| !build.items.contains_key(id))
            .unwrap_or(true)
        {
            mode.selected_id = mode_tree_default_selection(mode, &build, attach_pid, &state)
                .or_else(|| build.visible.first().cloned());
            mode.scroll = 0;
        }
        if let Some(session) = state.sessions.session(&mode.session_name) {
            let rows = session.window().size().rows;
            let status_on = state
                .options
                .resolve(Some(session.name()), OptionName::Status)
                .map(|value| value != "off")
                .unwrap_or(true);
            let usable = rows.saturating_sub(u16::from(status_on));
            mode.last_list_rows = usize::from(mode_tree_list_rows(
                usable,
                build.visible.len(),
                mode.preview_mode,
            ));
        }
        if let Some(selected) = mode
            .selected_id
            .as_ref()
            .and_then(|id| build.visible.iter().position(|visible| visible == id))
        {
            ensure_selected_visible(mode, selected);
        }
        clamp_scroll(mode, &build);
        normalize_selection(mode, &build);
        Ok(build)
    }

    pub(super) async fn seed_mode_tree_defaults(
        &self,
        mode: &mut ModeTreeClientState,
    ) -> Result<(), RmuxError> {
        if !matches!(mode.kind, super::mode_tree_model::ModeTreeKind::Tree) {
            return Ok(());
        }
        let mut state = self.state.lock().await;
        state.ensure_live_window_link_occurrences();
        let mut sessions = state.sessions.iter().collect::<Vec<_>>();
        sessions.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        for (_, session) in sessions {
            let session_item_key = session_item_id(session.id());
            match mode.tree_depth {
                TreeDepth::Session => {}
                TreeDepth::Window | TreeDepth::Pane => {
                    mode.expanded.insert(session_item_key);
                }
            }
            if matches!(mode.tree_depth, TreeDepth::Pane) {
                for (window_index, window) in session.windows() {
                    let occurrence_id = state
                        .window_link_occurrence_id(session.name(), *window_index)
                        .expect("live mode-tree windows have occurrence identities");
                    mode.expanded.insert(window_item_id(
                        session.id(),
                        *window_index,
                        window.id(),
                        occurrence_id,
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) async fn mode_tree_clients_snapshot(&self) -> Vec<ClientSnapshot> {
        let now = Local::now().timestamp();
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .iter()
            .map(|(&pid, active)| ClientSnapshot {
                pid,
                attach_id: active.id,
                session_name: Some(active.session_name.clone()),
                label: attached_client_label(pid),
                activity: now,
                width: active.client_size.cols,
                height: active.client_size.rows,
            })
            .collect::<Vec<_>>()
    }

    pub(super) async fn mode_tree_buffer_empty(&self) -> bool {
        let state = self.state.lock().await;
        state.buffers.is_empty()
    }

    pub(super) async fn mode_tree_client_empty(&self) -> bool {
        self.mode_tree_clients_snapshot().await.is_empty()
    }
}

pub(super) fn build_buffer_items(
    mode: &ModeTreeClientState,
    state: &HandlerState,
) -> ModeTreeBuild {
    let mut entries = state.buffers.entries();
    sort_buffer_entries(&mut entries, mode.sort_order, mode.reversed);
    let mut roots = Vec::new();
    let mut items = BTreeMap::new();
    for entry in entries {
        let line = render_buffer_line(mode, state, entry);
        if !matches_mode_tree_filter(mode, &line.1, line.2.as_deref()) {
            continue;
        }
        let id = format!("buffer:{}", entry.name());
        roots.push(id.clone());
        items.insert(
            id.clone(),
            ModeTreeItem {
                id,
                parent: None,
                children: Vec::new(),
                depth: 0,
                line: line.0,
                search_text: line.1,
                preview: line.3,
                no_tag: false,
                action: ModeTreeAction::Buffer {
                    name: entry.name().to_owned(),
                },
            },
        );
    }
    finalize_mode_tree(items, roots, mode)
}

fn render_buffer_line<'a>(
    mode: &ModeTreeClientState,
    state: &'a HandlerState,
    entry: rmux_core::BufferView<'a>,
) -> (String, String, Option<String>, Vec<String>) {
    let context = RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_named_value("buffer_name", entry.name())
        .with_named_value("buffer_size", entry.size().to_string())
        .with_named_value("buffer_sample", entry.sample())
        .with_named_value("buffer_created", entry.created().to_string());
    let default = format!(
        "{} [{} bytes] {}",
        entry.name(),
        entry.size(),
        entry.sample()
    );
    let rendered = render_runtime_template(
        mode.row_format
            .as_deref()
            .unwrap_or("#{buffer_name}: #{buffer_sample}"),
        &context,
        true,
    );
    let line = if rendered.is_empty() {
        default
    } else {
        rendered
    };
    let filter = mode
        .filter_format
        .as_deref()
        .map(|template| render_runtime_template(template, &context, false));
    let preview = String::from_utf8_lossy(entry.content())
        .lines()
        .take(20)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (line.clone(), line, filter, preview)
}

pub(super) fn build_client_items(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    clients: &[ClientSnapshot],
) -> ModeTreeBuild {
    let mut clients = clients.to_vec();
    sort_clients(&mut clients, mode.sort_order, mode.reversed);
    let mut roots = Vec::new();
    let mut items = BTreeMap::new();
    for client in clients {
        let context = RuntimeFormatContext::new(FormatContext::new())
            .with_state(state)
            .with_named_value("client_name", client.label.clone())
            .with_named_value("client_size", format!("{}x{}", client.width, client.height))
            .with_named_value("client_activity", client.activity.to_string())
            .with_named_value(
                "client_session",
                client
                    .session_name
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )
            .with_named_value(
                "session_name",
                client
                    .session_name
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            );
        let rendered = render_runtime_template(
            mode.row_format
                .as_deref()
                .unwrap_or(TMUX_CLIENT_DEFAULT_FORMAT),
            &context,
            true,
        );
        let line = if mode.row_format.is_none() {
            format!("{}: {}", client.label, default_client_mode_line(&client))
        } else if rendered.is_empty() {
            client.label.clone()
        } else {
            rendered
        };
        let filter = mode
            .filter_format
            .as_deref()
            .map(|template| render_runtime_template(template, &context, false));
        if !matches_mode_tree_filter(mode, &line, filter.as_deref()) {
            continue;
        }
        let id = client_item_id(client.pid, client.attach_id);
        let preview = client
            .session_name
            .as_ref()
            .and_then(|session_name| {
                state.sessions.session(session_name).map(|session| {
                    PaneTarget::with_window(
                        session_name.clone(),
                        session.active_window_index(),
                        session.active_pane_index(),
                    )
                })
            })
            .map(|target| {
                super::preview_lines_for_target(
                    state,
                    &target,
                    20,
                    40,
                    &Utf8Config::from_options(&state.options),
                )
            })
            .unwrap_or_default();
        roots.push(id.clone());
        items.insert(
            id.clone(),
            ModeTreeItem {
                id,
                parent: None,
                children: Vec::new(),
                depth: 0,
                line: line.clone(),
                search_text: line,
                preview,
                no_tag: false,
                action: ModeTreeAction::Client {
                    pid: client.pid,
                    attach_id: client.attach_id,
                    control: false,
                },
            },
        );
    }
    finalize_mode_tree(items, roots, mode)
}

fn default_client_mode_line(client: &ClientSnapshot) -> String {
    let activity = local_datetime(client.activity)
        .map(format_pretty_time)
        .unwrap_or_default();
    match client.session_name.as_ref() {
        Some(session_name) => format!("{activity}: session {session_name}"),
        None => activity,
    }
}

fn format_pretty_time(time: DateTime<Local>) -> String {
    let now = Local::now();
    let age = now.timestamp().saturating_sub(time.timestamp());

    if age < 24 * 60 * 60 {
        return time.format("%H:%M").to_string();
    }
    if (time.year() == now.year() && time.month() == now.month()) || age < 28 * 24 * 60 * 60 {
        return time.format("%a%d").to_string();
    }
    let same_or_previous_year = (time.year() == now.year() && time.month() < now.month())
        || (time.year() == now.year() - 1 && time.month() > now.month());
    if same_or_previous_year {
        return time.format("%d%b").to_string();
    }
    time.format("%b%y").to_string()
}

fn local_datetime(epoch: i64) -> Option<DateTime<Local>> {
    match Local.timestamp_opt(epoch, 0) {
        LocalResult::Single(date_time) => Some(date_time),
        LocalResult::Ambiguous(date_time, _) => Some(date_time),
        LocalResult::None => None,
    }
}

fn attached_client_label(attach_pid: u32) -> String {
    rmux_os::process::fd_path(attach_pid, 0)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| attach_pid.to_string())
}

fn mode_tree_default_selection(
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
    attach_pid: u32,
    state: &HandlerState,
) -> Option<String> {
    match mode.kind {
        super::mode_tree_model::ModeTreeKind::Tree => {
            let session = state.sessions.session(&mode.session_name)?;
            let active_window_index = session.active_window_index();
            let active_window = session.window_at(active_window_index)?;
            let occurrence_id = state
                .window_link_occurrence_id(session.name(), active_window_index)
                .expect("live mode-tree windows have occurrence identities");
            let selected_id = match mode.tree_depth {
                TreeDepth::Session => session_item_id(session.id()),
                TreeDepth::Window => window_item_id(
                    session.id(),
                    active_window_index,
                    active_window.id(),
                    occurrence_id,
                ),
                TreeDepth::Pane => pane_item_id(
                    session.id(),
                    active_window_index,
                    active_window.id(),
                    occurrence_id,
                    active_window.active_pane()?.id(),
                ),
            };
            build
                .items
                .contains_key(&selected_id)
                .then_some(selected_id)
        }
        super::mode_tree_model::ModeTreeKind::Client => {
            build.items.values().find_map(|item| match item.action {
                ModeTreeAction::Client { pid, .. } if pid == attach_pid => Some(item.id.clone()),
                _ => None,
            })
        }
        super::mode_tree_model::ModeTreeKind::Buffer
        | super::mode_tree_model::ModeTreeKind::Customize => build.visible.first().cloned(),
    }
}
