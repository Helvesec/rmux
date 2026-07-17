use super::mode_tree_model::{
    ModeTreeAction, ModeTreeBuild, ModeTreeClientState, ModeTreeItem, SearchDirection,
};

pub(super) fn selected_items<'a>(
    mode: &'a ModeTreeClientState,
    build: &'a ModeTreeBuild,
) -> Vec<&'a ModeTreeItem> {
    let mut tagged = build
        .items
        .values()
        .filter(|item| mode.tagged.contains(&item.id))
        .collect::<Vec<_>>();
    if tagged.is_empty() && mode.tagged.is_empty() {
        if let Some(selected) = mode.selected_id.as_ref().and_then(|id| build.items.get(id)) {
            tagged.push(selected);
        }
    }
    tagged
}

pub(super) fn current_selected_item<'a>(
    mode: &'a ModeTreeClientState,
    build: &'a ModeTreeBuild,
) -> Option<&'a ModeTreeItem> {
    mode.selected_id.as_ref().and_then(|id| build.items.get(id))
}

pub(super) fn current_tree_kill_prompt(
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
) -> Option<String> {
    current_selected_item(mode, build).and_then(tree_kill_prompt_for_item)
}

pub(super) fn tagged_tree_kill_prompt(mode: &ModeTreeClientState) -> Option<String> {
    let tagged = mode.tagged.len();
    (tagged != 0).then(|| format!("Kill {tagged} tagged? "))
}

fn tree_kill_prompt_for_item(item: &ModeTreeItem) -> Option<String> {
    match &item.action {
        ModeTreeAction::TreeTarget {
            session_name,
            window_index: None,
            ..
        } => Some(format!("Kill session {session_name}? ")),
        ModeTreeAction::TreeTarget {
            window_index: Some(window_index),
            pane_index: None,
            ..
        } => Some(format!("Kill window {window_index}? ")),
        ModeTreeAction::TreeTarget {
            pane_index: Some(pane_index),
            ..
        } => Some(format!("Kill pane {pane_index}? ")),
        _ => None,
    }
}

pub(super) fn move_selection(
    mode: &mut ModeTreeClientState,
    build: &ModeTreeBuild,
    delta: isize,
    wrap: bool,
) {
    let Some(current) = mode
        .selected_id
        .as_ref()
        .and_then(|id| build.visible.iter().position(|visible| visible == id))
    else {
        mode.selected_id = build.visible.first().cloned();
        mode.scroll = 0;
        mode.preview_scroll = 0;
        return;
    };
    let last = build.visible.len().saturating_sub(1);
    let next = if wrap && delta < 0 && current == 0 {
        last
    } else if wrap && delta > 0 && current == last {
        0
    } else {
        current.saturating_add_signed(delta).min(last)
    };
    let changed = mode.selected_id.as_ref() != build.visible.get(next);
    mode.selected_id = build.visible.get(next).cloned();
    if changed {
        mode.preview_scroll = 0;
    }
    ensure_selected_visible(mode, next);
}

pub(super) fn select_edge(mode: &mut ModeTreeClientState, build: &ModeTreeBuild, end: bool) {
    let index = if end {
        build.visible.len().saturating_sub(1)
    } else {
        0
    };
    mode.selected_id = build.visible.get(index).cloned();
    mode.preview_scroll = 0;
    ensure_selected_visible(mode, index);
}

pub(super) fn collapse_or_parent(mode: &mut ModeTreeClientState, build: &ModeTreeBuild) {
    let Some(selected_id) = mode.selected_id.as_ref() else {
        return;
    };
    if mode.expanded.remove(selected_id) {
        return;
    }
    if let Some(parent) = build
        .items
        .get(selected_id)
        .and_then(|item| item.parent.clone())
    {
        mode.selected_id = Some(parent.clone());
        mode.preview_scroll = 0;
        if let Some(index) = build.visible.iter().position(|id| id == &parent) {
            ensure_selected_visible(mode, index);
        }
    }
}

pub(super) fn expand_or_child(mode: &mut ModeTreeClientState, build: &ModeTreeBuild) {
    let Some(selected_id) = mode.selected_id.as_ref() else {
        return;
    };
    let Some(item) = build.items.get(selected_id) else {
        return;
    };
    if !item.children.is_empty() && !mode.expanded.contains(selected_id) {
        mode.expanded.insert(selected_id.clone());
        return;
    }
    if let Some(first_child) = item
        .children
        .iter()
        .find(|child| build.items.contains_key(child.as_str()))
    {
        mode.selected_id = Some(first_child.clone());
        mode.preview_scroll = 0;
        if let Some(index) = build.visible.iter().position(|id| id == first_child) {
            ensure_selected_visible(mode, index);
        }
    }
}

pub(super) fn toggle_tag(mode: &mut ModeTreeClientState, build: &ModeTreeBuild) {
    let Some(selected_id) = mode.selected_id.clone() else {
        return;
    };
    let Some(item) = build.items.get(&selected_id) else {
        return;
    };
    if item.no_tag {
        return;
    }
    if mode.tagged.remove(&selected_id) {
        return;
    }
    untag_ancestors(mode, build, &selected_id);
    untag_descendants(mode, build, &selected_id);
    mode.tagged.insert(selected_id);
}

pub(super) fn tag_all(mode: &mut ModeTreeClientState, build: &ModeTreeBuild) {
    for root in &build.roots {
        tag_all_from(mode, build, root);
    }
}

fn tag_all_from(mode: &mut ModeTreeClientState, build: &ModeTreeBuild, id: &str) {
    let Some(item) = build.items.get(id) else {
        return;
    };
    if item.no_tag {
        for child in &item.children {
            tag_all_from(mode, build, child);
        }
    } else {
        mode.tagged.insert(id.to_owned());
    }
}

fn untag_ancestors(mode: &mut ModeTreeClientState, build: &ModeTreeBuild, id: &str) {
    let mut current = build.items.get(id).and_then(|item| item.parent.clone());
    while let Some(parent) = current {
        mode.tagged.remove(&parent);
        current = build
            .items
            .get(&parent)
            .and_then(|item| item.parent.clone());
    }
}

fn untag_descendants(mode: &mut ModeTreeClientState, build: &ModeTreeBuild, id: &str) {
    let Some(item) = build.items.get(id) else {
        return;
    };
    for child in &item.children {
        mode.tagged.remove(child);
        untag_descendants(mode, build, child);
    }
}

pub(super) fn repeat_search(mode: &mut ModeTreeClientState, build: &ModeTreeBuild, reverse: bool) {
    let Some(search) = mode.search.clone() else {
        return;
    };
    let direction = if reverse {
        match search.direction {
            SearchDirection::Forward => SearchDirection::Backward,
            SearchDirection::Backward => SearchDirection::Forward,
        }
    } else {
        search.direction
    };
    let Some(current_index) = mode
        .selected_id
        .as_ref()
        .and_then(|id| build.order.iter().position(|item| item == id))
    else {
        return;
    };

    let smart_case = search
        .value
        .chars()
        .all(|ch| !ch.is_ascii_alphabetic() || ch.is_ascii_lowercase());
    let matches = |text: &str| {
        if smart_case {
            text.to_ascii_lowercase()
                .contains(&search.value.to_ascii_lowercase())
        } else {
            text.contains(&search.value)
        }
    };

    let len = build.order.len();
    for offset in 1..=len {
        let index = match direction {
            SearchDirection::Forward => (current_index + offset) % len,
            SearchDirection::Backward => (current_index + len - (offset % len)) % len,
        };
        let id = &build.order[index];
        if build
            .items
            .get(id)
            .is_some_and(|item| matches(&item.search_text))
        {
            expand_parents(mode, build, id);
            mode.selected_id = Some(id.clone());
            mode.preview_scroll = 0;
            // After expanding parents the visible list may be stale;
            // recompute to find the correct position for scroll adjustment.
            let fresh_visible: Vec<String> = build
                .roots
                .iter()
                .flat_map(|root| {
                    super::mode_tree_order::visible_tree_order(&build.items, root, &mode.expanded)
                })
                .collect();
            if let Some(visible_index) = fresh_visible.iter().position(|visible| visible == id) {
                ensure_selected_visible(mode, visible_index);
            } else {
                mode.scroll = 0;
            }
            return;
        }
    }
}

fn expand_parents(mode: &mut ModeTreeClientState, build: &ModeTreeBuild, id: &str) {
    let mut current = build.items.get(id).and_then(|item| item.parent.clone());
    while let Some(parent) = current {
        mode.expanded.insert(parent.clone());
        current = build
            .items
            .get(&parent)
            .and_then(|item| item.parent.clone());
    }
}

pub(super) fn cycle_sort(mode: &mut ModeTreeClientState) {
    if mode.order_seq.is_empty() {
        return;
    }
    let current = mode
        .sort_order
        .and_then(|sort_order| {
            mode.order_seq
                .iter()
                .position(|candidate| candidate == &sort_order)
        })
        .unwrap_or(0);
    mode.sort_order = Some(mode.order_seq[(current + 1) % mode.order_seq.len()]);
    mode.scroll = 0;
}

pub(super) fn ensure_selected_visible(mode: &mut ModeTreeClientState, index: usize) {
    if index < mode.scroll {
        mode.scroll = index;
    } else if mode.last_list_rows > 0 && index >= mode.scroll + mode.last_list_rows {
        mode.scroll = index.saturating_sub(mode.last_list_rows.saturating_sub(1));
    }
}

pub(super) fn clamp_scroll(mode: &mut ModeTreeClientState, build: &ModeTreeBuild) {
    if build.visible.is_empty() {
        mode.scroll = 0;
        return;
    }
    let max_scroll = build.visible.len().saturating_sub(1);
    if mode.scroll > max_scroll {
        mode.scroll = max_scroll;
    }
    if let Some(selected) = mode
        .selected_id
        .as_ref()
        .and_then(|id| build.visible.iter().position(|visible| visible == id))
    {
        if selected < mode.scroll {
            mode.scroll = selected;
        }
    }
}

pub(super) fn normalize_selection(mode: &mut ModeTreeClientState, build: &ModeTreeBuild) {
    if mode.selected_id.is_none() {
        mode.selected_id = build.visible.first().cloned();
    }
}
