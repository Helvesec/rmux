use std::collections::{BTreeMap, BTreeSet};

use rmux_proto::{PaneId, SessionId, WindowId};

use crate::pane_terminals::WindowLinkOccurrenceId;

use super::mode_tree_model::{ModeTreeBuild, ModeTreeClientState, ModeTreeItem};

pub(super) fn session_item_id(session_id: SessionId) -> String {
    format!("session:{session_id}")
}

pub(super) fn client_item_id(pid: u32, attach_id: u64) -> String {
    format!("client:{pid}:{attach_id}")
}

pub(super) fn window_item_id(
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: WindowLinkOccurrenceId,
) -> String {
    format!(
        "window:{session_id}:{window_index}:{window_id}:{}",
        occurrence_id.as_u64()
    )
}

pub(super) fn pane_item_id(
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: WindowLinkOccurrenceId,
    pane_id: PaneId,
) -> String {
    format!(
        "pane:{session_id}:{window_index}:{window_id}:{}:{pane_id}",
        occurrence_id.as_u64()
    )
}

pub(super) fn finalize_mode_tree(
    items: BTreeMap<String, ModeTreeItem>,
    roots: Vec<String>,
    mode: &ModeTreeClientState,
) -> ModeTreeBuild {
    let order = tree_order(&items, &roots);
    let visible = roots
        .iter()
        .flat_map(|id| visible_tree_order(&items, id, &mode.expanded))
        .collect::<Vec<_>>();
    ModeTreeBuild {
        items,
        roots,
        order,
        visible,
        no_matches: false,
    }
}

fn tree_order(items: &BTreeMap<String, ModeTreeItem>, roots: &[String]) -> Vec<String> {
    let mut order = Vec::new();
    for root in roots {
        push_tree_order(items, root, &mut order);
    }
    order
}

fn push_tree_order(items: &BTreeMap<String, ModeTreeItem>, id: &str, order: &mut Vec<String>) {
    order.push(id.to_owned());
    if let Some(item) = items.get(id) {
        for child in &item.children {
            push_tree_order(items, child, order);
        }
    }
}

pub(super) fn visible_tree_order(
    items: &BTreeMap<String, ModeTreeItem>,
    id: &str,
    expanded: &BTreeSet<String>,
) -> Vec<String> {
    let mut order = vec![id.to_owned()];
    if let Some(item) = items.get(id) {
        if !item.children.is_empty() && expanded.contains(id) {
            for child in &item.children {
                order.extend(visible_tree_order(items, child, expanded));
            }
        }
    }
    order
}
