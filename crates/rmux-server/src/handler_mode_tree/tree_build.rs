//! Session/window/pane tree item construction for choose-tree mode.

use std::collections::BTreeMap;

use rmux_core::{formats::FormatContext, Pane, Session, Window};
use rmux_proto::{RmuxError, SessionName};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

use super::mode_tree_filter::matches_mode_tree_filter;
use super::mode_tree_model::{
    ModeTreeAction, ModeTreeBuild, ModeTreeClientState, ModeTreeItem, SortOrder,
};
use super::mode_tree_order::{finalize_mode_tree, pane_item_id, session_item_id, window_item_id};
use super::mode_tree_sort::{sort_sessions, sort_windows};

const TMUX_WINDOW_TREE_DEFAULT_FORMAT: &str = "#{?pane_format,#{?pane_marked,#[reverse],}#{pane_current_command}#{?pane_active,*,}#{?pane_marked,M,}#{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: \"#{pane_title}\",},window_format,#{?window_marked_flag,#[reverse],}#{window_name}#{window_flags}#{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: \"#{pane_title}\",},#{session_windows} windows#{?session_grouped, (group #{session_group}: #{session_group_list}),}#{?session_attached, (attached),}}";

pub(super) fn build_tree_items(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    attached_counts: &[(SessionName, usize)],
) -> Result<ModeTreeBuild, RmuxError> {
    let _show_all_group_members = mode.show_all_group_members;
    let mut sessions = state.sessions.iter().collect::<Vec<_>>();
    sort_sessions(&mut sessions, mode.sort_order, mode.reversed);

    let mut roots = Vec::new();
    let mut items = BTreeMap::new();
    for (session_name, session) in sessions {
        let Some((session_item, session_items)) =
            build_tree_session(mode, state, attached_counts, session_name, session)?
        else {
            continue;
        };
        roots.push(session_item.id.clone());
        for item in session_items {
            items.insert(item.id.clone(), item);
        }
    }
    Ok(finalize_mode_tree(items, roots, mode))
}

fn build_tree_session(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    attached_counts: &[(SessionName, usize)],
    session_name: &SessionName,
    session: &Session,
) -> Result<Option<(ModeTreeItem, Vec<ModeTreeItem>)>, RmuxError> {
    let session_id = session_item_id(session_name);
    let mut collected = Vec::new();
    let attached_count = attached_counts
        .iter()
        .find_map(|(name, count)| (name == session_name).then_some(*count))
        .unwrap_or(0);

    let mut windows = session.windows().iter().collect::<Vec<_>>();
    sort_windows(&mut windows, mode.sort_order, mode.reversed);
    for (window_index, window) in windows {
        let window_id = window_item_id(session_name, *window_index);
        let mut pane_children = Vec::new();
        let mut pane_items = Vec::new();
        let mut panes = window.panes().iter().collect::<Vec<_>>();
        panes.sort_by_key(|pane| pane.index());
        if mode.reversed && matches!(mode.sort_order, Some(SortOrder::Index)) {
            panes.reverse();
        }
        for pane in panes {
            let pane_line = render_tree_pane_line(
                mode,
                state,
                session,
                attached_count,
                *window_index,
                window,
                pane,
            );
            if !matches_mode_tree_filter(mode, &pane_line.1, pane_line.2.as_deref()) {
                continue;
            }
            let pane_id = pane_item_id(session_name, *window_index, pane.index());
            pane_children.push(pane_id.clone());
            pane_items.push(ModeTreeItem {
                id: pane_id,
                parent: Some(window_id.clone()),
                children: Vec::new(),
                depth: 2,
                line: pane_line.0,
                search_text: pane_line.1,
                preview: pane_line.3,
                no_tag: false,
                action: ModeTreeAction::TreeTarget {
                    session_name: session_name.clone(),
                    window_index: Some(*window_index),
                    pane_index: Some(pane.index()),
                    pane_id: Some(pane.id().as_u32()),
                },
            });
        }

        let window_line =
            render_tree_window_line(mode, state, session, attached_count, *window_index, window);
        let window_matches =
            matches_mode_tree_filter(mode, &window_line.1, window_line.2.as_deref());
        if !window_matches && pane_children.is_empty() {
            continue;
        }
        let window_item = ModeTreeItem {
            id: window_id.clone(),
            parent: Some(session_id.clone()),
            children: pane_children,
            depth: 1,
            line: window_line.0,
            search_text: window_line.1,
            preview: window_line.3,
            no_tag: false,
            action: ModeTreeAction::TreeTarget {
                session_name: session_name.clone(),
                window_index: Some(*window_index),
                pane_index: None,
                pane_id: None,
            },
        };
        collected.push(window_item);
        collected.extend(pane_items);
    }

    let session_line = render_tree_session_line(mode, state, session, attached_count);
    let session_matches =
        matches_mode_tree_filter(mode, &session_line.1, session_line.2.as_deref());
    if !session_matches && collected.is_empty() {
        return Ok(None);
    }

    let children = collected
        .iter()
        .filter(|item| item.parent.as_deref() == Some(session_id.as_str()))
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let session_item = ModeTreeItem {
        id: session_id,
        parent: None,
        children,
        depth: 0,
        line: session_line.0,
        search_text: session_line.1,
        preview: session_line.3,
        no_tag: false,
        action: ModeTreeAction::TreeTarget {
            session_name: session_name.clone(),
            window_index: None,
            pane_index: None,
            pane_id: None,
        },
    };

    let mut all = vec![session_item.clone()];
    all.extend(collected);
    Ok(Some((session_item, all)))
}

fn render_tree_session_line(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    session: &Session,
    attached_count: usize,
) -> (String, String, Option<String>, Vec<String>) {
    let context = RuntimeFormatContext::new(
        FormatContext::from_session(session).with_session_attached(attached_count),
    )
    .with_state(state)
    .with_session(session);
    let default = format!("{} windows", session.windows().len());
    render_tree_named_line(
        mode,
        context,
        session.name().as_str(),
        mode.row_format
            .as_deref()
            .unwrap_or(TMUX_WINDOW_TREE_DEFAULT_FORMAT),
        default,
        vec![
            format!("session {}", session.name()),
            format!("windows {}", session.windows().len()),
        ],
    )
}

fn render_tree_window_line(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    session: &Session,
    attached_count: usize,
    window_index: u32,
    window: &Window,
) -> (String, String, Option<String>, Vec<String>) {
    let context = RuntimeFormatContext::new(
        FormatContext::from_session(session)
            .with_session_attached(attached_count)
            .with_window(
                window_index,
                window,
                window_index == session.active_window_index(),
                Some(window_index) == session.last_window_index(),
            ),
    )
    .with_state(state)
    .with_session(session)
    .with_window(window_index, window);
    let default = format!(
        "{}{}",
        window.name().unwrap_or_default(),
        render_runtime_template("#{window_flags}", &context, false)
    );
    render_tree_named_line(
        mode,
        context,
        &window_index.to_string(),
        mode.row_format
            .as_deref()
            .unwrap_or(TMUX_WINDOW_TREE_DEFAULT_FORMAT),
        default,
        Vec::new(),
    )
}

fn render_tree_pane_line(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    session: &Session,
    attached_count: usize,
    window_index: u32,
    window: &Window,
    pane: &Pane,
) -> (String, String, Option<String>, Vec<String>) {
    let context = RuntimeFormatContext::new(
        FormatContext::from_session(session)
            .with_session_attached(attached_count)
            .with_window(
                window_index,
                window,
                window_index == session.active_window_index(),
                Some(window_index) == session.last_window_index(),
            )
            .with_pane(pane, pane.index() == window.active_pane_index()),
    )
    .with_state(state)
    .with_session(session)
    .with_window(window_index, window)
    .with_pane(pane);
    let default = format!(
        "{}{}",
        render_runtime_template("#{pane_current_command}", &context, false),
        render_runtime_template("#{pane_flags}", &context, false)
    );
    let cwd = render_runtime_template("#{pane_current_path}", &context, false);
    let cwd_suffix = if cwd.is_empty() {
        None
    } else {
        Some(abbreviate_home(&cwd))
    };
    let (mut line, mut search_text, filter, preview) = render_tree_named_line(
        mode,
        context,
        &pane.index().to_string(),
        mode.row_format
            .as_deref()
            .unwrap_or(TMUX_WINDOW_TREE_DEFAULT_FORMAT),
        default,
        Vec::new(),
    );
    if let Some(path) = cwd_suffix {
        // rmux extension over tmux: show each pane's live foreground-process
        // cwd (/proc/$pid/cwd, so it tracks the user *into* vim, cargo, etc.)
        // so the picker is useful when you have many shells across many trees.
        line.push_str(" · ");
        line.push_str(&path);
        search_text.push_str(" · ");
        search_text.push_str(&path);
    }
    (line, search_text, filter, preview)
}

fn abbreviate_home(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_owned();
    };
    let home = home.to_string_lossy();
    if home.is_empty() {
        return path.to_owned();
    }
    let trimmed = home.trim_end_matches('/');
    if path == trimmed {
        return "~".to_owned();
    }
    if let Some(rest) = path.strip_prefix(trimmed) {
        if rest.starts_with('/') {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}

fn render_tree_named_line<'a>(
    mode: &ModeTreeClientState,
    context: RuntimeFormatContext<'a>,
    name: &str,
    template: &str,
    default: String,
    preview: Vec<String>,
) -> (String, String, Option<String>, Vec<String>) {
    let rendered = render_runtime_template(template, &context, true);
    let search_rendered = render_runtime_template(template, &context, false);
    let detail = if rendered.is_empty() {
        default.clone()
    } else {
        rendered
    };
    let search_detail = if search_rendered.is_empty() {
        default
    } else {
        search_rendered
    };
    let line = tree_item_display_line(name, &detail);
    let search_text = tree_item_display_line(name, &search_detail);
    let filter = mode
        .filter_format
        .as_deref()
        .map(|template| render_runtime_template(template, &context, false));
    (line, search_text, filter, preview)
}

pub(super) fn tree_item_display_line(name: &str, detail: &str) -> String {
    if detail.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {detail}")
    }
}
