//! Customize-mode item construction for options and key bindings.

use std::collections::BTreeMap;

use rmux_core::{formats::FormatContext, KeyBindingDisplay, ShowOptionsMode};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneTarget, RmuxError, WindowTarget};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{session_not_found, HandlerState};

use super::mode_tree_filter::matches_mode_tree_filter;
use super::mode_tree_model::{ModeTreeAction, ModeTreeBuild, ModeTreeClientState, ModeTreeItem};
use super::mode_tree_order::finalize_mode_tree;
use super::mode_tree_sort::split_name_value;

pub(super) fn build_customize_items(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    attach_pid: u32,
) -> Result<ModeTreeBuild, RmuxError> {
    let session = state
        .sessions
        .session(&mode.session_name)
        .ok_or_else(|| session_not_found(&mode.session_name))?;
    let window_target =
        WindowTarget::with_window(mode.session_name.clone(), session.active_window_index());
    let pane_target = PaneTarget::with_window(
        mode.session_name.clone(),
        session.active_window_index(),
        session.active_pane_index(),
    );

    let mut roots = Vec::new();
    let mut items = BTreeMap::new();

    let categories = vec![
        (
            "customize:server".to_owned(),
            "Server Options".to_owned(),
            OptionScopeSelector::ServerGlobal,
        ),
        (
            "customize:session".to_owned(),
            "Session Options".to_owned(),
            OptionScopeSelector::Session(mode.session_name.clone()),
        ),
        (
            "customize:window".to_owned(),
            "Window Options".to_owned(),
            OptionScopeSelector::Window(window_target.clone()),
        ),
        (
            "customize:pane".to_owned(),
            "Pane Options".to_owned(),
            OptionScopeSelector::Pane(pane_target.clone()),
        ),
    ];

    for (category_id, label, scope_selector) in categories {
        roots.push(category_id.clone());
        let lines = state.options.show_options_lines_with_mode(
            &scope_selector,
            false,
            ShowOptionsMode::Resolved,
        )?;
        let mut child_ids = Vec::new();
        for line in lines {
            let (name, value) = split_name_value(&line);
            let item_id = format!("{category_id}:{line}");
            let context = RuntimeFormatContext::new(FormatContext::new())
                .with_state(state)
                .with_named_value("customize_name", name.clone())
                .with_named_value("customize_value", value.clone())
                .with_named_value("customize_scope", category_id.clone());
            let rendered = render_runtime_template(
                mode.row_format
                    .as_deref()
                    .unwrap_or("#{customize_name} #{customize_value}"),
                &context,
                true,
            );
            let line_text = if rendered.is_empty() {
                line.clone()
            } else {
                rendered
            };
            let filter = mode
                .filter_format
                .as_deref()
                .map(|template| render_runtime_template(template, &context, false));
            if !matches_mode_tree_filter(mode, &line_text, filter.as_deref()) {
                continue;
            }
            child_ids.push(item_id.clone());
            items.insert(
                item_id.clone(),
                ModeTreeItem {
                    id: item_id,
                    parent: Some(category_id.clone()),
                    children: Vec::new(),
                    depth: 1,
                    line: line_text.clone(),
                    search_text: line_text,
                    preview: vec![
                        format!("scope {}", category_id),
                        format!("name {}", name),
                        format!("value {}", value),
                    ],
                    no_tag: false,
                    action: ModeTreeAction::CustomizeOption {
                        scope: scope_selector.clone(),
                        name,
                    },
                },
            );
        }
        items.insert(
            category_id.clone(),
            ModeTreeItem {
                id: category_id.clone(),
                parent: None,
                children: child_ids,
                depth: 0,
                line: label,
                search_text: category_id.clone(),
                preview: Vec::new(),
                no_tag: true,
                action: ModeTreeAction::None,
            },
        );
    }

    let keys_root = "customize:keys".to_owned();
    roots.push(keys_root.clone());
    let table_ids = state
        .key_bindings
        .tables()
        .map(|table| format!("{keys_root}:{}", table.name()))
        .collect::<Vec<_>>();
    items.insert(
        keys_root.clone(),
        ModeTreeItem {
            id: keys_root.clone(),
            parent: None,
            children: table_ids.clone(),
            depth: 0,
            line: format!("Key Bindings ({attach_pid})"),
            search_text: "keys".to_owned(),
            preview: Vec::new(),
            no_tag: true,
            action: ModeTreeAction::None,
        },
    );
    for table in state.key_bindings.tables() {
        let table_id = format!("{keys_root}:{}", table.name());
        let bindings = state.key_bindings.list_bindings(
            Some(table.name()),
            rmux_core::KeyBindingSortOrder::Key,
            false,
        );
        let mut binding_ids = Vec::new();
        for binding in bindings {
            let item = customize_key_item(mode, state, &table_id, &binding);
            if matches_mode_tree_filter(mode, &item.line, None) {
                binding_ids.push(item.id.clone());
                items.insert(item.id.clone(), item);
            }
        }
        items.insert(
            table_id.clone(),
            ModeTreeItem {
                id: table_id.clone(),
                parent: Some(keys_root.clone()),
                children: binding_ids,
                depth: 1,
                line: table.name().to_owned(),
                search_text: table.name().to_owned(),
                preview: vec![format!("references {}", table.references())],
                no_tag: true,
                action: ModeTreeAction::None,
            },
        );
    }

    Ok(finalize_mode_tree(items, roots, mode))
}

fn customize_key_item(
    mode: &ModeTreeClientState,
    state: &HandlerState,
    table_id: &str,
    binding: &KeyBindingDisplay,
) -> ModeTreeItem {
    let context = RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_named_value("customize_name", binding.key_string())
        .with_named_value("customize_value", binding.command_string())
        .with_named_value("customize_scope", binding.table_name());
    let rendered = render_runtime_template(
        mode.row_format
            .as_deref()
            .unwrap_or("#{customize_name} #{customize_value}"),
        &context,
        true,
    );
    let line = if rendered.is_empty() {
        format!("{} {}", binding.key_string(), binding.command_string())
    } else {
        rendered
    };
    ModeTreeItem {
        id: format!("{table_id}:{}", binding.key_string()),
        parent: Some(table_id.to_owned()),
        children: Vec::new(),
        depth: 2,
        line: line.clone(),
        search_text: line,
        preview: vec![
            format!("table {}", binding.table_name()),
            format!("key {}", binding.key_string()),
            format!("command {}", binding.command_string()),
        ],
        no_tag: false,
        action: ModeTreeAction::CustomizeKey {
            table_name: binding.table_name().to_owned(),
            key: binding.binding().key(),
            key_string: binding.key_string().to_owned(),
        },
    }
}
