use super::*;
use rmux_core::events::OutputCursorItem;
use rmux_proto::ProcessCommand;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{sleep, Instant};

const HELPER: &str = "handler::pane_state_tests::windows_eof::windows_eof_child";
const TAIL: &str = "WINDOWS_EOF_UNREAD_TAIL";

// The pane process uses file gates so OFF never needs to be lifted to exit.
// Its own deadline bounds it even if the parent test fails unexpectedly.
#[test]
fn windows_eof_child() {
    let Some(dir) = std::env::var_os("RMUX_WINDOWS_EOF_FIXTURE") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let wait = |name: &str| {
        while !dir.join(name).exists() {
            if std::time::Instant::now() >= deadline {
                std::process::exit(91);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    std::fs::write(dir.join("ready"), std::process::id().to_string()).unwrap();
    wait("park");
    println!("WINDOWS_EOF_PARK");
    std::io::stdout().flush().unwrap();
    wait("tail");
    if dir.join("with-tail").exists() {
        println!("{TAIL}");
        std::io::stdout().flush().unwrap();
    }
    std::fs::write(dir.join("written"), b"flushed").unwrap();
    wait("exit");
    std::process::exit(0);
}

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(tail: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/windows-eof/fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&dir).unwrap();
        if tail {
            std::fs::write(dir.join("with-tail"), b"").unwrap();
        }
        Self(dir.canonicalize().unwrap())
    }

    fn release(&self, name: &str) {
        std::fs::write(self.0.join(name), b"").unwrap();
    }

    async fn wait(&self, name: &str) {
        wait_until(|| self.0.join(name).exists()).await;
        assert!(self.0.join(name).exists(), "fixture did not reach {name}");
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        // Only our own child gates. Preserve the receipt files for diagnosis.
        for name in ["park", "tail", "exit"] {
            let _ = std::fs::write(self.0.join(name), b"");
        }
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !condition() && Instant::now() < deadline {
        sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_off_kept_pane_retires_reader_at_zero_tail_exit() {
    exit_case(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_off_kept_pane_drains_unread_tail_before_retiring() {
    exit_case(true, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn windows_off_normal_exit_drains_unread_tail_before_removal() {
    exit_case(false, true).await;
}

async fn exit_case(remain: bool, tail: bool) {
    let fixture = FixtureDir::new(tail);
    let handler = RequestHandler::new();
    let set = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::SessionGlobal,
            name: "default-shell".to_owned(),
            value: Some(r"C:\Windows\System32\cmd.exe".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(matches!(set, Response::SetOptionByName(_)), "{set:?}");
    let (session, _, _) = create_session_with_pane(&handler, "windows-off-eof").await;
    handler.wait_for_initial_panes_for_test().await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: Some(vec![format!(
                "RMUX_WINDOWS_EOF_FIXTURE={}",
                fixture.0.display()
            )]),
            command: None,
            start_directory: None,
            target_window_index: Some(1),
            insert_at_target: false,
            process_command: Some(ProcessCommand::Argv(vec![
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                "--exact".to_owned(),
                HELPER.to_owned(),
                "--nocapture".to_owned(),
            ])),
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    fixture.wait("ready").await;
    let target = PaneTarget::with_window(session.clone(), 1, 0);
    let set = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: "remain-on-exit".to_owned(),
            value: Some(if remain { "on" } else { "off" }.to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(matches!(set, Response::PaneOptionSet(_)), "{set:?}");
    let (pane_id, sender, presence, pipe) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&session)
            .unwrap()
            .window_at(1)
            .unwrap()
            .pane(0)
            .unwrap()
            .id();
        let sender = state
            .runtime_pane_output_drain_handles(&session, pane_id)
            .1
            .unwrap();
        let presence = state.pane_output_read_demand.acquire_attached_client();
        presence.set_pane_off(pane_id.as_u32(), true);
        let pipe = state.clone_pane_master(&session, 1, 0).unwrap().into_io();
        (pane_id, sender, presence, pipe)
    };
    let generation = sender.current_generation();
    let probe = sender.reader_probe_for_test();
    let mut output = sender.subscribe(); // observation only; adds no read demand
    fixture.release("park");
    wait_until(|| probe.parked() == 1).await;
    assert_eq!(probe.running(), 1, "real native reader started");
    assert_eq!(
        probe.parked(),
        1,
        "real reader reached all-OFF condvar wait"
    );
    assert!(tokio::time::timeout(
        Duration::from_millis(30),
        sender.wait_for_reader_demand(pane_id.as_u32(), Some(generation))
    )
    .await
    .is_err());
    fixture.release("tail");
    fixture.wait("written").await;
    sleep(Duration::from_millis(100)).await;
    let mut bytes = Vec::new();
    while let Some(item) = output.try_recv() {
        if let OutputCursorItem::Event(event) = item {
            bytes.extend_from_slice(event.bytes());
        }
    }
    assert!(
        !String::from_utf8_lossy(&bytes).contains(TAIL),
        "OFF must preserve unread live-process tail"
    );
    assert_eq!(probe.parked(), 1);
    println!("before exit: pid={} pane={} generation={generation} running={} parked={} remain={remain} tail={tail}", std::fs::read_to_string(fixture.0.join("ready")).unwrap(), pane_id.as_u32(), probe.running(), probe.parked());
    fixture.release("exit");
    let deadline = Instant::now() + Duration::from_secs(4);
    let retained;
    loop {
        let state = handler.state.lock().await;
        let is_dead = state.pane_is_dead(&session, pane_id);
        let is_removed = state
            .pane_target_for_runtime_pane(&session, pane_id)
            .is_none();
        if (remain && is_dead) || (!remain && is_removed) {
            retained = if remain {
                let current = state
                    .runtime_pane_output_drain_handles(&session, pane_id)
                    .1
                    .unwrap();
                assert!(
                    Arc::ptr_eq(&probe, &current.reader_probe_for_test()),
                    "same sender retained"
                );
                assert_eq!(current.current_generation(), generation);
                assert_eq!(
                    state.pane_exit_metadata(&session, pane_id).unwrap().status,
                    Some(0)
                );
                true
            } else {
                false
            };
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native exit watcher did not commit pane exit"
        );
        drop(state);
        sleep(Duration::from_millis(5)).await;
    }
    wait_until(|| probe.running() == 0).await;
    let finished = probe.running() == 0;
    let mut eof_count = 0;
    while let Some(item) = output.try_recv() {
        if let OutputCursorItem::Event(event) = item {
            if event.bytes().is_empty() {
                eof_count += 1;
            }
            bytes.extend_from_slice(event.bytes());
        }
    }
    // On failing source, independently establish actual pipe EOF after the
    // watcher returned, without waking or retiring the production reader.
    if !finished && !tail {
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                let mut buffer = [0_u8; 4096];
                let mut count = 0;
                loop {
                    let read = pipe.read(&mut buffer).unwrap();
                    if read == 0 {
                        return count;
                    }
                    count += read;
                    assert!(
                        count < 16_384,
                        "zero-tail fixture unexpectedly produced bulk output"
                    );
                }
            }),
        )
        .await
        .expect("closed native pipe must reach EOF")
        .unwrap();
        println!("independent native pipe read reached EOF; drained_bytes={drained}; production reader still running={} parked={}", probe.running(), probe.parked());
    }
    let retirement = tokio::time::timeout(
        Duration::from_millis(30),
        sender.wait_for_reader_demand(pane_id.as_u32(), Some(generation)),
    )
    .await
    .ok();
    println!("after exit: retained={retained} generation={} running={} parked={} eof_count={eof_count} retirement={retirement:?} tail_seen={}", sender.current_generation(), probe.running(), probe.parked(), String::from_utf8_lossy(&bytes).contains(TAIL));
    // Cleanup happens after the causal observation and before failing asserts.
    let killed = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: session,
            kill_all_except_target: false,
            kill_group: false,
            clear_alerts: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
    wait_until(|| probe.running() == 0).await;
    println!("owned cleanup: reader_threads={}", probe.running());
    assert_eq!(probe.running(), 0, "owned reader cleanup");
    drop(presence);
    assert!(finished, "reader remained parked after native exit with retained pane, sender and generation, demand still OFF");
    assert_eq!(
        retirement,
        Some(false),
        "EOF retires this generation without new demand"
    );
    assert_eq!(
        eof_count, 1,
        "reader publishes EOF exactly once before retirement"
    );
    assert_eq!(
        String::from_utf8_lossy(&bytes).matches(TAIL).count(),
        usize::from(tail),
        "unread tail preserved exactly once"
    );
}
