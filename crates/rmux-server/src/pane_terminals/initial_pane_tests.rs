use super::*;
use rmux_proto::{ProcessCommand, TerminalSize};
use std::path::Path;

fn options(command: &ProcessCommand) -> InitialPaneSpawnOptions<'_> {
    InitialPaneSpawnOptions {
        socket_path: Path::new("direct-unit-endpoint"),
        spawn_environment: None,
        raw_spawn_environment: None,
        environment_overrides: None,
        command: Some(command),
        pane_alert_callback: None,
        pane_exit_callback: None,
    }
}
fn child() -> ProcessCommand {
    #[cfg(unix)]
    {
        ProcessCommand::Argv(vec!["/bin/cat".into()])
    }
    #[cfg(windows)]
    {
        ProcessCommand::Argv(vec![
            r"C:\Windows\System32\cmd.exe".into(),
            "/D".into(),
            "/Q".into(),
        ])
    }
}
fn assert_running(state: &HandlerState, name: &SessionName, size: TerminalSize) {
    let session = state.sessions.session(name).unwrap();
    let pane = session.window().pane(0).unwrap();
    let owner = state.runtime_session_name(name);
    assert!(state.terminals.pane_pid(&owner, pane.id(), 0, 0).unwrap() > 0);
    let master = state
        .terminals
        .clone_pane_master(&owner, pane.id(), 0, 0)
        .unwrap();
    assert_eq!(master.size().unwrap(), size);
    assert!(state.pane_output_generation(&owner, pane.id()) > 0);
    assert_eq!(state.pane_lifecycle(pane.id()).unwrap().dimensions(), size);
    assert_eq!(
        state.transcripts[&owner][&pane.id()].lock().screen().size(),
        size
    );
}
#[tokio::test]
async fn initial_pane_immediate_keeps_child_output_and_native_resize() {
    let mut state = HandlerState::default();
    let name = SessionName::new("trace-immediate").unwrap();
    let size = TerminalSize { cols: 80, rows: 24 };
    state.sessions.create_session(name.clone(), size).unwrap();
    let command = child();
    state
        .insert_initial_session_terminal(&name, options(&command))
        .unwrap();
    state
        .mutate_attached_window_geometry_and_resize_terminal(
            &rmux_proto::WindowTarget::with_window(name.clone(), 0),
            |geometry| {
                geometry.resize(
                    TerminalSize { cols: 90, rows: 26 },
                    TerminalSize { cols: 90, rows: 25 },
                );
                Ok(())
            },
        )
        .unwrap();
    assert_running(&state, &name, TerminalSize { cols: 90, rows: 25 });
    state.shutdown_terminals_for_test();
}
#[cfg(windows)]
#[tokio::test]
async fn initial_pane_deferred_installs_latest_geometry() {
    let mut state = HandlerState::default();
    let name = SessionName::new("trace-deferred").unwrap();
    let size = TerminalSize { cols: 80, rows: 24 };
    state.sessions.create_session(name.clone(), size).unwrap();
    let command = child();
    let job = state
        .prepare_deferred_initial_session_terminal(&name, options(&command))
        .unwrap();
    let terminal = HandlerState::open_deferred_initial_pane_terminal(&job).unwrap();
    // A real logical change between preparation and installation must remain visible.
    // No terminal is installed yet. The normal resize path must preserve the
    // logical change; deferred installation applies its latest native size.
    state
        .mutate_store_and_resize_window_terminal_with_family_if(
            &name,
            0,
            |store| {
                let target = rmux_proto::WindowTarget::with_window(name.clone(), 0);
                let mut switch = store.window_switch_mut(&target)?;
                let size = TerminalSize {
                    cols: 100,
                    rows: 30,
                };
                switch.resize(size, size);
                Ok(())
            },
            |_| true,
        )
        .unwrap();
    assert!(state
        .complete_deferred_initial_pane_spawn(job, terminal)
        .unwrap()
        .is_some());
    assert_running(
        &state,
        &name,
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    );
    let pane_id = state
        .sessions
        .session(&name)
        .unwrap()
        .window()
        .pane(0)
        .unwrap()
        .id();
    let target = rmux_proto::PaneTarget::with_window(name.clone(), 0, 0);
    assert!(
        state.active_pane_is_starting(&name),
        "input drain remains pending"
    );
    assert_eq!(state.pane_native_resize_calls_for_test(&target), 0);
    state.resize_window_terminal_runtime(&name, 0).unwrap();
    assert_eq!(state.pane_native_resize_calls_for_test(&target), 1);
    // A retained input-drain marker cannot excuse a lost installed terminal.
    let terminal = state.terminals.remove_pane(&name, pane_id).unwrap();
    let error = state.resize_window_terminal_runtime(&name, 0).unwrap_err();
    assert_eq!(
        rmux_proto::RmuxError::from(error).to_string(),
        "server error: missing pane terminal for trace-deferred:0.0"
    );
    state
        .terminals
        .insert_pane(name.clone(), pane_id, 0, 0, terminal)
        .unwrap();
    state.shutdown_terminals_for_test();
}

#[cfg(windows)]
#[tokio::test]
async fn initial_pane_deferred_session_resize_requires_pending_identity() {
    let mut state = HandlerState::default();
    let name = SessionName::new("deferred-session-resize").unwrap();
    let size = TerminalSize { cols: 80, rows: 24 };
    state.sessions.create_session(name.clone(), size).unwrap();
    let command = child();
    let job = state
        .prepare_deferred_initial_session_terminal(&name, options(&command))
        .unwrap();
    let target = rmux_proto::WindowTarget::with_window(name.clone(), 0);
    let changed = TerminalSize { cols: 90, rows: 25 };
    state
        .mutate_store_and_resize_window_terminal_with_family_if(
            &name,
            0,
            |store| {
                store.window_switch_mut(&target)?.resize(changed, changed);
                Ok(())
            },
            |_| false,
        )
        .unwrap();
    state.resize_terminals(&name).unwrap();
    assert_eq!(
        state.transcripts[&name][&job.identity.pane_id()]
            .lock()
            .screen()
            .size(),
        changed
    );
    assert_eq!(
        state
            .pane_lifecycle(job.identity.pane_id())
            .unwrap()
            .dimensions(),
        changed
    );
    assert_eq!(
        state.pane_native_resize_calls_for_test(&rmux_proto::PaneTarget::new(name.clone(), 0)),
        0
    );
    state.cancel_starting_pane(&name, job.identity.pane_id());
    assert_eq!(
        state.resize_terminals(&name).unwrap_err().to_string(),
        "server error: missing pane terminal for deferred-session-resize:0.0"
    );
    state.shutdown_terminals_for_test();
}

#[cfg(windows)]
#[tokio::test]
async fn initial_pane_deferred_mixed_selection_keeps_native_neighbor_and_compensation() {
    use crate::session_fixture::SessionSetupExt as _;
    use rmux_proto::{PaneTarget, SplitDirection, SplitWindowTarget, WindowTarget};

    let mut state = HandlerState::default();
    let name = SessionName::new("deferred-mixed").unwrap();
    let alias = SessionName::new("deferred-alias").unwrap();
    let size = TerminalSize { cols: 80, rows: 24 };
    state.sessions.create_session(name.clone(), size).unwrap();
    let command = child();
    let job = state
        .prepare_deferred_initial_session_terminal(&name, options(&command))
        .unwrap();
    let created = state
        .split_window(
            SplitWindowTarget::Pane(PaneTarget::new(name.clone(), 0)),
            SplitDirection::Vertical,
            false,
            Path::new("direct-unit-endpoint"),
            None,
            None,
            SplitPaneProcess::Process(Some(&command)),
            None,
            None,
            None,
            false,
            true,
            Some(rmux_core::FloatingPaneRequest::new(Some(3), Some(4), 30, 8).unwrap()),
            None,
            None,
        )
        .unwrap();
    let live = PaneTarget::with_window(name.clone(), 0, created.pane.pane_index());
    let pending = PaneTarget::new(name.clone(), 0);
    let window_id = state.sessions.session(&name).unwrap().window().id();
    state.sessions.create_session(alias.clone(), size).unwrap();
    state
        .sessions
        .session_setup(&alias)
        .unwrap()
        .link_window_id(7, window_id, false, false)
        .unwrap();
    state.attach_window_link_slot(&name, 0, &alias, 7);
    let target = WindowTarget::with_window(alias.clone(), 7);

    state
        .mutate_window_and_resize_terminal_with_family(&target, |selection| {
            selection.select(1, false)
        })
        .unwrap();
    assert_eq!(state.pane_native_resize_calls_for_test(&pending), 0);
    assert_eq!(state.pane_native_resize_calls_for_test(&live), 1);
    state.resize_terminals(&name).unwrap();
    assert_eq!(state.pane_native_resize_calls_for_test(&live), 2);

    state.fail_next_resize_for_test();
    let error = state
        .mutate_window_and_resize_terminal_with_family(&target, |selection| {
            selection.select(0, false)
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "server error: injected pane terminal resize failure"
    );
    assert_eq!(
        state
            .sessions
            .session(&name)
            .unwrap()
            .window()
            .active_pane_index(),
        1
    );
    assert_eq!(
        state
            .sessions
            .session(&alias)
            .unwrap()
            .window_at(7)
            .unwrap()
            .active_pane_index(),
        1
    );
    assert_eq!(state.pane_native_resize_calls_for_test(&pending), 0);
    assert_eq!(state.pane_native_resize_calls_for_test(&live), 3);
    state.last_pane(target, false, None).unwrap();
    assert_eq!(
        state
            .sessions
            .session(&name)
            .unwrap()
            .window()
            .active_pane_index(),
        0
    );
    assert_eq!(state.pane_native_resize_calls_for_test(&live), 4);

    let live_id = state
        .sessions
        .session(&name)
        .unwrap()
        .window()
        .pane(1)
        .unwrap()
        .id();
    let terminal = state.terminals.remove_pane(&name, live_id).unwrap();
    let error = state.resize_window_terminal_runtime(&alias, 7).unwrap_err();
    assert_eq!(
        rmux_proto::RmuxError::from(error).to_string(),
        "server error: missing pane terminal for deferred-mixed:7.1"
    );
    state
        .terminals
        .insert_pane(name.clone(), live_id, 0, 1, terminal)
        .unwrap();
    assert!(state.starting_panes[&name].contains_key(&job.identity.pane_id()));
    state.shutdown_terminals_for_test();
}

#[cfg(windows)]
#[tokio::test]
async fn initial_pane_failed_resize_restores_geometry_and_compensates_native_pty() {
    let mut state = HandlerState::default();
    let name = SessionName::new("cutoff-rollback").unwrap();
    let original = TerminalSize { cols: 80, rows: 24 };
    state
        .sessions
        .create_session(name.clone(), original)
        .unwrap();
    let command = child();
    state
        .insert_initial_session_terminal(&name, options(&command))
        .unwrap();
    let pane_id = state
        .sessions
        .session(&name)
        .unwrap()
        .window()
        .pane(0)
        .unwrap()
        .id();
    let owner = state.runtime_session_name(&name);
    let child_before = state.terminals.pane_pid(&owner, pane_id, 0, 0).unwrap();
    let generation_before = state.pane_output_generation(&owner, pane_id);
    // Existing cfg(test) failure is before the forward native call. The
    // compensation uses the real ConPTY. This is not a native HRESULT failure.
    state.fail_next_resize_for_test();
    let error = state
        .mutate_attached_window_geometry_and_resize_terminal(
            &rmux_proto::WindowTarget::with_window(name.clone(), 0),
            |geometry| {
                geometry.resize(
                    TerminalSize { cols: 90, rows: 26 },
                    TerminalSize { cols: 90, rows: 25 },
                );
                Ok(())
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected pane terminal resize failure"),
        "{error}"
    );
    assert_eq!(state.window_runtime_resize_count_for_test(), 2);
    assert_running(&state, &name, original);
    assert_eq!(
        state.terminals.pane_pid(&owner, pane_id, 0, 0).unwrap(),
        child_before
    );
    assert_eq!(
        state.pane_output_generation(&owner, pane_id),
        generation_before
    );
    state.shutdown_terminals_for_test();
}

#[cfg(unix)]
#[tokio::test]
async fn pending_output_activation_preserves_content_and_adopts_current_geometry() {
    // Exercise the common activation boundary without a child or output race.
    // The Windows deferred path supplies a geometry newer than preparation.
    for size in [
        TerminalSize { cols: 80, rows: 24 },
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    ] {
        let mut state = HandlerState::default();
        let name = SessionName::new("pending-output-geometry").unwrap();
        let original = TerminalSize { cols: 80, rows: 24 };
        state
            .sessions
            .create_session(name.clone(), original)
            .unwrap();
        let pane_id = state
            .sessions
            .session(&name)
            .unwrap()
            .window()
            .pane(0)
            .unwrap()
            .id();
        let generation = state
            .insert_pane_output_state(
                &name,
                pane_id,
                rmux_core::PaneGeometry::new(0, 0, original.cols, original.rows),
                None,
            )
            .unwrap();
        let transcript = state.transcripts[&name][&pane_id].clone();
        transcript.lock().append_bytes(b"pending marker");
        let output = state.pane_outputs[&name][&pane_id].clone();
        let (before, (), _) = output.capture_with_observer(|| ());
        let output_sequence = transcript.lock().output_sequence();
        let master = rmux_pty::PtyPair::open().unwrap().into_master();
        let activated = state
            .activate_pending_pane_output(
                &name,
                pane_id,
                PaneOutputSpawn {
                    geometry: rmux_core::PaneGeometry::new(0, 0, size.cols, size.rows),
                    initial_title: None,
                    output_reader: master,
                    pane_alert_callback: None,
                    pane_exit_callback: None,
                },
            )
            .unwrap();
        assert_eq!(activated, generation);
        assert_eq!(state.pane_output_generation(&name, pane_id), generation);
        let (after, (), _) = output.capture_with_observer(|| ());
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.next_output_sequence, before.next_output_sequence);
        // Activation already publishes GenerationChanged; resize adds exactly
        // one invalidation only when the supplied geometry actually changes.
        assert_eq!(
            after.invalidation_revision,
            before.invalidation_revision + 1 + u64::from(size != original)
        );
        let guard = transcript.lock();
        assert_eq!(guard.screen().size(), size);
        assert_eq!(guard.output_sequence(), output_sequence);
        assert!(guard
            .capture_main(Default::default(), Default::default())
            .starts_with(b"pending marker"));
        drop(guard);
        state.shutdown_terminals_for_test();
    }
}
