use super::*;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use rmux_proto::{
    OptionScopeSelector, PaneTarget, SessionName, SetOptionMode, TerminalSize, WindowTarget,
};

#[test]
fn preview_pane_flags_use_the_represented_panes_active_last_and_zoom_state() {
    let mut state = HandlerState::default();
    let name = SessionName::new("flags").unwrap();
    state
        .sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let mut session = state.sessions.session_setup(&name).unwrap();
    session.split_active_pane().unwrap();
    session.select_pane(0).unwrap();
    session.select_pane(1).unwrap();
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-format",
        "#{pane_index}:#{pane_flags}",
    );
    let labels = |state: &HandlerState| {
        let session = state.sessions.session(&name).unwrap();
        let window = session.window_at(0).unwrap();
        [
            resolve_label(state, session, 0, window, window.pane(0), false).text,
            resolve_label(state, session, 0, window, window.pane(1), false).text,
            resolve_label(state, session, 0, window, window.active_pane(), true).text,
        ]
    };
    assert_eq!(labels(&state), ["0:-", "1:*", "1:*"]);
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .toggle_zoom_in_window(0, 1)
        .unwrap();
    assert_eq!(labels(&state), ["0:-", "1:*Z", "1:*Z"]);
}

fn put(state: &mut HandlerState, scope: OptionScopeSelector, name: &str, value: &str) {
    state
        .options
        .set_by_name(
            scope,
            name,
            Some(value.into()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .unwrap();
}

#[test]
fn preview_resolves_each_raw_column_in_its_window_or_pane_context() {
    let mut state = HandlerState::default();
    let name = SessionName::new("s").unwrap();
    let size = TerminalSize { cols: 80, rows: 24 };
    state.sessions.create_session(name.clone(), size).unwrap();
    let mut session = state.sessions.session_setup(&name).unwrap();
    session.split_active_pane().unwrap();
    session.create_window(size).unwrap();
    session.select_window(0).unwrap();
    session.select_pane(0).unwrap();
    let format = "#{window_format}#{pane_format}:#{window_index}.#{pane_index}:#{window_active}#{pane_active}";
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-format",
        format,
    );
    for (index, style) in [(0, "fg=red,bg=blue"), (1, "fg=yellow,bg=magenta")] {
        put(
            &mut state,
            OptionScopeSelector::Window(WindowTarget::with_window(name.clone(), index)),
            "tree-mode-preview-style",
            style,
        );
    }
    let pane_scope = OptionScopeSelector::Pane(PaneTarget::with_window(name.clone(), 0, 0));
    put(
        &mut state,
        pane_scope.clone(),
        "tree-mode-preview-format",
        "P0:#{window_format}#{pane_format}:#{pane_active}",
    );
    put(&mut state, pane_scope, "tree-mode-preview-style", "fg=cyan");
    let labels = |state: &HandlerState| {
        let session = state.sessions.session(&name).unwrap();
        let w0 = session.window_at(0).unwrap();
        let w1 = session.window_at(1).unwrap();
        [
            resolve_label(state, session, 0, w0, w0.active_pane(), true),
            resolve_label(state, session, 1, w1, w1.active_pane(), true),
            resolve_label(state, session, 0, w0, w0.pane(0), false),
            resolve_label(state, session, 0, w0, w0.pane(1), false),
        ]
    };
    let found = labels(&state);
    assert_eq!(
        found.each_ref().map(|label| label.text.as_str()),
        ["10:0.0:11", "10:1.0:01", "P0:01:1", "01:0.1:10"]
    );
    assert_eq!(
        found.each_ref().map(|label| label.style.clone()),
        [
            "fg=red,bg=blue",
            "fg=yellow,bg=magenta",
            "fg=cyan",
            "fg=red,bg=blue"
        ]
        .map(|s| Style::parse(s).unwrap())
    );
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .select_pane(1)
        .unwrap();
    let switched = labels(&state);
    assert_eq!(switched[0].text, "10:0.1:11");
    assert_eq!(switched[2].text, "P0:01:0");
    assert_eq!(switched[3].text, "01:0.1:11");
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-style",
        "#{?pane_active,not-a-style,fg=blue}",
    );
    // Remove explicit window and pane values so the dynamic raw value is consumed.
    for scope in [
        OptionScopeSelector::Window(WindowTarget::with_window(name.clone(), 0)),
        OptionScopeSelector::Pane(PaneTarget::with_window(name.clone(), 0, 0)),
    ] {
        state
            .options
            .set_by_name(
                scope,
                "tree-mode-preview-style",
                None,
                SetOptionMode::Replace,
                false,
                true,
                false,
            )
            .unwrap();
    }
    let dynamic = labels(&state);
    assert_eq!(dynamic[2].style, Style::parse("fg=blue").unwrap());
    assert_eq!(dynamic[3].style, Style::default());
}

fn text(line: &str, width: usize) -> String {
    let formatted = format_draw_line(line, &Style::default(), width, &Utf8Config::default());
    let mut frame = Vec::new();
    crate::renderer::render_formatted_line(&mut frame, 0, 0, &formatted);
    let mut screen = rmux_core::Screen::new(
        TerminalSize {
            cols: width as u16,
            rows: 1,
        },
        0,
    );
    rmux_core::input::InputParser::new().parse(&frame, &mut screen);
    let mut text = String::new();
    screen.visit_visible_line_cells(0, width, |cell| {
        if !cell.is_padding() {
            text.push_str(cell.text());
        }
    });
    text
}

#[test]
fn label_geometry_preserves_content_and_bounds_markup_and_unicode() {
    let utf8 = Utf8Config::default();
    let mut column = PreviewColumn {
        label: PreviewLabel {
            text: "#[fg=yellow]界é##[x]".into(),
            style: Style::parse("bg=blue").unwrap(),
        },
        lines: vec!["#[fg=green]abcdefghijklmnopqrstuvwxyzABCDEFGHIJK#[default]".into(); 10],
    };
    let line = render_preview_segment_row(&column, 5, 37, 10, &utf8);
    assert_eq!(text(&line, 37), "abcdefghijklm│ 界é#[x] │yzABCDEFGHIJK");
    assert!(line.contains("fg=green"));
    assert!(line.contains("fg=yellow,bg=blue"));
    column.label.text = "0123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz".into();
    assert_eq!(
        text(&render_preview_segment_row(&column, 5, 37, 10, &utf8), 37),
        "│ 0123456789:ABCDEFGHIJKLMNOPQRSTUV │"
    );
    for label in ["", "#[fg=red]", "\u{301}"] {
        column.label.text = label.into();
        assert_eq!(
            render_preview_segment_row(&column, 5, 37, 10, &utf8),
            column.lines[5]
        );
    }
    column.label.text = "界AB".into();
    for (width, height) in [(4, 10), (37, 2)] {
        assert_eq!(
            render_preview_segment_row(&column, 1, width, height, &utf8),
            column.lines[1]
        );
    }
    column.lines = vec!["12345".into(); 5];
    assert_eq!(render_preview_segment_row(&column, 3, 5, 5, &utf8), "12345");
    column.label.text = "Xlong".into();
    assert_eq!(
        text(&render_preview_segment_row(&column, 3, 5, 5, &utf8), 5),
        "│ X │"
    );
}

#[test]
fn cell_overlay_preserves_wide_boundaries_styles_and_literal_hashes() {
    let utf8 = Utf8Config::default();
    let mut base = format_draw_line("#[fg=green]界ab界##", &Style::default(), 7, &utf8);
    base.overlay(
        1,
        &format_draw_line("#[fg=red]X", &Style::default(), 1, &utf8),
    );
    let projected = base.to_styled_text();
    assert_eq!(text(&projected, 7), " Xab界#");
    assert!(projected.contains("fg=red]X"));
    assert!(projected.contains("fg=green]ab界##"));
    let filled = format_draw_line("#[fill=blue]X", &Style::default(), 3, &utf8).to_styled_text();
    assert_eq!(text(&filled, 3), "X  ");
    assert!(filled.contains("bg=blue"));
}

#[test]
fn selected_pane_preview_consumes_its_label_format_and_style() {
    let mut state = HandlerState::default();
    let name = SessionName::new("selected-preview").unwrap();
    state
        .sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let target = PaneTarget::new(name, 0);
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-format",
        "FIRST",
    );
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-style",
        "fg=red",
    );
    let first = super::super::render_tree_pane_preview(
        &state,
        target.clone(),
        80,
        10,
        &Utf8Config::default(),
    )
    .join("\n");
    assert!(first.contains("FIRST"), "{first}");
    assert!(first.contains("fg=red"), "{first}");
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-format",
        "SECOND",
    );
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "tree-mode-preview-style",
        "fg=blue",
    );
    let second =
        super::super::render_tree_pane_preview(&state, target, 80, 10, &Utf8Config::default())
            .join("\n");
    assert!(second.contains("SECOND"), "{second}");
    assert!(second.contains("fg=blue"), "{second}");
    assert!(!second.contains("FIRST"));
}
