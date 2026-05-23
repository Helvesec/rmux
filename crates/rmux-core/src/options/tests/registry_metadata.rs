use super::*;

const FROZEN_OPTIONS_TABLE_PATH: &str = "/opt/rmux/reference/tmux/options-table.c";
const FROZEN_OPTIONS_TABLE_ENV: &str = "RMUX_FROZEN_OPTIONS_TABLE";
const REQUIRE_FROZEN_TMUX_ENV: &str = "RMUX_REQUIRE_FROZEN_TMUX";

#[test]
fn option_registry_is_closed_unique_and_contains_full_frozen_inventory() {
    let metadata = registry::registry();
    let unique_options = metadata
        .iter()
        .map(|entry| entry.option())
        .collect::<HashSet<_>>();
    let unique_names = metadata
        .iter()
        .map(|entry| entry.name())
        .collect::<HashSet<_>>();

    assert_eq!(metadata.len(), 146);
    assert_eq!(unique_options.len(), 146);
    assert_eq!(unique_names.len(), 146);
}

#[test]
fn option_registry_matches_frozen_tmux_option_names() {
    let Some(source) = frozen_options_table_source() else {
        return;
    };
    let tmux_names = source
        .lines()
        .filter_map(|line| {
            let marker = ".name = \"";
            let start = line.find(marker)?;
            let rest = &line[start + marker.len()..];
            let end = rest.find('"')?;
            Some(rest[..end].to_owned())
        })
        .collect::<HashSet<_>>();
    let registry_names = registry::registry()
        .iter()
        .map(|entry| entry.name().to_owned())
        .collect::<HashSet<_>>();

    assert_eq!(tmux_names.len(), 146);
    assert_eq!(registry_names, tmux_names);
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
        registry::SCOPE_WINDOW
    );
    assert_eq!(
        registry::option_metadata(OptionName::PaneActiveBorderStyle).scope_mask(),
        registry::SCOPE_WINDOW
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
        registry::OptionValueType::Choice(&["off", "on"])
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
            assert_eq!(values.len(), 2);
            assert!(values.iter().any(|value| value.contains("#[align=left")));
            assert!(values.iter().any(|value| value.contains("#[align=centre]")));
        }
        default => panic!("unexpected status-format default: {default:?}"),
    }

    let update_environment = registry::option_metadata(OptionName::UpdateEnvironment);
    assert!(update_environment.is_array());
    assert_eq!(update_environment.separator(), " ");
    assert_eq!(
        update_environment.default_value(),
        registry::DefaultValue::Scalar(concat!(
            "DISPLAY KRB5CCNAME SSH_ASKPASS SSH_AUTH_SOCK SSH_",
            "AG",
            "ENT_PID SSH_CONNECTION WINDOWID XAUTHORITY"
        ))
    );
}

#[test]
fn style_parse_effect_inventory_matches_tmux_style_option_count() {
    let Some(source) = frozen_options_table_source() else {
        return;
    };
    let lines = source.lines().collect::<Vec<_>>();
    let tmux_style_options = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.contains("OPTIONS_TABLE_IS_STYLE").then_some(index))
        .filter_map(|index| {
            lines[..=index].iter().rev().find_map(|line| {
                let marker = ".name = \"";
                let start = line.find(marker)?;
                let rest = &line[start + marker.len()..];
                let end = rest.find('"')?;
                Some(rest[..end].to_owned())
            })
        })
        .collect::<HashSet<_>>();
    let registry_style_options = registry::registry()
        .iter()
        .filter(|metadata| metadata.effects().contains(registry::EFFECT_STYLE_PARSE))
        .map(|metadata| metadata.name())
        .map(str::to_owned)
        .collect::<HashSet<_>>();

    assert_eq!(registry_style_options, tmux_style_options);
}

fn frozen_options_table_source() -> Option<String> {
    let path = std::env::var(FROZEN_OPTIONS_TABLE_ENV)
        .unwrap_or_else(|_| FROZEN_OPTIONS_TABLE_PATH.to_owned());
    match fs::read_to_string(&path) {
        Ok(source) => Some(source),
        Err(error) if frozen_tmux_required() => {
            panic!("frozen tmux options-table.c is readable at {path}: {error}")
        }
        Err(error) => {
            eprintln!(
                "skipping frozen tmux options-table assertions: \
                 {path} unavailable: {error}. Set {REQUIRE_FROZEN_TMUX_ENV}=1 \
                 to make this a hard check."
            );
            None
        }
    }
}

fn frozen_tmux_required() -> bool {
    std::env::var(REQUIRE_FROZEN_TMUX_ENV).is_ok_and(|value| {
        matches!(
            value.as_str(),
            "1" | "true" | "TRUE" | "on" | "ON" | "yes" | "YES"
        )
    })
}
