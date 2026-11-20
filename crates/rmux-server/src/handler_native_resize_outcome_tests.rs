//! Opt-in Mac witnesses for the maintained native loop and its real handler callers.
use super::RequestHandler;
use crate::pane_terminals::ResizeCase;
use rmux_proto::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn command(root: &Path, label: &str) -> ProcessCommand {
    ProcessCommand::Argv(vec![
        "/usr/bin/python3".into(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/pane_terminals/native_resize_child.py"
        )
        .into(),
        root.display().to_string(),
        label.into(),
    ])
}

async fn create(h: &RequestHandler, session: &SessionName, root: &Path, label: &str) {
    let response = h
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: Some(root.display().to_string()),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: Some(label.into()),
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: Some(command(root, label)),
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
}

async fn run(case: ResizeCase) {
    let root = PathBuf::from(
        std::env::var("RMUX_RESIZE_EVIDENCE")
            .expect("set a fresh owned RMUX_RESIZE_EVIDENCE directory"),
    )
    .join(format!("{case:?}-{}", std::process::id()));
    assert!(!root.exists(), "each native case needs a fresh directory");
    std::fs::create_dir_all(&root).unwrap();
    let h = RequestHandler::new();
    let endpoint = root.join("owned-unbound.sock");
    h.set_socket_path(&endpoint);
    h.state
        .lock()
        .await
        .options
        .set(
            ScopeSelector::Global,
            OptionName::Status,
            "off".into(),
            SetOptionMode::Replace,
        )
        .unwrap();
    let owner = SessionName::new(format!("resize-{}-a", std::process::id())).unwrap();
    let alias = SessionName::new(format!("resize-{}-b", std::process::id())).unwrap();
    create(&h, &owner, &root, "owner-0").await;
    let response = h
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(owner.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
            command: None,
            process_command: Some(command(&root, "owner-1")),
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
    create(&h, &alias, &root, "unrelated").await;
    let target = WindowTarget::with_window(alias.clone(), 7);
    let response = h
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: target.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    h.wait_for_initial_panes_for_test().await;
    // Wait for each child's ready record before sending any raw-mode token.
    let until = std::time::Instant::now() + Duration::from_secs(4);
    loop {
        let ready = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("child-"))
            .filter(|e| {
                std::fs::read_to_string(e.path())
                    .unwrap_or_default()
                    .contains("\"ready\"")
            })
            .count();
        if ready == 3 {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "children did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (initial, before) = {
        let s = h.state.lock().await;
        (
            s.resize_outcome_samples(&root, "initial"),
            s.resize_outcome_state(),
        )
    };
    let pids = initial
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["pid"].as_u64().unwrap())
        .collect::<Vec<_>>();
    let mut events = h.hook_events.subscribe();
    {
        let mut s = h.state.lock().await;
        s.arm_resize_outcome_test(&root, case);
        if case == ResizeCase::BeforeBatch {
            // This preexisting obligation survives the failed mutation snapshot.
            s.record_applied_window_resize(WindowTarget::with_window(alias.clone(), 0));
        }
    }
    let mut lookup = Value::Null;
    let response = if case == ResizeCase::Lookup {
        lookup = h.state.lock().await.exercise_resize_lookup_test(&owner);
        None
    } else {
        let (width, height) = if case == ResizeCase::SameSize {
            (80, 24)
        } else {
            (90, 28)
        };
        Some(
            h.handle(Request::ResizeWindow(ResizeWindowRequest {
                target: target.clone(),
                width: Some(width),
                height: Some(height),
                adjustment: None,
            }))
            .await,
        )
    };
    let (trace, after, final_children) = {
        let mut s = h.state.lock().await;
        (
            s.finish_resize_outcome_test(),
            s.resize_outcome_state(),
            s.resize_outcome_samples(&root, "final"),
        )
    };
    let mut published = vec![];
    while let Ok(event) = events.try_recv() {
        published.push(format!("{:?}", event.event));
    }
    let receipt = json!({"case":format!("{case:?}"),"owner":owner.as_str(),"alias":alias.as_str(),
        "handler_pid":std::process::id(),"endpoint":endpoint,"endpoint_bound":false,
        "initial":initial,"before":before,"response":format!("{response:?}"),"trace":trace,
        "after":after,"final":final_children,"published":published,"lookup":lookup});
    std::fs::write(
        root.join("case.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    // Teardown precedes terminal assertions, including assertion counterfactuals.
    h.state.lock().await.resize_outcome_quit();
    let until = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let quit = pids.iter().all(|pid| {
            std::fs::read_to_string(root.join(format!("child-{pid}.jsonl")))
                .unwrap_or_default()
                .contains("\"QUIT\"")
        });
        if quit {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "missing cooperative QUIT"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    h.state.lock().await.shutdown_terminals_for_test();
    drop(h);
    let cleanup = cleanup(&pids, &endpoint).await;
    std::fs::write(
        root.join("cleanup.json"),
        serde_json::to_vec_pretty(&cleanup).unwrap(),
    )
    .unwrap();
    assert_eq!(cleanup["alive"], json!([]));
    assert!(!endpoint.exists());
    for (a, b) in initial
        .as_array()
        .unwrap()
        .iter()
        .zip(final_children.as_array().unwrap())
    {
        assert_eq!(a["pid"], b["pid"]);
        assert_eq!(a["pane_id"], b["pane_id"]);
    }
    let expected_sizes = if case == ResizeCase::Residual {
        [[45, 28], [39, 24], [80, 24]]
    } else {
        [[40, 24], [39, 24], [80, 24]]
    };
    for (row, size) in final_children
        .as_array()
        .unwrap()
        .iter()
        .zip(expected_sizes)
    {
        assert_eq!(row["master_size"], json!(size));
    }
    let calls = trace["calls"].as_array().unwrap();
    let expected_calls = match case {
        ResizeCase::Record | ResizeCase::SameSize | ResizeCase::BeforeBatch => 2,
        ResizeCase::Partial | ResizeCase::ScopedPartial => 4,
        ResizeCase::Residual | ResizeCase::ScopedResidual => 3,
        ResizeCase::Lookup => 5,
        // The pane-transfer cases never drive `resize-window`; their witness
        // lives with the FMT1 transfer tests.
        ResizeCase::TransferPartial | ResizeCase::TransferResidual => {
            unreachable!("pane-transfer resize cases are not a resize-window outcome")
        }
    };
    assert_eq!(calls.len(), expected_calls);
    assert!(calls.iter().all(|c| c["owner"] == owner.as_str()));
    if matches!(
        case,
        ResizeCase::BeforeBatch | ResizeCase::Partial | ResizeCase::Residual
    ) {
        assert_eq!(
            before["sessions"], after["sessions"],
            "snapshot fields restored, not native agreement"
        );
    }
    if let Some(response) = response {
        let expected = if case == ResizeCase::SameSize {
            Response::ResizeWindow(ResizeWindowResponse {
                target: target.clone(),
            })
        } else {
            let source = "child process operation failed: injected native resize pre-call failure";
            let original = if case == ResizeCase::BeforeBatch {
                "injected pane terminal resize failure".to_owned()
            } else {
                format!("failed to resize pane terminal for {owner}:7.1: {source}")
            };
            let message = if case == ResizeCase::Residual {
                format!("failed to roll back window runtime for {alias}:7 after server error: {original}: server error: failed to resize pane terminal for {owner}:7.0: {source}")
            } else {
                original
            };
            Response::Error(ErrorResponse {
                error: RmuxError::Server(message),
            })
        };
        assert_eq!(
            bincode::serialize(&response).unwrap(),
            bincode::serialize(&expected).unwrap()
        );
    }
    if matches!(case, ResizeCase::SameSize | ResizeCase::BeforeBatch) {
        assert_eq!(published.len(), 2);
        assert!(published[0].starts_with("WindowLayoutChanged"));
        assert!(published[1].starts_with("WindowResized"));
        let index = if case == ResizeCase::SameSize { 7 } else { 0 };
        assert!(published
            .iter()
            .all(|e| e.contains(&format!("window_index: {index}"))));
    } else {
        assert!(published.is_empty());
    }
    println!("NATIVE_RESIZE_OUTCOME {}", root.display());
}

async fn cleanup(pids: &[u64], endpoint: &Path) -> Value {
    let until = std::time::Instant::now() + Duration::from_secs(3);
    let mut checks = vec![];
    loop {
        let mut alive = vec![];
        for pid in pids {
            let output = std::process::Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "pid=,ppid=,state=,comm="])
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if !stdout.trim().is_empty() {
                alive.push(*pid);
            }
            checks.push(json!({"pid":pid,"status":output.status.code(),"stdout":stdout}));
        }
        if alive.is_empty() || std::time::Instant::now() >= until {
            return json!({"pids":pids,"alive":alive,"checks":checks,"endpoint_exists":endpoint.exists()});
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[ignore = "fresh owned Mac native evidence directory required"]
async fn maintained_same_size() {
    run(ResizeCase::SameSize).await;
}
#[tokio::test]
#[ignore = "fresh owned Mac native evidence directory required"]
async fn maintained_before_batch_compensates() {
    run(ResizeCase::BeforeBatch).await;
}
#[tokio::test]
#[ignore = "fresh owned Mac native evidence directory required"]
async fn maintained_partial_compensates() {
    run(ResizeCase::Partial).await;
}
#[tokio::test]
#[ignore = "fresh owned Mac native evidence directory required"]
async fn maintained_failed_compensation_retains_residual() {
    run(ResizeCase::Residual).await;
}
#[tokio::test]
#[ignore = "fresh owned Mac native evidence directory required"]
async fn maintained_later_lookup_and_session_sibling() {
    run(ResizeCase::Lookup).await;
}
