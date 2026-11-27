use crate::handler::RequestHandler;
use crate::pane_terminals::HandlerState;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use rmux_core::{input::InputParser, Screen};
use rmux_proto::{
    NewSessionRequest, OptionScopeSelector, PaneTarget, Request, Response, SplitWindowRequest,
    SplitWindowTarget,
};
use rmux_proto::{SessionName, SetOptionMode, SplitDirection, TerminalSize, WindowTarget};

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

fn rendered(state: &HandlerState, name: &SessionName) -> Screen {
    let session = state.sessions.session(name).unwrap();
    let frame = crate::renderer::render_with_attached_count_prompt_and_pane_title(
        session,
        &state.options,
        1,
        crate::renderer::StatusRenderContext {
            state: Some(state),
            ..Default::default()
        },
    );
    let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 0);
    InputParser::new().parse(&frame, &mut screen);
    screen
}

fn visible_line_text(screen: &Screen, row: usize, cols: usize) -> String {
    let mut text = String::new();
    assert!(screen.visit_visible_line_cells(row, cols, |cell| text.push_str(cell.text())));
    text
}

#[test]
fn pane_flags_track_active_last_and_zoom_without_changing_selection() {
    use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
    use rmux_core::formats::FormatContext;

    let mut state = HandlerState::default();
    let name = SessionName::new("flags").unwrap();
    state
        .sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let flags = |state: &HandlerState| {
        let session = state.sessions.session(&name).unwrap();
        let window = session.window_at(0).unwrap();
        window
            .panes()
            .iter()
            .map(|pane| {
                let base = FormatContext::new()
                    .with_managed_session(session)
                    .with_window(0, window, true, false)
                    .with_window_pane(window, pane);
                let runtime = RuntimeFormatContext::new(base)
                    .with_state(state)
                    .with_session(session)
                    .with_window(0, window)
                    .with_pane(pane);
                render_runtime_template(
                    "#{pane_active}:#{pane_last}:#{pane_flags}",
                    &runtime,
                    false,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(flags(&state), ["1:0:*"]);
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .split_before(0, 0, SplitDirection::Horizontal, false, true)
        .unwrap();
    assert_eq!(flags(&state), ["1:0:*", "0:0:"]);
    let mut session = state.sessions.session_setup(&name).unwrap();
    session.select_pane(0).unwrap();
    session.select_pane(1).unwrap();
    assert_eq!(flags(&state), ["0:1:-", "1:0:*"]);
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .select_pane(1)
        .unwrap();
    assert_eq!(flags(&state), ["0:1:-", "1:0:*"]);
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .toggle_zoom_in_window(0, 1)
        .unwrap();
    assert_eq!(flags(&state), ["0:1:-", "1:0:*Z"]);
    let mut session = state.sessions.session_setup(&name).unwrap();
    session.toggle_zoom_in_window(0, 1).unwrap();
    session.select_pane(0).unwrap();
    assert_eq!(flags(&state), ["1:0:*", "0:1:-"]);
}

#[tokio::test]
async fn default_second_status_line_consumes_both_named_pane_formats() {
    let handler = RequestHandler::new();
    let name = SessionName::new("opt").unwrap();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(name.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    let mut state = handler.state.lock().await;
    let window = OptionScopeSelector::Window(WindowTarget::with_window(name.clone(), 0));
    put(
        &mut state,
        OptionScopeSelector::SessionGlobal,
        "status",
        "2",
    );
    put(
        &mut state,
        OptionScopeSelector::SessionGlobal,
        "status-left",
        "FIRST",
    );
    put(
        &mut state,
        OptionScopeSelector::SessionGlobal,
        "status-right",
        "",
    );
    put(&mut state, window.clone(), "pane-base-index", "7");
    for (index, title) in [(0, "Alpha"), (1, "Beta")] {
        // Fence shell output before installing fixture titles: the state lock
        // does not prevent the reader from mutating the shared transcript.
        let output = state.pane_output_for_target(&name, 0, index).unwrap();
        output.set_generation(output.current_generation() + 1);
        state
            .set_pane_title(&PaneTarget::with_window(name.clone(), 0, index), title)
            .unwrap();
    }
    let mut session = state.sessions.session_setup(&name).unwrap();
    session.select_pane(0).unwrap();
    session.select_pane(1).unwrap();
    let screen = rendered(&state, &name);
    let line = visible_line_text(&screen, 23, 80);
    assert!(line.contains("7:[Alpha]-"), "{line:?}");
    assert!(line.contains("8:[Beta]*"), "{line:?}");
    assert!(visible_line_text(&screen, 22, 80).starts_with("FIRST"));
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .toggle_zoom_in_window(0, 1)
        .unwrap();
    let line = visible_line_text(&rendered(&state, &name), 23, 80);
    assert!(line.contains("7:[Alpha]-"), "{line:?}");
    assert!(line.contains("8:[Beta]*Z"), "{line:?}");
    let mut session = state.sessions.session_setup(&name).unwrap();
    session.toggle_zoom_in_window(0, 1).unwrap();
    session.select_pane(0).unwrap();
    let line = visible_line_text(&rendered(&state, &name), 23, 80);
    assert!(line.contains("7:[Alpha]*"), "{line:?}");
    assert!(line.contains("8:[Beta]-"), "{line:?}");
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .select_pane(1)
        .unwrap();
    put(
        &mut state,
        OptionScopeSelector::WindowGlobal,
        "window-pane-status-format",
        "N:#{pane_index}:#{pane_title}",
    );
    put(
        &mut state,
        window.clone(),
        "window-pane-current-status-format",
        "#[fg=red]C:#{pane_index}:#{pane_title}",
    );
    let screen = rendered(&state, &name);
    assert_eq!(
        visible_line_text(&screen, 23, 80).trim_end(),
        "   P: N:7:Alpha  C:8:Beta"
    );
    state
        .sessions
        .session_setup(&name)
        .unwrap()
        .select_pane(0)
        .unwrap();
    let screen = rendered(&state, &name);
    assert_eq!(
        visible_line_text(&screen, 23, 80).trim_end(),
        "   P: C:7:Alpha N:8:Beta"
    );
    let mut red = false;
    screen.visit_visible_line_cells(23, 80, |cell| {
        if cell.text() == "C" {
            red = cell.fg() == rmux_core::parse_colour("red").unwrap();
        }
    });
    assert!(red, "current label uses inline style");
    put(
        &mut state,
        OptionScopeSelector::SessionGlobal,
        "status",
        "3",
    );
    let screen = rendered(&state, &name);
    assert_eq!(
        visible_line_text(&screen, 22, 80).trim_end(),
        "   P: C:7:Alpha N:8:Beta"
    );
    put(
        &mut state,
        window.clone(),
        "window-pane-current-status-format",
        "",
    );
    assert_eq!(
        visible_line_text(&rendered(&state, &name), 22, 80).trim_end(),
        "   P:  N:8:Beta"
    );
    state
        .options
        .set_by_name(
            window,
            "window-pane-current-status-format",
            None,
            SetOptionMode::Replace,
            false,
            true,
            false,
        )
        .unwrap();
    let line = visible_line_text(&rendered(&state, &name), 22, 80);
    assert!(line.contains("7:[Alpha]"), "{line:?}");

    // Application OSC titles still replace an explicitly assigned pane title.
    let target = PaneTarget::with_window(name.clone(), 0, 0);
    let transcript = state.transcript_handle(&target).unwrap();
    let output = state.pane_output_for_target(&name, 0, 0).unwrap();
    crate::pane_io::publish_pane_bytes_for_test(
        &transcript,
        &output,
        b"\x1b]2;Application\x07".to_vec(),
    );
    let line = visible_line_text(&rendered(&state, &name), 22, 80);
    assert!(line.contains("7:[Application]"), "{line:?}");
    assert!(line.contains("N:8:Beta"), "{line:?}");
}
