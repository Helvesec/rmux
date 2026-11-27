//! Actual child output -> parser replies -> native pane stdin, with acquired
//! event injection at the attached-input boundary (not physical keyboard proof).
use super::*;
use std::io::{Read, Write};
use std::path::PathBuf;

const CHILD: &str = "handler::tests::send_keys_tests::kitty_native::kitty_native_child";
/// Bounds only a lost exchange: the parent waits this long for each marker.
/// A starved host runs both processes at a fraction of a CPU, so neither
/// bound is a latency contract.
const MARKER_WAIT: Duration = Duration::from_secs(60);
/// Ends a child orphaned by a dead parent; far above a starved full exchange.
const CHILD_WATCHDOG: Duration = Duration::from_secs(300);

#[test]
fn kitty_native_child() {
    let Some(dir) = std::env::var_os("RMUX_KITTY_NATIVE_FIXTURE") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let timeout_dir = dir.clone();
    std::thread::spawn(move || {
        std::thread::sleep(CHILD_WATCHDOG);
        let _ = std::fs::write(timeout_dir.join("timeout"), b"child watchdog");
        std::process::exit(91);
    });
    native_raw_input();
    let exchange = |name: &str, request: &[u8], expected: &[u8]| {
        std::io::stdout().write_all(request).unwrap();
        std::io::stdout().flush().unwrap();
        let mut actual = Vec::new();
        let mut chunk = [0; 256];
        while actual.len() < expected.len() {
            let limit = (expected.len() - actual.len()).min(chunk.len());
            let count = std::io::stdin().read(&mut chunk[..limit]).unwrap();
            assert_ne!(count, 0, "{name}: unexpected EOF");
            actual.extend_from_slice(&chunk[..count]);
            std::fs::write(dir.join(name), &actual).unwrap();
        }
        assert_eq!(actual, expected, "{name}");
    };
    exchange("query", b"\x1b[?u", b"\x1b[?0u");
    exchange("set", b"\x1b[=8u\x1b[?u", b"\x1b[?8u");
    exchange("push", b"\x1b[>31u\x1b[?u", b"\x1b[?31u");
    exchange("pop", b"\x1b[<u\x1b[?u", b"\x1b[?8u");
    exchange("events-mode", b"\x1b[=10u\x1b[?u", b"\x1b[?10u");
    std::fs::write(dir.join("events-ready"), b"").unwrap();
    exchange("events", b"", b"\x1b[97u\x1b[97;1:2u\x1b[97;1:3u");
    exchange("legacy-mode", b"\x1b[<99u\x1b[?u", b"\x1b[?0u");
    std::fs::write(dir.join("legacy-ready"), b"").unwrap();
    exchange("legacy", b"", b"x");
    exchange(
        "screens",
        b"\x1b[=8u\x1b[?1049h\x1b[?u\x1b[=31u\x1b[?1049l\x1b[?u",
        b"\x1b[?0u\x1b[?8u",
    );
    for (name, controls, reply, expected) in [
        (
            "review-alt",
            b"\x1b[=4u\x1b[?u".as_slice(),
            b"\x1b[?4u".as_slice(),
            b"\x1b[\x1bA\x1b\x01".as_slice(),
        ),
        (
            "review-alt-events",
            b"\x1b[=6u\x1b[?u",
            b"\x1b[?6u",
            b"\x1ba\x1b[97;3:2u\x1b[97;3:3u",
        ),
        (
            "review-shift-binding",
            b"\x1b[=31u\x1b[?u",
            b"\x1b[?31u",
            b"BOUND\x1b[98:66;2u\x1b[98;2:3u",
        ),
        (
            "review-shift-fallback",
            b"\x1b[<99u\x1b[>4;0m\x1b[?u",
            b"\x1b[?0u",
            "BBÉ\x1bB\n".as_bytes(),
        ),
        (
            "review-mok",
            b"\x1b[>4;2m\x1b[?u",
            b"\x1b[?0u",
            b"\x1b[27;2;13~",
        ),
        ("review-quarantine", b"\x1b[=10u\x1b[?u", b"\x1b[?10u", b"Z"),
        (
            "review-paste",
            b"\x1b[?2004h\x1b[?u",
            b"\x1b[?10u",
            b"\x1b[200~\x1b[97;xu\x1b[201~",
        ),
    ] {
        exchange(&format!("{name}-mode"), controls, reply);
        std::fs::write(dir.join(format!("{name}-ready")), b"").unwrap();
        exchange(name, b"", expected);
    }
    // The parent kills the session as soon as `done` exists, so `done` must
    // appear with its content: publish it by rename, never by create-then-write.
    std::fs::write(dir.join("done.partial"), b"ok").unwrap();
    std::fs::rename(dir.join("done.partial"), dir.join("done")).unwrap();
    std::process::exit(0);
}

#[cfg(unix)]
fn native_raw_input() {
    use rustix::termios::{tcgetattr, tcsetattr, OptionalActions};
    let stdin = std::io::stdin();
    let mut termios = tcgetattr(&stdin).unwrap();
    termios.make_raw();
    tcsetattr(&stdin, OptionalActions::Now, &termios).unwrap();
}

#[cfg(windows)]
fn native_raw_input() {
    rmux_pty::test_support::enable_raw_console_for_test().unwrap();
}

async fn wait_file(dir: &std::path::Path, name: &str) -> bool {
    let deadline = tokio::time::Instant::now() + MARKER_WAIT;
    while tokio::time::Instant::now() < deadline {
        if dir.join(name).exists() {
            return true;
        }
        if dir.join("timeout").exists() {
            return false;
        }
        sleep(Duration::from_millis(10)).await;
    }
    false
}

fn fixture_entries(dir: &std::path::Path) -> Vec<String> {
    let mut names = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kitty_native_application_negotiates_and_receives_acquired_events() {
    // Removed when the test ends, passing or not; a failure reports what the
    // child had written instead of leaving the tree behind.
    let scratch = crate::test_shell::ScratchPath::directory("kitty-native");
    let dir = scratch.canonicalize().unwrap();
    let handler = RequestHandler::new();
    handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::ExtendedKeys,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    let session = session_name("kitty-native");
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: Some(vec![format!("RMUX_KITTY_NATIVE_FIXTURE={}", dir.display())]),
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: Some(rmux_proto::ProcessCommand::Argv(vec![
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                "--exact".into(),
                CHILD.into(),
                "--nocapture".into(),
            ])),
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
    handler.wait_for_initial_panes_for_test().await;
    // The child reads an exact number of bytes from its events file, so a focus
    // edge ahead of the negotiation would truncate the sequence under test.
    handler
        .disable_pane_focus_reports_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    let pid = std::process::id();
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, session.clone(), tx).await;
    spawn_accounted_attach_control_drain(&handler, pid, rx).await;
    for key in ["A", "u"] {
        super::kitty_review::bind(&handler, key).await;
    }
    let ready = wait_file(&dir, "events-ready").await;
    let mut done = false;
    if ready {
        handler
            .handle_attached_live_input_for_test(pid, b"\x1b[97u\x1b[97;1:2u\x1b[97;1:3u")
            .await
            .unwrap();
        if wait_file(&dir, "legacy-ready").await {
            handler
                .handle_attached_live_input_for_test(pid, b"\x1b[120;1;120u")
                .await
                .unwrap();
            let mut pending = Vec::new();
            let mut review_ready = true;
            for (name, chunks) in [
                ("review-alt", vec![b"\x1b[91;3u\x1b[97;4u\x1b[97;7u".as_slice()]),
                // Alt+[ is measured above under flag 4. For the flag 6 event
                // sequence use Alt+a: system ConPTY can lose an incomplete
                // ESC[ when its next CSI spans native reads (audit R1 sink).
                ("review-alt-events", vec![b"\x1b[97;3u\x1b[97;3:2u\x1b[97;3:3u"]),
                ("review-shift-binding", vec![b"\x1b[97:65;2;65u\x1b[97:65;2:2;65u\x1b[97;2:3u\x1b[98:66;2u\x1b[98;2:3u"]),
                ("review-shift-fallback", vec![b"\x1b[98:66;2u\x1b[98:66;2:2u\x1b[98;2:3u\x1b[233:201;2u\x1b[98:66;4u\x1b[13;2u"]),
                ("review-mok", vec![b"\x1b[13;2u"]),
                ("review-quarantine", vec![b"\x1b[97;x", b"uZ"]),
                ("review-paste", vec![b"\x1b[200~\x1b[97;x", b"u\x1b[201~"]),
            ] {
                if !wait_file(&dir, &format!("{name}-ready")).await {
                    review_ready = false;
                    break;
                }
                for chunk in chunks {
                    handler.handle_attached_live_input(pid, &mut pending, chunk).await.unwrap();
                }
                assert!(pending.is_empty(), "{name}");
            }
            done = review_ready && wait_file(&dir, "done").await;
        }
    }
    let _ = handler
        .handle(Request::KillSession(rmux_proto::KillSessionRequest {
            target: session,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(
        ready && done,
        "native fixture incomplete; child wrote {:?} under {}",
        fixture_entries(&dir),
        dir.display()
    );
    assert_eq!(std::fs::read(dir.join("done")).unwrap(), b"ok");
}
