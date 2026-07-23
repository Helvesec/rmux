use super::*;

use crate::handler::prompt_support::PromptInputEvent;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    PaneResizeRequest, ResizePaneAdjustment, ResizePaneRequest, SplitWindowExtRequest,
};

struct LinkedMutationFixture {
    owner: SessionName,
    grouped_peer: SessionName,
    linked_peer: SessionName,
    pane_one_id: rmux_proto::PaneId,
}

impl LinkedMutationFixture {
    fn targets(&self) -> [WindowTarget; 3] {
        [
            WindowTarget::with_window(self.owner.clone(), 0),
            WindowTarget::with_window(self.grouped_peer.clone(), 0),
            WindowTarget::with_window(self.linked_peer.clone(), 1),
        ]
    }
}

async fn linked_mutation_fixture(handler: &RequestHandler, label: &str) -> LinkedMutationFixture {
    let status = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(status, Response::SetOption(_)), "{status:?}");

    let owner = session_name(&format!("{label}-owner"));
    let grouped_peer = session_name(&format!("{label}-grouped"));
    let linked_peer = session_name(&format!("{label}-linked"));
    create_session(handler, owner.as_str()).await;
    create_grouped_session(handler, grouped_peer.as_str(), &owner).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(owner.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    create_session(handler, linked_peer.as_str()).await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(linked_peer.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    handler.wait_for_initial_panes_for_test().await;

    let fixture = {
        let mut state = handler.state.lock().await;
        for target in [
            WindowTarget::with_window(owner.clone(), 0),
            WindowTarget::with_window(grouped_peer.clone(), 0),
            WindowTarget::with_window(linked_peer.clone(), 1),
        ] {
            state
                .sessions
                .session_mut(target.session_name())
                .expect("fixture session exists")
                .select_pane_in_window(target.window_index(), 0)
                .expect("fixture pane zero selection succeeds");
        }
        let window = state
            .sessions
            .session(&owner)
            .and_then(|session| session.window_at(0))
            .expect("fixture owner window exists");
        LinkedMutationFixture {
            owner,
            grouped_peer,
            linked_peer,
            pane_one_id: window.pane(1).expect("pane one exists").id(),
        }
    };
    assert_alias_windows_identical(handler, &fixture).await;
    fixture
}

async fn assert_alias_windows_identical(handler: &RequestHandler, fixture: &LinkedMutationFixture) {
    let state = handler.state.lock().await;
    let expected = state
        .sessions
        .session(&fixture.owner)
        .and_then(|session| session.window_at(0))
        .expect("owner window exists")
        .clone();
    for target in fixture.targets() {
        assert_eq!(
            state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .expect("window alias exists"),
            &expected,
            "window model diverged for {target}"
        );
    }
}

async fn assert_alias_zoom(
    handler: &RequestHandler,
    fixture: &LinkedMutationFixture,
    zoomed: bool,
    active_pane: u32,
) {
    assert_alias_windows_identical(handler, fixture).await;
    let state = handler.state.lock().await;
    for target in fixture.targets() {
        let window = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .expect("window alias exists");
        assert_eq!(window.is_zoomed(), zoomed, "zoom diverged for {target}");
        assert_eq!(
            window.active_pane_index(),
            active_pane,
            "active pane diverged for {target}"
        );
    }
}

async fn pane_terminal_size(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) -> TerminalSize {
    let master = handler
        .state
        .lock()
        .await
        .clone_pane_master_if_alive(session_name, window_index, pane_index)
        .expect("pane terminal remains alive");
    let size = master.size().expect("pane terminal exposes its size");
    TerminalSize {
        cols: size.cols,
        rows: size.rows,
    }
}

async fn assert_active_runtime_and_lifecycle_match(
    handler: &RequestHandler,
    fixture: &LinkedMutationFixture,
) {
    let (active_pane, expected_size, lifecycle_size) = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&fixture.owner)
            .and_then(|session| session.window_at(0))
            .expect("owner window exists");
        let active_pane = window.active_pane_index();
        let pane = window.pane(active_pane).expect("active pane exists");
        (
            active_pane,
            TerminalSize {
                cols: pane.geometry().cols(),
                rows: pane.geometry().rows(),
            },
            state
                .pane_lifecycle(pane.id())
                .expect("active pane lifecycle exists")
                .dimensions(),
        )
    };
    assert_eq!(lifecycle_size, expected_size);
    assert_eq!(
        pane_terminal_size(handler, &fixture.owner, 0, active_pane).await,
        expected_size,
        "shared PTY size must commit with the linked window model"
    );
}

#[tokio::test]
async fn cli_and_sdk_zoom_commit_linked_model_runtime_and_lifecycle_together() {
    let handler = RequestHandler::new();
    let fixture = linked_mutation_fixture(&handler, "linked-zoom").await;
    let resize_count_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();

    let zoomed = handler
        .handle(Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::with_window(fixture.linked_peer.clone(), 1, 1),
            adjustment: ResizePaneAdjustment::Zoom,
        }))
        .await;
    assert!(matches!(zoomed, Response::ResizePane(_)), "{zoomed:?}");
    assert_alias_zoom(&handler, &fixture, true, 1).await;
    assert_active_runtime_and_lifecycle_match(&handler, &fixture).await;

    let unzoomed = handler
        .handle(Request::PaneResize(PaneResizeRequest {
            target: PaneTargetRef::by_id(fixture.owner.clone(), fixture.pane_one_id),
            adjustment: ResizePaneAdjustment::Zoom,
        }))
        .await;
    assert!(matches!(unzoomed, Response::ResizePane(_)), "{unzoomed:?}");
    assert_alias_zoom(&handler, &fixture, false, 1).await;
    assert_active_runtime_and_lifecycle_match(&handler, &fixture).await;

    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_count_before + 2,
        "CLI and stable-id SDK zooms must each resize the shared window runtime once"
    );
}

#[tokio::test]
async fn pane_selection_resize_failure_rolls_back_every_alias_and_the_shared_runtime() {
    let handler = RequestHandler::new();
    let fixture = linked_mutation_fixture(&handler, "linked-select-rollback").await;
    let terminal_size_before = pane_terminal_size(&handler, &fixture.owner, 0, 0).await;
    let resize_count_before = {
        let mut state = handler.state.lock().await;
        let count = state.window_runtime_resize_count_for_test();
        state.fail_next_resize_for_test();
        count
    };

    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::with_window(fixture.owner.clone(), 0, 1),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert_eq!(
        response,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Server(
                "injected pane terminal resize failure".to_owned(),
            ),
        })
    );
    assert_alias_zoom(&handler, &fixture, false, 0).await;
    assert_eq!(
        pane_terminal_size(&handler, &fixture.owner, 0, 0).await,
        terminal_size_before
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_count_before + 2,
        "failed commit and runtime rollback must each issue one bounded resize"
    );
}

#[tokio::test]
async fn split_window_zoom_commits_the_new_zoomed_window_to_every_alias() {
    let handler = RequestHandler::new();
    let fixture = linked_mutation_fixture(&handler, "linked-split-zoom").await;

    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(
                fixture.linked_peer.clone(),
                1,
                1,
            )),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: true,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let Response::SplitWindow(response) = response else {
        panic!("linked split-window -Z failed: {response:?}");
    };
    handler.wait_for_initial_panes_for_test().await;
    assert_alias_windows_identical(&handler, &fixture).await;
    let state = handler.state.lock().await;
    for target in fixture.targets() {
        let window = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .expect("window alias exists");
        assert!(window.is_zoomed(), "split zoom diverged for {target}");
        assert_eq!(window.active_pane_index(), response.pane.pane_index());
    }
    drop(state);
    let master = handler
        .state
        .lock()
        .await
        .clone_pane_master_if_alive(&fixture.owner, 0, response.pane.pane_index())
        .expect("new split pane terminal remains alive");
    let size = master.size().expect("new split pane exposes its size");
    assert_eq!(
        TerminalSize {
            cols: size.cols,
            rows: size.rows
        },
        TerminalSize {
            cols: 120,
            rows: 40
        }
    );
}

#[tokio::test]
async fn mode_tree_zoom_and_dismissal_commit_every_linked_alias() {
    let handler = RequestHandler::new();
    let fixture = linked_mutation_fixture(&handler, "linked-mode-tree-zoom").await;
    let attach_pid = std::process::id().saturating_add(9_141);
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, fixture.owner.clone(), control_tx)
        .await;

    let parsed = CommandParser::new()
        .parse_arguments(["choose-tree", "-Zw"])
        .expect("zoomed choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("zoomed choose-tree command is valid")
        .expect("choose-tree is recognized");
    handler
        .execute_queued_mode_tree(
            attach_pid,
            command,
            &crate::handler::scripting_support::QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("zoomed choose-tree opens");
    assert_alias_zoom(&handler, &fixture, true, 0).await;
    assert_active_runtime_and_lifecycle_match(&handler, &fixture).await;

    assert!(handler
        .handle_mode_tree_key_event(attach_pid, PromptInputEvent::Char('q'))
        .await
        .expect("q dismisses choose-tree"));
    assert_alias_zoom(&handler, &fixture, false, 0).await;
    assert_active_runtime_and_lifecycle_match(&handler, &fixture).await;
}

#[tokio::test]
async fn non_zoom_window_geometry_mutations_remain_transactional_across_aliases() {
    let handler = RequestHandler::new();
    let fixture = linked_mutation_fixture(&handler, "linked-layout").await;

    let resized = handler
        .handle(Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::with_window(fixture.linked_peer.clone(), 1, 0),
            adjustment: ResizePaneAdjustment::AbsoluteHeight { rows: 12 },
        }))
        .await;
    assert!(matches!(resized, Response::ResizePane(_)), "{resized:?}");
    assert_alias_windows_identical(&handler, &fixture).await;

    let layout = handler
        .handle(Request::NextLayout(rmux_proto::NextLayoutRequest {
            target: WindowTarget::with_window(fixture.grouped_peer.clone(), 0),
        }))
        .await;
    assert!(matches!(layout, Response::NextLayout(_)), "{layout:?}");
    assert_alias_windows_identical(&handler, &fixture).await;

    let rotated = handler
        .handle(Request::RotateWindow(rmux_proto::RotateWindowRequest {
            target: WindowTarget::with_window(fixture.owner.clone(), 0),
            direction: RotateWindowDirection::Down,
            restore_zoom: false,
        }))
        .await;
    assert!(matches!(rotated, Response::RotateWindow(_)), "{rotated:?}");
    assert_alias_windows_identical(&handler, &fixture).await;
    assert_active_runtime_and_lifecycle_match(&handler, &fixture).await;
}
