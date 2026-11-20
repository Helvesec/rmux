//! Internal display/execution contract; no tmux version parity is asserted.

use super::{
    key_string_lookup_string, parse_binding_command_tokens, KeyBindingSortOrder, KeyBindingStore,
};
use crate::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use crate::formats::{render_template, FormatVariable, FormatVariables};

struct MenuContext {
    floating: bool,
    panes: usize,
}

impl FormatVariables for MenuContext {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        self.format_value_by_name(variable.name())
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        match name {
            "pane_floating_flag" => Some(usize::from(self.floating).to_string()),
            "window_panes" => Some(self.panes.to_string()),
            _ => None,
        }
    }
}

fn menu(commands: &ParsedCommands) -> &ParsedCommand {
    for command in commands.commands() {
        if command.name() == "display-menu" {
            return command;
        }
        for argument in command.arguments() {
            if let CommandArgument::Commands(nested) = argument {
                if nested
                    .commands()
                    .iter()
                    .any(|item| item.name() == "display-menu")
                {
                    return menu(nested);
                }
            }
        }
    }
    panic!("binding contains a display-menu AST");
}

fn split_swap_items(commands: &ParsedCommands, context: &MenuContext) -> Vec<(String, String)> {
    let arguments = menu(commands).arguments();
    ["h", "v", "u", "d"]
        .into_iter()
        .map(|key| {
            let item = arguments
                .windows(3)
                .find(|item| {
                    item[1].as_string() == Some(key)
                        && matches!(&item[2], CommandArgument::Commands(action)
                        if action.commands().first().is_some_and(|command|
                            command.name() == if ["h", "v"].contains(&key) {
                                "split-window"
                            } else {
                                "swap-pane"
                            }))
                })
                .expect("split/swap menu item");
            let CommandArgument::Commands(action) = &item[2] else {
                unreachable!();
            };
            (
                render_template(item[0].as_string().expect("label"), context),
                action.to_tmux_string(),
            )
        })
        .collect()
}

fn assert_menu_display_contract(table: &str, key_name: &str) {
    let store = KeyBindingStore::default();
    let key = key_string_lookup_string(key_name).expect("key");
    // Attached input takes this active binding's commands; list-keys and
    // customize take the displayed command_string from the same store.
    let active = store.get_binding(table, key).expect("active binding");
    let rows = store.list_bindings(Some(table), KeyBindingSortOrder::Key, false);
    let row = rows
        .iter()
        .find(|row| row.key_string() == key_name)
        .expect("display row");
    assert_eq!(row.binding(), active);
    let displayed = parse_binding_command_tokens(&[row.command_string().to_owned()])
        .expect("listed command parses for save/replay");
    let mut replay = KeyBindingStore::new();
    assert!(replay.add_binding(table, key, None, active.repeat(), Some(displayed.clone())));
    let replayed = replay.get_binding(table, key).expect("replayed binding");

    for floating in [false, true] {
        for panes in [1, 2] {
            let context = MenuContext { floating, panes };
            let swap_prefix = if panes > 1 { "" } else { "-" };
            let expected = vec![
                ("Horizontal Split".to_owned(), "split-window -h".to_owned()),
                ("Vertical Split".to_owned(), "split-window -v".to_owned()),
                (format!("{swap_prefix}Swap Up"), "swap-pane -U".to_owned()),
                (format!("{swap_prefix}Swap Down"), "swap-pane -D".to_owned()),
            ];
            assert_eq!(split_swap_items(active.commands(), &context), expected);
            assert_eq!(
                split_swap_items(&displayed, &context),
                expected,
                "{table} {key_name}: floating={floating}, window_panes={panes}"
            );
            assert_eq!(split_swap_items(replayed.commands(), &context), expected);
        }
    }
}

#[test]
fn prefix_menu_display_matches_executed_and_replayed_ast() {
    assert_menu_display_contract("prefix", ">");
}

#[test]
fn mouse_menu_display_matches_executed_and_replayed_ast() {
    assert_menu_display_contract("root", "MouseDown3Pane");
}

#[test]
fn meta_mouse_menu_display_matches_executed_and_replayed_ast() {
    assert_menu_display_contract("root", "M-MouseDown3Pane");
}

#[test]
fn rebound_menus_and_escape_alias_rows_keep_their_display_contract() {
    let mut store = KeyBindingStore::default();
    let before = store.list_bindings(None, KeyBindingSortOrder::Key, false);
    for (table, name) in [
        ("prefix", ">"),
        ("root", "MouseDown3Pane"),
        ("root", "M-MouseDown3Pane"),
    ] {
        let key = key_string_lookup_string(name).expect("key");
        let custom = parse_binding_command_tokens(&[
            "display-menu 'Custom action' h { display-message 'saved value' }".to_owned(),
        ])
        .expect("custom command");
        assert!(store.add_binding(table, key, None, false, Some(custom.clone())));
        let rows = store.list_bindings(Some(table), KeyBindingSortOrder::Key, false);
        let row = rows
            .iter()
            .find(|row| row.key_string() == name)
            .expect("custom row");
        assert_eq!(row.command_string(), custom.to_tmux_binding_string());
        assert_eq!(row.binding().commands(), &custom);
        let saved =
            parse_binding_command_tokens(&[row.command_string().to_owned()]).expect("replay");
        assert_eq!(saved.to_tmux_string(), custom.to_tmux_string());
    }
    let after = store.list_bindings(None, KeyBindingSortOrder::Key, false);
    for table in ["copy-mode", "copy-mode-vi"] {
        let aliases = |rows: &[super::KeyBindingDisplay]| {
            rows.iter()
                .filter(|row| {
                    row.table_name() == table && ["Escape", "C-["].contains(&row.key_string())
                })
                .map(|row| {
                    (
                        row.key_string().to_owned(),
                        row.command_string().to_owned(),
                        row.binding().clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let original = aliases(&before);
        assert_eq!(original.len(), 2);
        assert_eq!(original[0].2, original[1].2);
        assert_eq!(aliases(&after), original);
    }
    let reverse = store.list_bindings(None, KeyBindingSortOrder::Key, true);
    assert_eq!(reverse, after.into_iter().rev().collect::<Vec<_>>());
}
