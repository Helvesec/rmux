use std::collections::BTreeSet;
use std::error::Error;

use rmux_core::command_inventory::{
    list_command_names, render_list_commands, RMUX_EXTENSION_COMMANDS,
};
use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    NewPaneInputV1Request, PaneTarget, Request, ResizePaneAxesRequest, SourceFileInputV1Request,
};

const INTERNAL_COMMAND_NAMES: [&str; 9] = [
    "new-pane-input-v1",
    "source-file-input-v1",
    "SourceFileInputV1",
    "source-file-input-data-v1",
    "SourceFileInputDataV1",
    "source-file-relay-v1",
    "SourceFileRelayV1",
    "resize-pane-axes",
    "ResizePaneAxes",
];

pub(super) fn expected_routing_names(variants: &BTreeSet<String>) -> BTreeSet<String> {
    // ResizePaneAxes shares ResizePane's stable routing name. The enum/handler
    // comparisons require both variants independently, without merging.
    variants
        .iter()
        .map(|variant| match variant.as_str() {
            "resize-pane-axes" => "resize-pane".to_owned(),
            _ => variant.clone(),
        })
        .collect()
}

pub(super) fn assert_public_commands() -> Result<(), Box<dyn Error>> {
    let inventory = list_command_names().collect::<BTreeSet<_>>();
    let rendered = render_list_commands(Some("#{command_list_name}"), None)?;
    let listed = rendered.iter().map(String::as_str).collect::<BTreeSet<_>>();

    // Both V2 snapshot routes are documented public commands.
    for command in ["export-window-snapshot-v2", "restore-window-snapshot-v2"] {
        assert!(inventory.contains(command), "missing inventory: {command}");
        // Bare list-commands intentionally hides extensions; public extensions
        // are selected by exact name (command_inventory::render_list_commands).
        assert_eq!(
            render_list_commands(Some("#{command_list_name}"), Some(command))?,
            [command]
        );
    }

    // Wire variants must not advertise distinct public commands.
    for internal in INTERNAL_COMMAND_NAMES {
        assert!(
            !inventory.contains(internal),
            "internal inventory: {internal}"
        );
        assert!(
            !listed.contains(internal),
            "internal list-commands: {internal}"
        );
        assert!(render_list_commands(None, Some(internal)).is_err());
    }
    // NewPaneInputV1 carries argv, parsed with the public extension table by
    // admit_new_pane_input_v1. Its stable wire routing tag is not a CLI verb.
    let input = NewPaneInputV1Request::new(vec!["newp".to_owned()], None, None);
    let parser = CommandParser::new().with_exact_commands(RMUX_EXTENSION_COMMANDS);
    let parsed = parser.parse_arguments_with_assignments(&input.arguments)?;
    let resize = Request::ResizePaneAxes(Box::new(ResizePaneAxesRequest {
        target: PaneTarget::with_window(super::session_name("surface"), 0, 0),
        columns: Some(30),
        rows: Ok(Some(8)),
    }));
    assert_eq!(
        parsed
            .commands()
            .iter()
            .map(|command| command.name())
            .collect::<Vec<_>>(),
        ["new-pane"],
        "internal wire variants must resolve to their public command families"
    );
    assert_eq!(resize.command_name(), "resize-pane");
    // SourceFileInputV1 also carries public argv, but keeps a distinct wire
    // routing name. Neither that tag nor the Rust variant is a CLI command.
    let source = SourceFileInputV1Request::new(
        vec!["source".to_owned(), "input.conf".to_owned()],
        None,
        None,
    );
    let parsed_source = parser.parse_arguments_with_assignments(&source.arguments)?;
    assert_eq!(
        parsed_source
            .commands()
            .iter()
            .map(|command| command.name())
            .collect::<Vec<_>>(),
        ["source-file"]
    );
    assert_eq!(
        Request::SourceFileInputV1(Box::new(source)).command_name(),
        "source-file-input-v1"
    );
    for family in ["new-pane", "source-file", resize.command_name()] {
        assert!(inventory.contains(family));
        assert_eq!(
            render_list_commands(Some("#{command_list_name}"), Some(family))?,
            [family]
        );
    }
    for internal in INTERNAL_COMMAND_NAMES {
        assert!(parser.parse_arguments_with_assignments([internal]).is_err());
    }
    Ok(())
}
