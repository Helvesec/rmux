use rmux_core::{OptionStore, Pane, Screen};
use rmux_proto::{OptionName, OptionScopeSelector, PaneTarget};
use std::borrow::Cow;

pub(super) fn resolve(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
) -> Vec<(u32, i32)> {
    options
        .resolve_array_entries_for_scope(
            &OptionScopeSelector::Pane(PaneTarget::with_window(
                session.name().clone(),
                session.active_window_index(),
                pane.index(),
            )),
            OptionName::PaneColours,
        )
        .into_iter()
        .filter_map(|(index, value)| {
            let colour = rmux_core::style::parse_colour(&value).ok()?;
            (index < 256 && colour != rmux_core::input::COLOUR_NONE).then_some((index, colour))
        })
        .collect()
}

pub(super) fn apply<'a>(screen: &'a Screen, entries: &[(u32, i32)]) -> Cow<'a, Screen> {
    if entries.is_empty() {
        return Cow::Borrowed(screen);
    }
    let mut rendered = screen.clone();
    rendered.overlay_colour_palette(entries);
    Cow::Owned(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_core::input::InputParser;
    use rmux_proto::TerminalSize;
    #[test]
    fn palette_changes_rendered_ansi_bright_and_indexed_colours_without_capture_mutation() {
        let mut source = Screen::new(TerminalSize { cols: 20, rows: 1 }, 0);
        InputParser::new().parse(
            b"\x1b[30mA\x1b[90mB\x1b[38;5;123mC\x1b[38;2;1;2;3mD",
            &mut source,
        );
        let original = source.capture_transcript(Default::default(), Default::default());
        let rendered = apply(&source, &[(0, 1), (8, 4), (123, 2)]);
        let bytes = rendered
            .render_visible_line_independent(
                0,
                rmux_core::GridRenderOptions {
                    with_sequences: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("[31mA"), "{text:?}");
        assert!(text.contains("[34mB"), "{text:?}");
        assert!(text.contains("[32mC"), "{text:?}");
        assert!(text.contains("38;2;1;2;3"), "{text:?}");
        assert_eq!(
            source.capture_transcript(Default::default(), Default::default()),
            original
        );
    }
}
