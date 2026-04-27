use super::{format_draw_line, render_formatted_line};
use crate::status_ranges::StatusRangeType;
use rmux_core::{Style, Utf8Config};

fn render(expanded: &str, base: &str, available: usize) -> String {
    let line = format_draw_line(
        expanded,
        &Style::parse(base).expect("base style parses"),
        available,
        &Utf8Config::default(),
    );
    let mut frame = Vec::new();
    render_formatted_line(&mut frame, 0, 0, &line);
    String::from_utf8(frame).expect("frame is utf-8")
}

#[test]
fn inline_style_switches_apply_and_reset() {
    let frame = render("#[fg=red]red#[default]x", "bg=blue", 4);
    assert!(frame.contains("\u{1b}[0;31;44mred"));
    assert!(frame.contains("\u{1b}[0;44mx"));
}

#[test]
fn ignore_treats_following_style_sequences_as_literal_text() {
    let frame = render("#[ignore]#[fg=red]x#[noignore]y", "default", 32);
    assert!(frame.contains("#[fg=red]x#[noignore]y"));
    assert!(!frame.contains("\u{1b}[31m"));
}

#[test]
fn push_and_pop_default_restore_prior_cell_state() {
    let frame = render(
        "#[fg=red]A#[push-default]#[fg=blue]B#[default]C#[pop-default]#[default]D",
        "bg=green",
        4,
    );
    assert!(frame.contains("\u{1b}[0;31;42mA"));
    assert!(frame.contains("\u{1b}[0;34;42mB"));
    assert!(frame.contains("\u{1b}[0;31;42mC"));
    assert!(frame.contains("\u{1b}[0;42mD"));
}

#[test]
fn fill_background_covers_trailing_space() {
    let frame = render("#[fill=blue]X", "default", 3);
    assert!(frame.contains("\u{1b}[44m"));
    assert!(frame.contains("X  "));
}

#[test]
fn range_directives_split_click_targets_inside_one_surface() {
    let line = format_draw_line(
        "#[range=left]a#[range=window|9]b#[norange]c",
        &Style::default(),
        3,
        &Utf8Config::default(),
    );

    assert_eq!(line.ranges.len(), 2);
    assert!(matches!(line.ranges[0].kind, StatusRangeType::Left));
    assert_eq!(line.ranges[0].x, 0..=0);
    assert!(matches!(line.ranges[1].kind, StatusRangeType::Window(9)));
    assert_eq!(line.ranges[1].x, 1..=1);
}
