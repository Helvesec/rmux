use std::fs;

use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    BindKeyRequest, KillPaneRequest, PaneKillRequest, PaneTargetRef, Request, Response,
    SourceFileRequest,
};
use tokio::sync::mpsc;

use super::{
    assert_oracle_transition, build_scenario, capture_oracle_removal, relevant_notifications,
    session_name, OracleRemoval, Scenario, TargetClientKind,
};
use crate::pane_io::AttachControl;
use crate::test_shell::ScratchPath;
use crate::DaemonConfig;

const BINDING_PID: u32 = u32::MAX - 83_000;
const DAEMON_TEST_STACK_SIZE: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
enum EntryPath {
    CliRequest,
    Control,
    SourceFile,
    Startup,
    Binding,
    Queue,
    RunShellCommands,
    SdkStableId,
}

struct BindingGuard {
    _events: mpsc::UnboundedReceiver<AttachControl>,
}

fn durable_test_root(label: &str) -> ScratchPath {
    ScratchPath::directory(&format!("kill-selection-{label}"))
}

async fn prepare_binding(scenario: &mut Scenario, target: &str) -> BindingGuard {
    let actor = session_name("kill-selection-binding-actor");
    super::new_session(&scenario.handler, &actor).await;
    let (attach_tx, attach_rx) = mpsc::unbounded_channel();
    scenario
        .handler
        .register_attach(BINDING_PID, actor, attach_tx)
        .await;
    let response = scenario
        .handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "prefix".to_owned(),
            key: "X".to_owned(),
            note: Some("explicit kill selection regression".to_owned()),
            repeat: false,
            command: Some(vec![
                "kill-pane".to_owned(),
                "-t".to_owned(),
                target.to_owned(),
            ]),
        })))
        .await;
    assert!(matches!(response, Response::BindKey(_)), "{response:?}");
    let _ = relevant_notifications(&mut scenario.observer_events);
    BindingGuard { _events: attach_rx }
}

fn assert_destroyed(response: Response, entry_path: EntryPath) {
    let Response::KillPane(response) = response else {
        panic!("{entry_path:?} did not return kill-pane success: {response:?}");
    };
    assert!(response.window_destroyed, "{entry_path:?}: {response:?}");
}

async fn invoke_entry_path(
    scenario: &mut Scenario,
    oracle: &OracleRemoval,
    target: &str,
    entry_path: EntryPath,
) {
    match entry_path {
        EntryPath::CliRequest => {
            assert_destroyed(
                scenario
                    .handler
                    .handle(Request::KillPane(KillPaneRequest {
                        target: oracle.target.clone(),
                        kill_all_except: false,
                    }))
                    .await,
                entry_path,
            );
        }
        EntryPath::Control => {
            let commands = scenario
                .handler
                .parse_control_commands(&format!("kill-pane -t {target}"))
                .await
                .expect("control kill-pane parses");
            let result = scenario
                .handler
                .execute_control_commands_identity(
                    scenario.observer_pid,
                    scenario.observer_control_id,
                    commands,
                )
                .await;
            assert!(result.error.is_none(), "{entry_path:?}: {:?}", result.error);
        }
        EntryPath::SourceFile => {
            let root = durable_test_root("source-file");
            let config = root.join("kill-pane.conf");
            fs::write(&config, format!("kill-pane -t {target}\n"))
                .expect("write source-file fixture");
            let response = scenario
                .handler
                .handle(Request::SourceFile(Box::new(SourceFileRequest {
                    paths: vec![config.to_string_lossy().into_owned()],
                    quiet: false,
                    parse_only: false,
                    verbose: false,
                    expand_paths: false,
                    target: None,
                    caller_cwd: Some(root.to_path_buf()),
                    stdin: None,
                })))
                .await;
            assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
            fs::remove_dir_all(root).expect("remove source-file fixture");
        }
        EntryPath::Startup => {
            let root = durable_test_root("startup");
            let config = root.join("kill-pane.conf");
            fs::write(&config, format!("kill-pane -t {target}\n")).expect("write startup fixture");
            let daemon_config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
                vec![config],
                false,
                Some(root.to_path_buf()),
            );
            scenario
                .handler
                .load_startup_config(daemon_config.config_load().clone())
                .await;
            fs::remove_dir_all(root).expect("remove startup fixture");
        }
        EntryPath::Binding => {
            scenario
                .handler
                .handle_attached_live_input_for_test(BINDING_PID, b"\x02X")
                .await
                .expect("real prefix binding executes");
        }
        EntryPath::Queue => {
            let commands = CommandParser::new()
                .parse(&format!("kill-pane -t {target}"))
                .expect("queued kill-pane parses");
            scenario
                .handler
                .execute_parsed_commands_for_test(scenario.observer_pid, commands)
                .await
                .expect("queued kill-pane executes");
        }
        EntryPath::RunShellCommands => {
            let commands = CommandParser::new()
                .parse(&format!("run-shell -C 'kill-pane -t {target}'"))
                .expect("run-shell -C kill-pane parses");
            scenario
                .handler
                .execute_parsed_commands_for_test(scenario.observer_pid, commands)
                .await
                .expect("run-shell -C kill-pane executes");
        }
        EntryPath::SdkStableId => {
            assert_destroyed(
                scenario
                    .handler
                    .handle(Request::PaneKill(PaneKillRequest {
                        target: PaneTargetRef::by_id(scenario.target.clone(), oracle.pane_id),
                        kill_all_except: false,
                    }))
                    .await,
                entry_path,
            );
        }
    }
}

fn run_on_daemon_test_stack<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let worker = std::thread::Builder::new()
        .name("explicit-kill-entry-path-test".to_owned())
        .stack_size(DAEMON_TEST_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("entry-path test runtime builds");
            runtime.block_on(test());
        })
        .expect("entry-path test worker spawns");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn every_explicit_kill_entry_path_publishes_the_oracle_identity_before_close() {
    run_on_daemon_test_stack(every_explicit_kill_entry_path_body);
}

async fn every_explicit_kill_entry_path_body() {
    // CLI parsing has its own request-mapping tests; CliRequest is the exact
    // request sent by that path. The remaining variants exercise their real
    // server parsers/queues, including a real prefix binding and run-shell -C.
    for (path_index, entry_path) in [
        EntryPath::CliRequest,
        EntryPath::Control,
        EntryPath::SourceFile,
        EntryPath::Startup,
        EntryPath::Binding,
        EntryPath::Queue,
        EntryPath::RunShellCommands,
        EntryPath::SdkStableId,
    ]
    .into_iter()
    .enumerate()
    {
        let mut scenario =
            build_scenario(TargetClientKind::None, 3, "manual", 900 + path_index as u32).await;
        let initial_target = {
            let state = scenario.handler.state.lock().await;
            let session = state
                .sessions
                .session(&scenario.target)
                .expect("entry target session exists");
            let pane = session
                .window()
                .active_pane()
                .expect("entry target pane exists");
            format!(
                "{}:{}.{}",
                scenario.target,
                session.active_window_index(),
                pane.index()
            )
        };
        let _binding_guard = if matches!(entry_path, EntryPath::Binding) {
            Some(prepare_binding(&mut scenario, &initial_target).await)
        } else {
            None
        };
        let oracle = capture_oracle_removal(&scenario.handler, &scenario.target).await;
        invoke_entry_path(&mut scenario, &oracle, &initial_target, entry_path).await;
        assert_oracle_transition(&mut scenario, oracle, &format!("entry-path={entry_path:?}"))
            .await;
    }
}
