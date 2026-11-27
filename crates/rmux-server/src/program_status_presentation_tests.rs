use super::{run_commands, session_name, RequestHandler};
use rmux_proto::{NewSessionRequest, Request, TerminalSize};

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 8 };

async fn session() -> RequestHandler {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("work"),
            detached: true,
            size: Some(SIZE),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&response);
    handler
}

#[tokio::test]
async fn program_status_json_indices_target_the_reported_panes() {
    let handler = session().await;
    run_commands(&handler, "set -gw pane-base-index 1").await;
    run_commands(&handler, "split-window -d -t work:0").await;
    run_commands(&handler, "new-window -d -t work:4").await;
    run_commands(&handler, "set -w -t work:4 pane-base-index 7").await;
    let json = run_commands(
        &handler,
        "list-sessions -F '#{session_program_status_json}'",
    )
    .await;
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let panes = json["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 3);
    for pane in panes {
        let target = format!("work:{}.{}", pane["window_index"], pane["pane_index"]);
        let id = run_commands(
            &handler,
            &format!("display-message -p -t {target} '#{{pane_id}}'"),
        )
        .await;
        assert_eq!(id.trim_end(), pane["pane_id"], "{target}");
    }
}

async fn status_screen(handler: &RequestHandler) -> rmux_core::Screen {
    let state = handler.state.lock().await;
    let frame = crate::renderer::render_with_attached_count_prompt_and_pane_title(
        state.sessions.session(&session_name("work")).unwrap(),
        &state.options,
        0,
        crate::renderer::StatusRenderContext {
            state: Some(&state),
            ..Default::default()
        },
    );
    let mut screen = rmux_core::Screen::new(SIZE, 0);
    rmux_core::input::InputParser::new().parse(&frame, &mut screen);
    screen
}

async fn status_row(handler: &RequestHandler) -> String {
    let screen = status_screen(handler).await;
    let mut line = String::new();
    assert!(screen.visit_visible_line_cells(7, 80, |cell| {
        if !cell.is_padding() {
            line.push_str(cell.text());
        }
    }));
    line.trim_end().to_owned()
}

#[tokio::test]
async fn program_status_modifiers_preserve_literal_text_and_suffix_style() {
    let handler = session().await;
    // Both fields report "#[fg=red]ALERT".
    report(
        &handler,
        b"\x1b]7501;state=done:title=I1tmZz1yZWRdQUxFUlQ=:msg=I1tmZz1yZWRdQUxFUlQ=\x07",
    )
    .await;
    run_commands(&handler, "set -g status-style fg=white,bg=black").await;
    let mut failures = Vec::new();
    for field in ["pane_program_status_title", "pane_program_status_message"] {
        for body in [
            field.to_owned(),
            format!("#{{{field}}}"),
            format!("#{{?pane_active,#{{{field}}},wrong}}"),
        ] {
            for (modifier, expected) in [
                ("", "#[fg=red]ALERT"),
                ("p20:", "#[fg=red]ALERT      "),
                ("p-20:", "      #[fg=red]ALERT"),
                ("s/ALERT/TEST/:", "#[fg=red]TEST"),
                ("s/foo/bar/:", "#[fg=red]ALERT"),
                ("b:", "#[fg=red]ALERT"),
                ("=30:", "#[fg=red]ALERT"),
                ("=5:", "#[fg="),
                ("=30;p20:", "#[fg=red]ALERT      "),
            ] {
                let template = format!(
                    "#{{{modifier}{body}}}|#{{session_name}}:#{{window_index}}.#{{pane_index}}"
                );
                run_commands(&handler, &format!("set -g status-format[0] '{template}'")).await;
                let screen = status_screen(&handler).await;
                let mut text = String::new();
                let mut colours = Vec::new();
                assert!(screen.visit_visible_line_cells(7, 80, |cell| {
                    if !cell.is_padding() {
                        text.push_str(cell.text());
                        colours.push(cell.fg());
                    }
                }));
                if text.trim_end() != format!("{expected}|work:0.0")
                    || colours.iter().any(|&fg| fg != 7)
                {
                    failures.push(format!("{template}: {text:?}, fg={colours:?}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Protection must not bypass basename's normal lookup transformation.
    report(&handler, b"\x1b]7501;state=done:title=L3RtcC8jW2ZnPXJlZF1BTEVSVA==:msg=L3RtcC8jW2ZnPXJlZF1BTEVSVA==\x07").await;
    for field in ["pane_program_status_title", "pane_program_status_message"] {
        run_commands(
            &handler,
            &format!("set -g status-format[0] '#{{b:{field}}}|'"),
        )
        .await;
        assert_eq!(status_row(&handler).await, "#[fg=red]ALERT|");
    }
}

#[tokio::test]
async fn program_status_substitution_matches_literal_hashes_and_preserves_captures() {
    let handler = session().await;
    run_commands(&handler, "set -g status-style fg=white,bg=black").await;
    let mut failures = Vec::new();
    for (wire, duplicated, replaced) in [
        (
            b"\x1b]7501;state=done:title=Zm9vI2Jhcg==:msg=Zm9vI2Jhcg==\x07".as_slice(),
            "foo##bar",
            "fooXbar",
        ),
        (
            b"\x1b]7501;state=done:title=I1tmZz1yZWRdRkFLRQ==:msg=I1tmZz1yZWRdRkFLRQ==\x07"
                .as_slice(),
            "##[fg=red]FAKE",
            "X[fg=red]FAKE",
        ),
    ] {
        report(&handler, wire).await;
        for field in ["pane_program_status_title", "pane_program_status_message"] {
            for body in [field.to_owned(), format!("#{{{field}}}")] {
                for (modifier, expected) in [
                    ("s/##/X/", replaced),
                    (r"s/(##)/\1\1/", duplicated),
                    (r"s/(##)/\1/;s/##/X/", replaced),
                ] {
                    let template = format!("#{{{modifier}:{body}}}|#{{session_name}}:#{{window_index}}.#{{pane_index}}");
                    run_commands(&handler, &format!("set -g status-format[0] '{template}'")).await;
                    let screen = status_screen(&handler).await;
                    let mut text = String::new();
                    let mut colours = Vec::new();
                    assert!(screen.visit_visible_line_cells(7, 80, |cell| {
                        if !cell.is_padding() {
                            text.push_str(cell.text());
                            colours.push(cell.fg());
                        }
                    }));
                    if text.trim_end() != format!("{expected}|work:0.0")
                        || colours.iter().any(|&fg| fg != 7)
                    {
                        failures.push(format!("{template}: {text:?}, fg={colours:?}"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

async fn report(handler: &RequestHandler, wire: &[u8]) {
    let work = session_name("work");
    let mut state = handler.state.lock().await;
    let pane = state
        .sessions
        .session(&work)
        .unwrap()
        .active_pane()
        .unwrap()
        .id();
    state
        .append_bytes_to_runtime_pane_transcript(&work, pane, wire)
        .unwrap();
}

#[tokio::test]
async fn program_status_text_is_literal_in_the_status_bar() {
    let handler = session().await;
    report(
        &handler,
        b"\x1b]7501;state=idle:title=I1tkZWZhdWx0XSBJZGxl:msg=IyNbYm9sZF0=\x07",
    )
    .await;
    let fields = "#{pane_program_status_title}|#{pane_program_status_message}";
    let expected = "#[default] Idle|##[bold]";
    let raw = run_commands(&handler, &format!("display-message -p '{fields}'")).await;
    assert_eq!(raw.trim_end(), expected);
    run_commands(&handler, "set -g status-left-length 30").await;
    run_commands(&handler, "set -g status-right ''").await;
    run_commands(
        &handler,
        "set -g status-left '#{pane_program_status_title}|'",
    )
    .await;
    run_commands(
        &handler,
        "set -gw window-status-current-format '#{pane_program_status_message}'",
    )
    .await;
    assert_eq!(status_row(&handler).await, expected);
    // A restored global default expands the component options through T and W.
    run_commands(&handler, "set -g status-format[0] unused").await;
    run_commands(&handler, "set -gu status-format").await;
    assert_eq!(status_row(&handler).await, expected);
    for prefix in ["#[bold]", "#[ignore]"] {
        run_commands(
            &handler,
            &format!("set -g status-format[0] '{prefix}{fields}'"),
        )
        .await;
        assert_eq!(status_row(&handler).await, expected, "{prefix}");
    }
    run_commands(&handler, "set -g @reported '#[default] Idle'").await;
    run_commands(&handler, "set -g status-format[0] '#{?#{==:#{pane_program_status_title},#{@reported}},#{pane_program_status_message},wrong}'").await;
    assert_eq!(status_row(&handler).await, "##[bold]");
    run_commands(
        &handler,
        "set -g status-format[0] '#{q/e:pane_program_status_title}'",
    )
    .await;
    assert_eq!(status_row(&handler).await, "#[default] Idle");
}

#[tokio::test]
async fn program_status_fallback_keeps_configured_style() {
    let handler = session().await;
    for field in ["pane_program_status_title", "pane_program_status_message"] {
        run_commands(
            &handler,
            &format!("set-environment -g {field} '#[bold]ALERT'"),
        )
        .await;
    }
    run_commands(&handler, "set -g status-left-length 5").await;
    run_commands(&handler, "set -g status-right ''").await;
    run_commands(&handler, "set -gw window-status-current-format '|'").await;
    for field in ["pane_program_status_title", "pane_program_status_message"] {
        run_commands(&handler, &format!("set -g status-left '#{{{field}}}'")).await;
        assert_eq!(status_row(&handler).await, "ALERT|");
    }
    // The same bytes have different origins when only the title was reported.
    report(&handler, b"\x1b]7501;state=idle:title=I1tib2xkXUFMRVJU\x07").await;
    run_commands(&handler, "set -g status-format[0] '#{=5:pane_program_status_title}|#{=5:pane_program_status_message}'").await;
    assert_eq!(status_row(&handler).await, "#[bol|ALERT");
}

#[tokio::test]
async fn program_status_width_counts_literal_cells() {
    let handler = session().await;
    // Title is "#[default] Idle", message is "表#[2".
    report(
        &handler,
        b"\x1b]7501;state=idle:title=I1tkZWZhdWx0XSBJZGxl:msg=6KGoI1sy\x07",
    )
    .await;
    for (field, width, expected) in [
        ("title", "40", "#[default] Idle"),
        ("title", "5", "#[def"),
        ("title", "-8", "lt] Idle"),
        ("message", "40", "表#[2"),
        ("message", "3", "表#"),
        ("message", "-3", "#[2"),
        ("message", "/3/#[bold]~/", "表#~"),
        ("message", "/-3/~/", "~#[2"),
    ] {
        let key = format!("pane_program_status_{field}");
        for body in [key.clone(), format!("#{{{key}}}")] {
            let template =
                format!("#{{={width}:{body}}}|#{{session_name}}:#{{window_index}}.#{{pane_index}}");
            run_commands(&handler, &format!("set -g status-format[0] '{template}'")).await;
            assert_eq!(
                status_row(&handler).await,
                format!("{expected}|work:0.0"),
                "{template}"
            );
        }
    }
}

#[tokio::test]
async fn program_status_truncation_marker_stays_literal() {
    let handler = session().await;
    report(
        &handler,
        b"\x1b]7501;state=done:title=QnVpbGQgI1sy:msg=I1tkZWZhdWx0XSBJZGxl\x07",
    )
    .await;
    run_commands(&handler, "set -g @label ABCDEFGHIJ").await;
    for (width, body, expected) in [
        (8, "pane_program_status_message", "#[defaulBuild #[2"),
        (-8, "pane_program_status_message", "Build #[2lt] Idle"),
        (8, "@label", "ABCDEFGHBuild #[2"),
        (40, "pane_program_status_message", "#[default] Idle"),
    ] {
        let format = format!("#{{=/{width}/#{{pane_program_status_title}}/:{body}}}|#{{session_name}}:#{{window_index}}.#{{pane_index}}");
        run_commands(&handler, &format!("set -g status-format[0] '{format}'")).await;
        assert_eq!(
            status_row(&handler).await,
            format!("{expected}|work:0.0"),
            "{format}"
        );
    }
}
