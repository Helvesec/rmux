//! A split that sizes its new pane before the pane it divides (`-b -l`)
//! stays inside that pane's cell, as tmux 3.7b keeps it, so it neither names
//! nor resizes a poisoned pane beside it; a session target and a floating
//! pane name the pane tmux 3.7b would split.

use super::poisoned_transcript_transfer::{layout, outcome, session_split};
use super::*;

/// `p:0.0`, the pane the tests poison, over `p:0.1`; `p:1.0`; `other:0.0`.
async fn one_poisonable_window(output_too: bool, setup: &[&str]) -> RequestHandler {
    let handler = RequestHandler::new();
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultCommand,
                value: crate::test_shell::stdin_discard_command(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    for name in ["p", "other"] {
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name(name),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
    }
    // Reproduce the Windows fixture's window-name prefix on every platform:
    // a bare `p` can name this window before it names session `p`.
    outcome(&handler, &["rename-window", "-t", "other:0", "powershell"])
        .await
        .expect("name the competing window");
    for step in [
        &["split-window", "-d", "-t", "p:0.0"][..],
        &["new-window", "-d", "-t", "p:1"],
        setup,
    ] {
        if !step.is_empty() {
            outcome(&handler, step)
                .await
                .unwrap_or_else(|error| panic!("{step:?}: {error}"));
        }
    }
    let poisoned = PaneTarget::with_window(session_name("p"), 0, 0);
    handler.poison_pane_transcript_for_test(&poisoned).await;
    if output_too {
        handler
            .state
            .lock()
            .await
            .pane_output_for_target(poisoned.session_name(), 0, 0)
            .expect("poisoned output")
            .poison_state_for_test();
    }
    handler
}

/// The panes of `p` by place and size.
async fn sizes(handler: &RequestHandler) -> String {
    let format = "#{session_name}:#{window_index}.#{pane_index}=#{pane_width}x#{pane_height}";
    outcome(handler, &["list-panes", "-s", "-t", "p:", "-F", format])
        .await
        .expect("list-panes -s lists a poisoned pane")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A split of `p:0.1` that sizes its new pane before it (`-b -l`) stays in
/// the cell of the pane it divides, as tmux 3.7b keeps it: served, with the
/// poisoned `p:0.0` at its size and the window tmux 3.7b draws, by place,
/// across a horizontal layout, through `other:9` linked to `p:0`, and as the
/// active pane of a session target (R3497 cases 11, 12, 15 and 16).
#[tokio::test]
async fn poisoned_pane_keeps_its_size_beside_a_split_sized_before_its_neighbour() {
    const SPLIT_BEFORE: &[&str] = &["split-window", "-d", "-b", "-l", "20", "-t"];
    const TMUX: &str = "p:0.0=80x12 p:0.1=80x1 p:0.2=80x9 p:1.0=80x24";
    let cases: [(&[&str], Vec<&str>, &str); 4] = [
        (&[], [SPLIT_BEFORE, &["p:0.1"]].concat(), TMUX),
        (
            &["select-layout", "-t", "p:0", "even-horizontal"],
            vec!["split-window", "-d", "-h", "-b", "-l", "60", "-t", "p:0.1"],
            "p:0.0=40x24 p:0.1=1x24 p:0.2=37x24 p:1.0=80x24",
        ),
        (
            &["link-window", "-d", "-s", "p:0", "-t", "other:9"],
            [SPLIT_BEFORE, &["other:9.1"]].concat(),
            TMUX,
        ),
        (
            &["select-pane", "-t", "p:0.1"],
            [SPLIT_BEFORE, &["p:"]].concat(),
            TMUX,
        ),
    ];
    let mut wrong = Vec::new();
    for output_too in [false, true] {
        for (setup, request, tmux) in &cases {
            let handler = one_poisonable_window(output_too, setup).await;
            let answered = outcome(&handler, request).await;
            let after = sizes(&handler).await;
            if answered != Ok(String::new()) || after != *tmux {
                wrong.push(format!("{setup:?} {request:?}: {answered:?}, {after}"));
            }
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
}

/// A session target splits the active pane of the session's active window:
/// with `p:1` active, the split is served there and the poisoned `p:0.0`
/// keeps its size. A floating pane opened on the poisoned pane names it and
/// is refused before anything changes; one opened on its neighbour is
/// served.
#[tokio::test]
async fn poisoned_pane_is_named_by_its_window_and_by_a_floating_pane() {
    for output_too in [false, true] {
        let handler = one_poisonable_window(output_too, &["select-window", "-t", "p:1"]).await;
        let answered = session_split(&handler, "p").await;
        let after = sizes(&handler).await;
        assert!(answered.is_ok(), "{answered:?}");
        assert_eq!(after, "p:0.0=80x12 p:0.1=80x11 p:1.0=80x12 p:1.1=80x11");

        // Reuse the platform's quiet default command, including on Windows.
        let float = |target: &str| format!("new-pane -d -t {target} -x30 -y8 -X3 -Y4");
        let floating = |target: String| {
            let handler = handler.clone();
            async move {
                handler
                    .execute_parsed_commands_for_test(
                        std::process::id(),
                        rmux_core::command_parser::CommandParser::new()
                            .parse(&target)
                            .expect("new-pane parses"),
                    )
                    .await
                    .map(drop)
                    .map_err(|error| error.to_string())
            }
        };
        let before = layout(&handler).await;
        assert_eq!(
            floating(float("p:0.0")).await,
            Err("pane %0 unavailable".to_owned())
        );
        assert_eq!(layout(&handler).await, before);
        assert_eq!(floating(float("p:0.1")).await, Ok(()));
        assert_ne!(layout(&handler).await, before);
    }
}
