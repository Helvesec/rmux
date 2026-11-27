//! FMT1 at the server surface: `move-pane`/`join-pane` carrying a live floating
//! source into a tiled target through the real request handler and real PTYs.
//!
//! The geometric expectations reuse the acquired tmux 3.7b receipts of
//! `floating-conversion-prep` for the same-window cells and this host's
//! qualified tmux 3.7b for the new cells (cross-window, `-f`, `-l`, marked
//! source, window alias); the measurements are recorded under
//! `fmt1/`.

use super::{QueuedLifecycleEvent, RequestHandler};
use rmux_core::command_parser::CommandParser;
use rmux_core::{FloatingPaneRect, LifecycleEvent, PaneId, Window};
use rmux_proto::{
    JoinPaneRequest, JoinPaneResponse, LinkWindowRequest, MovePaneRequest, MovePaneResponse,
    NewWindowRequest, PaneTarget, Request, Response, RmuxError, SessionName, SplitDirection,
    WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn queue(
    handler: &RequestHandler,
    command: &str,
) -> Result<rmux_proto::CommandOutput, RmuxError> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new().parse(command).unwrap(),
        )
        .await
}

async fn window(handler: &RequestHandler, name: &SessionName, window_index: u32) -> Window {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(name)
        .unwrap()
        .window_at(window_index)
        .unwrap()
        .clone()
}

fn pane_ids(window: &Window) -> Vec<(u32, PaneId)> {
    window
        .panes()
        .iter()
        .map(|pane| (pane.index(), pane.id()))
        .collect()
}

fn stack_ids(window: &Window) -> Vec<PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

fn index_of(window: &Window, pane_id: PaneId) -> u32 {
    window
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .expect("pane survives")
        .index()
}

fn geometry(window: &Window, pane_id: PaneId) -> rmux_core::PaneGeometry {
    window
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .expect("pane survives")
        .geometry()
}

fn drain_layout_targets(
    events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<WindowTarget> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.event {
            LifecycleEvent::WindowLayoutChanged { target } => Some(target),
            _ => None,
        })
        .collect()
}

/// The acquired fixture: one 80x24 tile, a 30x8 floating source at (3,2) and a
/// 12x5 floating witness at (50,4), with the tile selected. Panes run a silent
/// stdin-draining command so full `Window` equality stays stable across a
/// refusal. Returns `(handler, session, source_id, witness_id, tile_id)`.
async fn fixture(label: &str) -> (RequestHandler, SessionName, PaneId, PaneId, PaneId) {
    let handler = RequestHandler::new();
    queue(&handler, "set-option -g status off").await.unwrap();
    let command = crate::test_shell::stdin_discard_command();
    queue(
        &handler,
        &format!(
            "set-option -g default-command {}",
            crate::test_shell::command_quote(&command)
        ),
    )
    .await
    .unwrap();
    let name = session_name(label);
    queue(
        &handler,
        &format!("new-session -d -s {name} -n fmt1 -x80 -y24"),
    )
    .await
    .unwrap();
    queue(
        &handler,
        &format!("new-pane -d -t {name}:0.0 -x30 -y8 -X3 -Y2"),
    )
    .await
    .unwrap();
    queue(
        &handler,
        &format!("new-pane -d -t {name}:0.0 -x12 -y5 -X50 -Y4"),
    )
    .await
    .unwrap();
    let current = window(&handler, &name, 0).await;
    for pane in current.panes() {
        handler
            .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
                name.clone(),
                0,
                pane.index(),
            ))
            .await;
    }
    let tile_id = current.pane(0).unwrap().id();
    let stack = stack_ids(&current);
    assert_eq!(stack.len(), 2, "two floats were created");
    let (source_id, witness_id) = (stack[0], stack[1]);
    assert_eq!(
        current.active_pane_index(),
        0,
        "`new-pane -d` keeps the tile"
    );
    assert_eq!(
        current
            .floating_stack()
            .placement(source_id)
            .unwrap()
            .rect(),
        FloatingPaneRect::new(3, 2, 30, 8).unwrap()
    );
    assert_eq!(
        current
            .floating_stack()
            .placement(witness_id)
            .unwrap()
            .rect(),
        FloatingPaneRect::new(50, 4, 12, 5).unwrap()
    );
    (handler, name, source_id, witness_id, tile_id)
}

async fn cleanup(handler: &RequestHandler) {
    assert!(matches!(
        handler
            .handle(Request::KillServer(rmux_proto::KillServerRequest))
            .await,
        Response::KillServer(_)
    ));
}

/// Identity witness for "the runtime followed the pane": same pane id, same OS
/// process, same runtime generation and no exit. `dimensions` is returned so a
/// caller can also assert the final native size, or its absence of change.
#[derive(Debug, PartialEq, Eq)]
struct RuntimeWitness {
    pid: Option<u32>,
    generation: u64,
    revision: u64,
    dimensions: rmux_proto::TerminalSize,
    exited: bool,
}

async fn runtime_witness(handler: &RequestHandler, pane_id: PaneId) -> RuntimeWitness {
    let state = handler.state.lock().await;
    let lifecycle = state
        .pane_lifecycle(pane_id)
        .expect("the pane owns a runtime lifecycle");
    RuntimeWitness {
        pid: match lifecycle.process {
            crate::pane_terminals::PaneLifecycleProcessState::Running { pid } => pid,
            _ => None,
        },
        generation: lifecycle.generation,
        revision: lifecycle.revision,
        dimensions: lifecycle.dimensions(),
        exited: lifecycle.exit_state.is_some(),
    }
}

/// Polls `capture-pane -p` until `marker` appears, so the assertion measures the
/// transcript and not the PTY drain schedule.
async fn wait_for_transcript(handler: &RequestHandler, target: &str, marker: &str) -> String {
    let mut text = String::new();
    for _ in 0..200 {
        let captured = queue(handler, &format!("capture-pane -p -t {target}"))
            .await
            .expect("capture-pane succeeds");
        text = String::from_utf8_lossy(&captured.stdout).into_owned();
        if text.contains(marker) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    text
}

fn move_request(source: PaneTarget, target: PaneTarget) -> MovePaneRequest {
    MovePaneRequest {
        source,
        target,
        direction: SplitDirection::Horizontal,
        detached: false,
        before: false,
        full_size: false,
        size: None,
    }
}

fn join_request(source: PaneTarget, target: PaneTarget) -> JoinPaneRequest {
    JoinPaneRequest {
        source,
        target,
        direction: SplitDirection::Horizontal,
        detached: false,
        before: false,
        full_size: false,
        size: None,
    }
}

fn pane(name: &SessionName, window_index: u32, pane_index: u32) -> PaneTarget {
    PaneTarget::with_window(name.clone(), window_index, pane_index)
}

#[path = "handler_pane_float_transfer_tests/identity.rs"]
mod identity;
// The per-call resize observation only exists where the outcome seam compiles,
// gated exactly like that seam is at `handler.rs`.
#[cfg(all(test, target_os = "macos"))]
#[path = "handler_pane_float_transfer_tests/native_resize.rs"]
mod native_resize;
#[path = "handler_pane_float_transfer_tests/refusals.rs"]
mod refusals;
#[path = "handler_pane_float_transfer_tests/transfers.rs"]
mod transfers;
