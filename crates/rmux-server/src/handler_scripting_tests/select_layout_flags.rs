use super::*;

use rmux_core::PaneGeometry;
use rmux_proto::{LayoutName, SelectLayoutRequest, SelectLayoutTarget};

const DAEMON_TEST_STACK_SIZE: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
enum OracleApplication {
    First,
    Repeated,
}

async fn select_layout_fixture(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    create_background_identity_session(&handler, session.clone()).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");

    let selected = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::with_window(session.clone(), 0)),
            layout: LayoutName::EvenHorizontal,
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectLayout(_)),
        "{selected:?}"
    );

    (handler, session)
}

async fn assert_oracle_layout(
    handler: &RequestHandler,
    session: &SessionName,
    application: OracleApplication,
) {
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(session)
        .expect("select-layout session exists")
        .window_at(0)
        .expect("select-layout window exists");
    let pane_zero = window.pane(0).expect("pane 0 exists");
    let pane_one = window.pane(1).expect("pane 1 exists");

    match application {
        OracleApplication::First => {
            assert_eq!(window.layout(), LayoutName::EvenVertical);
            assert_eq!(pane_zero.geometry().x(), 0);
            assert_eq!(pane_one.geometry().x(), 0);
            assert_eq!(pane_zero.geometry().cols(), 80);
            assert_eq!(pane_one.geometry().cols(), 80);
            assert_eq!(pane_zero.geometry().y(), 0);
            assert_eq!(
                pane_one.geometry().y(),
                pane_zero.geometry().rows() + 1,
                "the first oracle application must select a top-to-bottom layout"
            );
        }
        OracleApplication::Repeated => {
            assert_eq!(window.layout(), LayoutName::MainHorizontal);
            assert_eq!(pane_zero.geometry(), PaneGeometry::new(0, 0, 80, 22));
            assert_eq!(pane_one.geometry(), PaneGeometry::new(0, 23, 80, 1));
            let dump = window.layout_dump();
            let (_, actual_body) = dump
                .split_once(',')
                .unwrap_or_else(|| panic!("layout dump lacks checksum separator: {dump}"));
            assert_eq!(
                actual_body,
                format!(
                    "80x24,0,0[80x22,0,0,{},80x1,0,23,{}]",
                    pane_zero.id().as_u32(),
                    pane_one.id().as_u32()
                ),
                "the repeated layout body must match tmux 3.7b oracle cell d89e"
            );
        }
    }
}

async fn assert_buffer(handler: &RequestHandler, name: &str) {
    let state = handler.state.lock().await;
    assert_eq!(
        state.buffers.get(name),
        Some(b"ok".as_slice()),
        "the command following successful select-layout must execute"
    );
}

async fn assert_buffer_absent(handler: &RequestHandler, name: &str) {
    let state = handler.state.lock().await;
    assert!(
        state.buffers.get(name).is_none(),
        "the command following a rejected select-layout must not execute"
    );
}

async fn execute_group(handler: &RequestHandler, requester_pid: u32, command: &str) {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("{command} should parse lexically: {error}"));
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .unwrap_or_else(|error| panic!("{command} should execute: {error}"));
}

fn first_command(session: &str, buffer: &str) -> String {
    format!("select-layout -En -t {session}:0 ; set-buffer -b {buffer} ok")
}

fn repeated_command(session: &str, buffer: &str) -> String {
    format!("selectl -nE -t {session}:0 ; set-buffer -b {buffer} ok")
}

fn run_on_daemon_test_stack<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let worker = std::thread::Builder::new()
        .name("select-layout-flags-test".to_owned())
        .stack_size(DAEMON_TEST_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("select-layout flags test runtime should build");
            runtime.block_on(test());
        })
        .expect("select-layout flags test worker should spawn");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn control_select_layout_cluster_advances_and_repeats_without_losing_follow_on() {
    run_on_daemon_test_stack(control_select_layout_cluster_body);
}

async fn control_select_layout_cluster_body() {
    let name = "select-layout-control";
    let (handler, session) = select_layout_fixture(name).await;
    let requester_pid = 73_101;
    let (_control_id, _events) =
        register_control_for_session(&handler, requester_pid, session.clone()).await;

    for (command, buffer, application) in [
        (
            first_command(name, "select-layout-control-first"),
            "select-layout-control-first",
            OracleApplication::First,
        ),
        (
            repeated_command(name, "select-layout-control-repeat"),
            "select-layout-control-repeat",
            OracleApplication::Repeated,
        ),
    ] {
        let parsed = handler
            .parse_control_commands(&command)
            .await
            .unwrap_or_else(|error| panic!("{command} should parse in control mode: {error}"));
        let result = handler
            .execute_control_commands(requester_pid, parsed)
            .await;
        assert_eq!(result.error, None, "{command}: {:?}", result.error);
        assert!(
            result.execution_errors.is_empty(),
            "{command}: {:?}",
            result.execution_errors
        );
        assert_oracle_layout(&handler, &session, application).await;
        assert_buffer(&handler, buffer).await;
    }
}

#[test]
fn source_file_select_layout_cluster_advances_and_repeats_without_losing_follow_on() {
    run_on_daemon_test_stack(source_file_select_layout_cluster_body);
}

async fn source_file_select_layout_cluster_body() {
    let name = "select-layout-source";
    let (handler, session) = select_layout_fixture(name).await;
    let root = temp_root("select-layout-flags");

    for (index, command, buffer, application) in [
        (
            1,
            first_command(name, "select-layout-source-first"),
            "select-layout-source-first",
            OracleApplication::First,
        ),
        (
            2,
            repeated_command(name, "select-layout-source-repeat"),
            "select-layout-source-repeat",
            OracleApplication::Repeated,
        ),
    ] {
        let relative = format!("select-layout-{index}.conf");
        write_config(&root.join(&relative), &format!("{command}\n"));
        let response = handler
            .handle(source_file_request(
                vec![relative],
                Some(root.to_path_buf()),
            ))
            .await;
        let Response::SourceFile(response) = response else {
            panic!("{command} should return a source-file response");
        };
        assert!(
            response.exit_status().is_none() || response.exit_status() == Some(0),
            "{command}: {response:?}"
        );
        assert!(response.stderr().is_empty(), "{command}: {response:?}");
        assert_oracle_layout(&handler, &session, application).await;
        assert_buffer(&handler, buffer).await;
    }

    fs::remove_dir_all(root).expect("remove select-layout source root");
}

#[test]
fn startup_select_layout_cluster_advances_and_repeats_without_losing_follow_on() {
    run_on_daemon_test_stack(startup_select_layout_cluster_body);
}

async fn startup_select_layout_cluster_body() {
    let name = "select-layout-startup";
    let (handler, session) = select_layout_fixture(name).await;
    let root = temp_root("select-layout-flags-startup");
    let relative = PathBuf::from("startup.conf");
    let config = crate::DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![relative.clone()],
        false,
        Some(root.to_path_buf()),
    );

    for (command, buffer, application) in [
        (
            first_command(name, "select-layout-startup-first"),
            "select-layout-startup-first",
            OracleApplication::First,
        ),
        (
            repeated_command(name, "select-layout-startup-repeat"),
            "select-layout-startup-repeat",
            OracleApplication::Repeated,
        ),
    ] {
        write_config(&root.join(&relative), &format!("{command}\n"));
        handler
            .load_startup_config(config.config_load().clone())
            .await;
        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "{command} should not record a startup error"
        );
        assert_oracle_layout(&handler, &session, application).await;
        assert_buffer(&handler, buffer).await;
    }

    fs::remove_dir_all(root).expect("remove select-layout startup root");
}

#[test]
fn attached_binding_select_layout_cluster_advances_and_repeats_without_losing_follow_on() {
    run_on_daemon_test_stack(attached_binding_select_layout_cluster_body);
}

async fn attached_binding_select_layout_cluster_body() {
    let name = "select-layout-binding";
    let (handler, session) = select_layout_fixture(name).await;
    let requester_pid = 73_102;
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), attach_tx)
        .await;

    for (key, command, buffer, application) in [
        (
            "X",
            first_command(name, "select-layout-binding-first"),
            "select-layout-binding-first",
            OracleApplication::First,
        ),
        (
            "Y",
            repeated_command(name, "select-layout-binding-repeat"),
            "select-layout-binding-repeat",
            OracleApplication::Repeated,
        ),
    ] {
        execute_group(
            &handler,
            requester_pid,
            &format!("bind-key {key} {{ {command} }}"),
        )
        .await;
        let keys = format!("\x02{key}");
        handler
            .handle_attached_live_input_for_test(requester_pid, keys.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("binding {key} should execute: {error}"));
        assert_oracle_layout(&handler, &session, application).await;
        assert_buffer(&handler, buffer).await;
    }
}

#[test]
fn run_shell_commands_select_layout_cluster_advances_and_repeats_without_losing_follow_on() {
    run_on_daemon_test_stack(run_shell_commands_select_layout_cluster_body);
}

async fn run_shell_commands_select_layout_cluster_body() {
    let name = "select-layout-run-shell";
    let (handler, session) = select_layout_fixture(name).await;

    for (command, buffer, application) in [
        (
            first_command(name, "select-layout-run-shell-first"),
            "select-layout-run-shell-first",
            OracleApplication::First,
        ),
        (
            repeated_command(name, "select-layout-run-shell-repeat"),
            "select-layout-run-shell-repeat",
            OracleApplication::Repeated,
        ),
    ] {
        let response = handler
            .handle(Request::RunShell(Box::new(RunShellRequest {
                command: command.clone(),
                arguments: Vec::new(),
                background: false,
                as_commands: true,
                show_stderr: false,
                delay_seconds: None,
                start_directory: None,
                target: None,
                source_depth: None,
            })))
            .await;
        assert!(
            matches!(response, Response::RunShell(_)),
            "{command}: {response:?}"
        );
        assert_oracle_layout(&handler, &session, application).await;
        assert_buffer(&handler, buffer).await;
    }

    let rejected_buffer = "select-layout-run-shell-rejected";
    let response = handler
        .handle(Request::RunShell(Box::new(RunShellRequest {
            command: format!(
                "select-layout -Enx -t {name}:0 ; \
                 set-buffer -b {rejected_buffer} must-not-run"
            ),
            arguments: Vec::new(),
            background: false,
            as_commands: true,
            show_stderr: false,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })))
        .await;
    let Response::Error(error) = response else {
        panic!("run-shell -C must surface the invalid select-layout cluster: {response:?}");
    };
    assert!(
        error.error.to_string().contains("unknown flag"),
        "unexpected run-shell -C invalid-cluster error: {error:?}"
    );
    assert_oracle_layout(&handler, &session, OracleApplication::Repeated).await;
    assert_buffer_absent(&handler, rejected_buffer).await;
}
