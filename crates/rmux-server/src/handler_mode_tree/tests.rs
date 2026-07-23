use super::super::scripting_support::QueueExecutionContext;
use super::super::{DetachedRequesterAuthority, RequesterOrigin};
use super::mode_tree_build::tree_item_display_line;
use super::mode_tree_model::{
    ModeTreeAction, ModeTreeBuild, ModeTreeItem, PreviewMode, SearchDirection, SearchState,
    SortOrder, TreeDepth,
};
use super::mode_tree_preview::{
    preview_horizontal_offset, preview_lines_for_screen, preview_vertical_offset,
    render_preview_segment_row, render_tree_columns_preview, tree_preview_layout,
    tree_window_preview_label, PreviewColumn,
};
use super::mode_tree_render::{mode_tree_list_rows, render_mode_tree_overlay, render_visible_item};
use super::mode_tree_selection::{
    clamp_scroll, collapse_or_parent, current_tree_kill_prompt, cycle_sort,
    ensure_selected_visible, expand_or_child, move_selection, repeat_search, selected_items,
    tag_all, tagged_tree_kill_prompt, toggle_tag,
};
use super::mode_tree_sort::stable_order;
use super::*;
use crate::pane_terminals::HandlerState;
use rmux_core::{command_parser::CommandParser, input::InputParser, Screen, Style, Utf8Config};
use rmux_proto::{
    NewSessionRequest, OptionName, PaneTarget, Request, Response, ScopeSelector, SessionName,
    SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
    TerminalSize,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::mpsc;

fn test_origin() -> RequesterOrigin {
    RequesterOrigin::new(std::process::id(), DetachedRequesterAuthority::Denied)
}

fn test_mode(list_rows: usize) -> ModeTreeClientState {
    ModeTreeClientState {
        origin: test_origin(),
        kind: ModeTreeKind::Tree,
        session_name: SessionName::new("test").expect("valid session"),
        session_id: rmux_proto::SessionId::new(1),
        host_pane: None,
        host_identity: None,
        host_transcript: None,
        preview_mode: PreviewMode::Off,
        row_format: None,
        filter_format: None,
        filter_text: None,
        key_format: DEFAULT_KEY_FORMAT.to_owned(),
        template: None,
        search: None,
        tagged: BTreeSet::new(),
        expanded: BTreeSet::new(),
        selected_id: None,
        scroll: 0,
        preview_scroll: 0,
        sort_order: None,
        order_seq: vec![SortOrder::Index, SortOrder::Name, SortOrder::Activity],
        reversed: false,
        tree_depth: TreeDepth::Pane,
        show_all_group_members: false,
        auto_accept: false,
        zoom_restore: None,
        last_list_rows: list_rows,
    }
}

fn flat_build(ids: &[&str]) -> ModeTreeBuild {
    let mut items = BTreeMap::new();
    let roots: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    for id in ids {
        items.insert(
            id.to_string(),
            ModeTreeItem {
                id: id.to_string(),
                parent: None,
                children: Vec::new(),
                depth: 0,
                line: id.to_string(),
                search_text: id.to_string(),
                preview: Vec::new(),
                no_tag: false,
                action: ModeTreeAction::None,
            },
        );
    }
    ModeTreeBuild {
        items,
        roots: roots.clone(),
        order: roots.clone(),
        visible: roots,
        no_matches: false,
    }
}

#[path = "tests/parse_and_tag.rs"]
mod parse_and_tag;

#[path = "tests/render_items.rs"]
mod render_items;

#[path = "tests/selection_scroll.rs"]
mod selection_scroll;

#[path = "tests/preview_rendering.rs"]
mod preview_rendering;

#[path = "tests/tags_search_parse.rs"]
mod tags_search_parse;

#[path = "tests/tree_navigation.rs"]
mod tree_navigation;

#[path = "tests/async_acceptance.rs"]
mod async_acceptance;

#[path = "tests/window_occurrence_identity.rs"]
mod window_occurrence_identity;

#[path = "tests/client_identity.rs"]
mod client_identity;

#[path = "tests/deferred_confirmation.rs"]
mod deferred_confirmation;
