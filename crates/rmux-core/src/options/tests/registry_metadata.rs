use super::*;

#[test]
fn neighbor_server_options_preserve_sibling_values_on_mutation_and_reset() {
    let mut store = OptionStore::new();
    let scope = OptionScopeSelector::ServerGlobal;
    for (name, value) in [
        ("buffer-limit", "2"),
        (
            "command-alias",
            "bank-alias=display-message -p alias-effect",
        ),
    ] {
        store
            .set_by_name(
                scope.clone(),
                name,
                Some(value.into()),
                SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .unwrap();
    }
    assert_eq!(store.resolve(None, OptionName::BufferLimit), Some("2"));
    assert_eq!(
        store.resolve_array_values(None, OptionName::CommandAlias),
        vec!["bank-alias=display-message -p alias-effect".to_owned()]
    );
    store
        .set_by_name(
            scope,
            "buffer-limit",
            None,
            SetOptionMode::Replace,
            false,
            true,
            false,
        )
        .unwrap();
    assert_eq!(store.resolve(None, OptionName::BufferLimit), Some("50"));
    assert_eq!(
        store.resolve_array_values(None, OptionName::CommandAlias),
        vec!["bank-alias=display-message -p alias-effect".to_owned()]
    );
}

mod frozen;
mod named_only;

#[test]
fn option_registry_is_closed_unique_and_contains_full_frozen_inventory() {
    let metadata = registry::registry();
    let unique_options = metadata
        .iter()
        .filter_map(|entry| entry.wire_option())
        .collect::<HashSet<_>>();
    let unique_names = metadata
        .iter()
        .map(|entry| entry.name())
        .collect::<HashSet<_>>();

    assert_eq!(unique_options.len(), 149);
    assert_eq!(unique_names.len(), metadata.len());
}

#[test]
fn option_lookup_indexes_match_registry_metadata() {
    for metadata in registry::registry() {
        if let Some(option) = metadata.wire_option() {
            assert_eq!(registry::option_metadata(option).name(), metadata.name());
        }

        let by_name = registry::resolve_exact_option_name(metadata.name())
            .expect("canonical option name resolves exactly");
        assert_eq!(by_name.known_option(), metadata.wire_option());
        assert_eq!(by_name.canonical_name(), metadata.name());
        assert!(!by_name.is_user());

        for alias in metadata.aliases() {
            let by_alias =
                registry::resolve_exact_option_name(alias).expect("option alias resolves exactly");
            assert_eq!(by_alias.known_option(), metadata.wire_option());
            assert_eq!(by_alias.canonical_name(), metadata.name());
            assert!(!by_alias.is_user());
        }
    }
}

#[test]
fn option_registry_matches_frozen_tmux_option_names() {
    let Some(inventory) = frozen::inventory() else {
        return;
    };
    let registry_names = registry::registry()
        .iter()
        .map(|entry| entry.name().to_owned())
        .collect();

    frozen::difference(&registry_names, &inventory.names).unwrap();
    assert_eq!(inventory.names.len(), 153);
}

#[test]
fn colour_aliases_resolve_before_prefix_matching() {
    for (alias, option, canonical_name) in [
        (
            "display-panes-color",
            OptionName::DisplayPanesColour,
            "display-panes-colour",
        ),
        (
            "display-panes-active-color",
            OptionName::DisplayPanesActiveColour,
            "display-panes-active-colour",
        ),
        (
            "clock-mode-color",
            OptionName::ClockModeColour,
            "clock-mode-colour",
        ),
        ("cursor-color", OptionName::CursorColour, "cursor-colour"),
        (
            "prompt-cursor-color",
            OptionName::PromptCursorColour,
            "prompt-cursor-colour",
        ),
        ("pane-colors", OptionName::PaneColours, "pane-colours"),
    ] {
        let query = resolve_option_name(alias).expect("alias resolves");
        assert_eq!(query.known_option(), Some(option));
        assert_eq!(query.canonical_name(), canonical_name);
    }
}

#[test]
fn frozen_choice_lists_and_scope_masks_match_tmux_inventory() {
    assert_eq!(
        registry::option_metadata(OptionName::StatusJustify).value_type(),
        registry::OptionValueType::Choice(&["left", "centre", "right", "absolute-centre"])
    );
    assert_eq!(
        registry::option_metadata(OptionName::PaneBorderStyle).scope_mask(),
        registry::SCOPE_WINDOW | registry::SCOPE_PANE
    );
    assert_eq!(
        registry::option_metadata(OptionName::PaneActiveBorderStyle).scope_mask(),
        registry::SCOPE_WINDOW | registry::SCOPE_PANE
    );
    assert_eq!(
        registry::option_metadata(OptionName::WindowStyle).scope_mask(),
        registry::SCOPE_WINDOW | registry::SCOPE_PANE
    );
    assert_eq!(
        registry::option_metadata(OptionName::CursorColour).scope_mask(),
        registry::SCOPE_WINDOW | registry::SCOPE_PANE
    );
    assert_eq!(
        registry::option_metadata(OptionName::AllowPassthrough).scope_mask(),
        registry::SCOPE_WINDOW | registry::SCOPE_PANE
    );
    assert_eq!(
        registry::option_metadata(OptionName::AllowPassthrough).value_type(),
        registry::OptionValueType::Choice(&["off", "on", "all"])
    );
}

#[test]
fn style_and_array_metadata_capture_tmux_specific_defaults() {
    assert!(registry::option_metadata(OptionName::ModeStyle)
        .effects()
        .contains(registry::EFFECT_STYLE_PARSE));
    assert!(registry::option_metadata(OptionName::StatusStyle)
        .effects()
        .contains(registry::EFFECT_STYLE_PARSE));

    let status_format = registry::option_metadata(OptionName::StatusFormat);
    assert!(status_format.is_array());
    match status_format.default_value() {
        registry::DefaultValue::Array(values) => {
            assert_eq!(values.len(), 3);
            assert!(values.iter().any(|value| value.contains("#[align=left")));
            assert!(values.iter().any(|value| value.contains("#{P:")));
            assert!(values.iter().any(|value| value.contains("#{S:")));
        }
        default => panic!("unexpected status-format default: {default:?}"),
    }

    assert_eq!(
        registry::option_metadata(OptionName::CopyModeLineNumbers).default_value(),
        registry::DefaultValue::Scalar("off")
    );
    assert_eq!(
        registry::option_metadata(OptionName::CopyModeLineNumberStyle).default_value(),
        registry::DefaultValue::Scalar("fg=white,dim")
    );
    assert_eq!(
        registry::option_metadata(OptionName::CopyModeCurrentLineNumberStyle).default_value(),
        registry::DefaultValue::Scalar("fg=yellow")
    );

    let update_environment = registry::option_metadata(OptionName::UpdateEnvironment);
    assert!(update_environment.is_array());
    assert_eq!(update_environment.separator(), " ,");
    assert_eq!(
        update_environment.default_value(),
        registry::DefaultValue::Scalar(concat!(
            "DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK ",
            "SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID ",
            "XAUTHORITY XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE"
        ))
    );
}

#[test]
fn style_parse_effect_inventory_matches_tmux_style_option_count() {
    let Some(inventory) = frozen::inventory() else {
        return;
    };
    let registry_style_options = registry::registry()
        .iter()
        .filter(|metadata| metadata.effects().contains(registry::EFFECT_STYLE_PARSE))
        .map(|metadata| metadata.name())
        .map(str::to_owned)
        .collect();

    frozen::difference(&registry_style_options, &inventory.styles).unwrap();
    assert_eq!(inventory.styles.len(), 33);
}

// Authenticated test-only export; selecting this test makes its inputs mandatory.
#[test]
#[ignore = "requires RMUX_FROZEN_OPTIONS_TABLE and RMUX_OPTIONS_BANK_OUTPUT"]
fn generated_options_bank_inventory() {
    use super::super::registry::{DefaultValue, OptionValueType};
    use serde_json::json;
    use sha2::{Digest, Sha256};

    let output =
        std::env::var_os("RMUX_OPTIONS_BANK_OUTPUT").expect("bank export path is required");
    let oracle = frozen::inventory().expect("authenticated tmux source is required");
    let oracle_path = std::env::var_os("RMUX_FROZEN_OPTIONS_TABLE")
        .expect("explicit authenticated oracle path is required");
    let oracle_bytes = std::fs::read(&oracle_path).expect("oracle source readable");
    let entries = registry::registry().iter().map(|entry| {
        let value_type = match entry.value_type() {
            OptionValueType::String => json!({"kind":"string"}),
            OptionValueType::Number { minimum, maximum } => json!({"kind":"number", "minimum":minimum, "maximum":maximum}),
            OptionValueType::Key => json!({"kind":"key"}),
            OptionValueType::Colour => json!({"kind":"colour"}),
            OptionValueType::Flag => json!({"kind":"flag"}),
            OptionValueType::Choice(choices) => json!({"kind":"choice", "choices":choices}),
            OptionValueType::Command => json!({"kind":"command"}),
        };
        let default = match entry.default_value() {
            DefaultValue::Scalar(value) => json!({"kind":"scalar", "value":value}),
            DefaultValue::Array(values) => json!({"kind":"array", "values":values}),
        };
        let wire = entry.wire_option().map(|option| format!("{option:?}"));
        let id = wire.as_ref().map(|symbol| format!("wire:{symbol}"))
            .unwrap_or_else(|| format!("named:{}", entry.name()));
        let scopes = [("server",registry::SCOPE_SERVER), ("session",registry::SCOPE_SESSION),
            ("window",registry::SCOPE_WINDOW), ("pane",registry::SCOPE_PANE)];
        let supported = scopes.iter().filter(|(_,bit)| entry.scope_mask() & bit != 0)
            .map(|(name,_)| *name).collect::<Vec<_>>();
        let visible = scopes.iter().filter(|(_,bit)| entry.visible_in(*bit))
            .map(|(name,_)| *name).collect::<Vec<_>>();
        let exact = registry::resolve_exact_option_name(entry.name()).expect("name lookup");
        assert_eq!(exact.known_option(), entry.wire_option());
        let store = OptionStore::new();
        let scope = match entry.global_root() {
            registry::GlobalRoot::Server => OptionScopeSelector::ServerGlobal,
            registry::GlobalRoot::Session => OptionScopeSelector::SessionGlobal,
            registry::GlobalRoot::Window => OptionScopeSelector::WindowGlobal,
        };
        let values = store.show_options_lines_filtered(&scope, Some(entry.name()), true)
            .expect("default value consumer");
        let named = store.show_options_lines_filtered(&scope, Some(entry.name()), false)
            .expect("default named consumer");
        let alias_consumers = entry.aliases().iter().map(|alias| {
            assert_eq!(store.show_options_lines_filtered(&scope, Some(alias), true).unwrap(), values);
            *alias
        }).collect::<Vec<_>>();
        let alpha = session_name("bank");
        let selectors = [
            ("server", OptionScopeSelector::ServerGlobal),
            ("session", OptionScopeSelector::SessionGlobal),
            ("window", OptionScopeSelector::WindowGlobal),
            ("pane", OptionScopeSelector::Pane(PaneTarget::with_window(alpha, 0, 0))),
        ];
        let validated_scopes = selectors.iter().filter(|(_, selector)|
            exact.supports_scope(selector)).map(|(label,_)| *label).collect::<Vec<_>>();
        assert_eq!(default_global_scope_for_option_name(entry.name()).unwrap(), scope);
        let shown_scopes = selectors.iter().filter(|(_, selector)| {
            store.show_options_lines_with_mode(selector, false,
                ShowOptionsMode::ResolvedWithInheritanceMarkers).unwrap().iter().any(|line| {
                    let shown_name = line.split(' ').next().unwrap().split('[').next().unwrap()
                        .trim_end_matches('*');
                    shown_name == entry.name()
                })
        }).map(|(label,_)| *label).collect::<Vec<_>>();
        json!({"id":id,"name":entry.name(),"wire_symbol":wire,"aliases":entry.aliases(),
            "value_type":value_type,"default":default,"scopes":supported,"show_scopes":visible,
            "scope_mask":entry.scope_mask(),"global_root":format!("{:?}",entry.global_root()),
            "is_array":entry.is_array(),"separator":entry.separator(),
            "is_style":entry.effects().contains(registry::EFFECT_STYLE_PARSE),
            "default_consumer":{"values":values,"named":named,"aliases":alias_consumers,
                "allowed_scopes":validated_scopes,"show_scopes":shown_scopes,
                "source":"OptionStore show defaults/listings, OptionQuery::supports_scope, default_global_scope_for_option_name"},
            "declared_effect_mask":format!("{:?}",entry.effects()),
            "consumer":{"status":"FAIL_MISSING","reason":"Registry change mask is not a real consumer effect proof"}})
    }).collect::<Vec<_>>();
    let ids = entries
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<HashSet<_>>();
    let names = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect::<HashSet<_>>();
    assert_eq!(entries.len(), ids.len());
    assert_eq!(entries.len(), names.len());
    let table = include_bytes!("../registry/table.rs");
    let document = json!({"schema":1,"registry_symbol":"registry::table::OPTIONS",
        "target_os":std::env::consts::OS,"registry_count":entries.len(),
        "table":{"path":"crates/rmux-core/src/options/registry/table.rs","bytes":table.len(),
            "sha256":format!("{:x}",Sha256::digest(table))},"options":entries,
        "oracle":{"version":"tmux 3.7b","source_sha":"e802909de06012a4df6209d55e86487c56223163",
            "archive_sha256":"87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96",
            "symbol":"options_table[]","source_bytes":oracle_bytes.len(),
            "source_sha256":format!("{:x}",Sha256::digest(&oracle_bytes)),
            "ordinary_names":oracle.names,"style_names":oracle.styles,
            "scope":"ordinary options; hook macros remain owned by hook contract"}});
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .expect("fresh output path");
    file.write_all(serde_json::to_string_pretty(&document).unwrap().as_bytes())
        .unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

#[test]
fn every_numeric_option_enforces_its_independent_table_range() {
    for metadata in registry::registry() {
        let registry::OptionValueType::Number { minimum, maximum } = metadata.value_type() else {
            continue;
        };
        let scope = match metadata.global_root() {
            registry::GlobalRoot::Server => OptionScopeSelector::ServerGlobal,
            registry::GlobalRoot::Session => OptionScopeSelector::SessionGlobal,
            registry::GlobalRoot::Window => OptionScopeSelector::WindowGlobal,
        };
        for value in [minimum, maximum] {
            let mut store = OptionStore::new();
            store
                .set_by_name(
                    scope.clone(),
                    metadata.name(),
                    Some(value.to_string()),
                    SetOptionMode::Replace,
                    false,
                    false,
                    false,
                )
                .unwrap();
            assert_eq!(
                store
                    .show_options_lines_filtered(&scope, Some(metadata.name()), true)
                    .unwrap(),
                vec![value.to_string()]
            );
        }
        for value in [i64::from(minimum) - 1, i64::from(maximum) + 1] {
            assert!(
                OptionStore::new()
                    .set_by_name(
                        scope.clone(),
                        metadata.name(),
                        Some(value.to_string()),
                        SetOptionMode::Replace,
                        false,
                        false,
                        false
                    )
                    .is_err(),
                "{} accepted {}",
                metadata.name(),
                value
            );
        }
    }
}

#[test]
fn array_separator_is_a_character_set_and_preserves_other_bytes() {
    for (name, input, expected) in [
        ("update-environment", "A B,C", vec!["A", "B", "C"]),
        (
            "pane-colours",
            "red blue,green",
            vec!["red", "blue", "green"],
        ),
        (
            "status-format",
            "one two,three",
            vec!["one", "two", "three"],
        ),
        ("command-alias", "a=one, b=two", vec!["a=one", " b=two"]),
    ] {
        let query = resolve_option_name(name).unwrap();
        let actual = super::super::mutation::split_array_assignment(input, query.separator());
        assert_eq!(actual, expected, "{name}");
    }
}
