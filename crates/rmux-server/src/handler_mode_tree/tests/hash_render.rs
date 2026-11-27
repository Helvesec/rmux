use super::*;

fn displayed_row(frame: &[u8], size: TerminalSize, row: usize) -> String {
    let mut screen = Screen::new(size, 0);
    InputParser::new().parse(frame, &mut screen);
    screen
        .absolute_line_view(row)
        .expect("displayed row")
        .cells()
        .iter()
        .map(|cell| if cell.is_padding() { "" } else { cell.text() })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn overlay_row(text: &str) -> String {
    let size = TerminalSize { cols: 80, rows: 24 };
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(SessionName::new("test").unwrap(), size)
        .unwrap();
    let mut mode = test_mode(24);
    mode.key_format.clear();
    let build = flat_build(&[text]);
    displayed_row(&render_mode_tree_overlay(&state, &mode, &build), size, 0)
}

#[test]
fn mode_tree_hash_runs_draw_unescaped_cells() {
    for (text, expected) in [
        ("a#b", "a#b"),
        ("a##b", "a#b"),
        ("a###b", "a##b"),
        ("a####b", "a##b"),
        ("a#####b", "a###b"),
        ("a######b", "a###b"),
        ("a##", "a#"),
        ("a###", "a##"),
        ("界##é", "界#é"),
    ] {
        assert_eq!(overlay_row(text), expected, "input {text:?}");
    }
}

#[test]
fn mode_tree_hash_neutral_style_keeps_displayed_cells() {
    for text in ["a##b", "a###b", "a####b", "界##é"] {
        assert_eq!(
            overlay_row(text),
            overlay_row(&format!("#[default]{text}")),
            "a neutral style must not change displayed cells for {text:?}"
        );
    }
}

#[test]
fn mode_tree_hash_style_quoted_session_name_draws_once() {
    let size = TerminalSize { cols: 80, rows: 24 };
    let session = SessionName::new("a#b").unwrap();
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(session.clone(), size)
        .unwrap();
    state.ensure_live_window_link_occurrences();
    let mut mode = test_mode(24);
    mode.session_name = session.clone();
    mode.session_id = state.sessions.session(&session).unwrap().id();
    mode.key_format.clear();
    mode.tree_depth = TreeDepth::Session;
    for template in ["#{q/e/:session_name}", "#[default]#{q/e/:session_name}"] {
        mode.row_format = Some(template.to_owned());
        let build =
            super::super::mode_tree_tree_build::build_tree_items(&mode, &state, &[]).unwrap();
        assert_eq!(build.visible.len(), 1);
        assert!(build.items[&build.visible[0]].line.contains("a##b"));
        let row = displayed_row(&render_mode_tree_overlay(&state, &mode, &build), size, 0);
        let detail = row.strip_prefix("  ").expect("session-tree leaf padding");
        assert_eq!(detail, "a#b: a#b", "template {template:?}");
    }
}
