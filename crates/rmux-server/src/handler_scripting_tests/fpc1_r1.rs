use super::*;
use std::fs;

const DAEMON_TEST_STACK_SIZE: usize = 8 * 1024 * 1024;

fn run_on_daemon_test_stack<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let worker = std::thread::Builder::new()
        .name("fpc1-r1-parser-test".to_owned())
        .stack_size(DAEMON_TEST_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("FPC1-R1 parser test runtime");
            runtime.block_on(test());
        })
        .expect("FPC1-R1 parser test worker");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

fn pane_lifecycle(
    state: &crate::pane_terminals::HandlerState,
    pane_id: rmux_proto::PaneId,
) -> crate::pane_terminals::PaneLifecycleState {
    state
        .pane_lifecycle(pane_id)
        .expect("source lifecycle exists")
        .clone()
}

fn assert_process_identity_preserved(
    before: &crate::pane_terminals::PaneLifecycleState,
    after: &crate::pane_terminals::PaneLifecycleState,
) {
    assert_eq!(after.pane_id, before.pane_id);
    assert_eq!(after.window_id, before.window_id);
    assert_eq!(after.process, before.process);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.command(), before.command());
    assert!(after.output_sequence >= before.output_sequence);
}

async fn assert_named_buffer(handler: &RequestHandler, name: &str, expected: &[u8]) {
    assert_eq!(handler.state.lock().await.buffers.get(name), Some(expected));
}

#[tokio::test]
async fn fpc1_r1_control_parser_executes_both_directions_and_preserves_exact_surface() {
    let (handler, name) = super::frs1::setup("fpc-r1-control").await;
    let before = super::frs1::window(&handler, &name).await;
    let source_id = before.pane(0).expect("source pane").id();
    let lifecycle_before = pane_lifecycle(&*handler.state.lock().await, source_id);

    let float = handler
        .parse_control_commands(
            "set-pane-placement -t fpc-r1-control:0.0 floating ; \
             set-buffer -b fpc-r1-control-progress float",
        )
        .await;
    if float.is_err() {
        super::frs1::cleanup(&handler).await;
    }
    let float = float.expect("real control parser admits FPC1");
    handler
        .execute_parsed_commands_for_test(std::process::id(), float)
        .await
        .expect("control conversion and follow-on execute");
    assert!(super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(source_id));
    assert_named_buffer(&handler, "fpc-r1-control-progress", b"float").await;

    let tiled = handler
        .parse_control_commands(
            "set-pane-placement -t fpc-r1-control:0.0 tiled ; \
             set-buffer -b fpc-r1-control-progress tiled",
        )
        .await
        .expect("real control parser admits the return conversion");
    handler
        .execute_parsed_commands_for_test(std::process::id(), tiled)
        .await
        .expect("control return and follow-on execute");
    let returned = super::frs1::window(&handler, &name).await;
    assert_eq!(returned.pane(0).expect("returned pane").id(), source_id);
    assert!(!returned.floating_stack().contains(source_id));
    assert_named_buffer(&handler, "fpc-r1-control-progress", b"tiled").await;

    let legacy_prefix = handler
        .parse_control_commands("set-b -b fpc-r1-legacy-prefix ok")
        .await
        .expect("existing unique legacy prefix remains accepted");
    handler
        .execute_parsed_commands_for_test(std::process::id(), legacy_prefix)
        .await
        .expect("legacy prefix executes");
    assert_named_buffer(&handler, "fpc-r1-legacy-prefix", b"ok").await;
    assert!(handler
        .parse_control_commands("set-pane-placemen -t fpc-r1-control:0.0 floating")
        .await
        .is_err());
    assert!(handler
        .parse_control_commands("web-share --config")
        .await
        .is_err());

    let lifecycle_after = pane_lifecycle(&*handler.state.lock().await, source_id);
    assert_process_identity_preserved(&lifecycle_before, &lifecycle_after);
    super::frs1::cleanup(&handler).await;
}

#[test]
fn fpc1_r1_source_binding_executes_both_directions_with_progress() {
    run_on_daemon_test_stack(fpc1_r1_source_binding_body);
}

async fn fpc1_r1_source_binding_body() {
    let (handler, name) = super::frs1::setup("fpc-r1-binding").await;
    let requester_pid = u32::MAX - 81_044;
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, name.clone(), attach_tx)
        .await;
    let before = super::frs1::window(&handler, &name).await;
    let source_id = before.pane(0).expect("source pane").id();
    let lifecycle_before = pane_lifecycle(&*handler.state.lock().await, source_id);
    let root = temp_root("fpc-r1-source-binding");
    fs::create_dir_all(&root).expect("source root");
    fs::write(
        root.join("binding.conf"),
        "bind-key X { set-pane-placement -t fpc-r1-binding:0.0 floating ; \
         set-buffer -b fpc-r1-binding-progress float }\n\
         bind-key Y { set-pane-placement -t fpc-r1-binding:0.0 tiled ; \
         set-buffer -b fpc-r1-binding-progress tiled }\n",
    )
    .expect("binding config");
    let response = handler
        .handle(source_file_request(
            vec!["binding.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    let admitted = matches!(
        &response,
        Response::SourceFile(result)
            if result.exit_status().is_none() || result.exit_status() == Some(0)
    );
    fs::remove_dir_all(&root).expect("remove source root");
    if !admitted {
        super::frs1::cleanup(&handler).await;
    }
    assert!(
        admitted,
        "real nested binding parser rejected FPC1: {response:?}"
    );

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02X")
        .await
        .expect("floating binding executes");
    assert!(super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(source_id));
    assert_named_buffer(&handler, "fpc-r1-binding-progress", b"float").await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02Y")
        .await
        .expect("tiled binding executes");
    let returned = super::frs1::window(&handler, &name).await;
    assert_eq!(returned.pane(0).expect("returned pane").id(), source_id);
    assert!(!returned.floating_stack().contains(source_id));
    assert_named_buffer(&handler, "fpc-r1-binding-progress", b"tiled").await;

    let lifecycle_after = pane_lifecycle(&*handler.state.lock().await, source_id);
    assert_process_identity_preserved(&lifecycle_before, &lifecycle_after);
    super::frs1::cleanup(&handler).await;
}
