use super::*;
use crate::pane_io::pane_output_channel;
use crate::pane_transcript::PaneTranscript;
use rmux_proto::{SessionName, TerminalSize};
use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

#[tokio::test]
async fn program_status_first_foreground_report_survives_without_negotiation() {
    for padding in [0, PANE_BLOCKING_PARSE_MIN_BYTES] {
        let mut child = ChildCommand::new("/bin/bash")
            .args(["--noprofile", "--norc", "-i"])
            .size(PtyTerminalSize::new(80, 24))
            .spawn()
            .unwrap();
        let transcript = PaneTranscript::shared(10, TerminalSize { cols: 80, rows: 24 });
        let task = spawn_pane_output_reader(
            SessionName::new("first-status").unwrap(),
            PaneId::new(1),
            child.master().try_clone().unwrap(),
            transcript.clone(),
            pane_output_channel(),
            None,
            None,
            None,
            PaneReaderRuntime::current().unwrap(),
        );
        // Read a shell builtin's report before changing its foreground group.
        child
            .master()
            .write_all(b"stty -echo; printf '\\033]7501;state=working:app=shell\\007'\n")
            .unwrap();
        let shell_seen = wait_for_app(&transcript, "shell").await;
        let command = format!(
            "python3 -c 'import os,sys; os.write(1,b\"x\"*{padding}+b\"\\x1b]7501;state=blocked:app=first\\x07\"); sys.stdin.readline()'\n"
        );
        child.master().write_all(command.as_bytes()).unwrap();
        let first_seen = wait_for_app(&transcript, "first").await;
        // Always release/reap our own child before asserting the regression.
        child.master().write_all(b"\nexit\n").unwrap();
        tokio::task::spawn_blocking(move || child.child_mut().wait())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(shell_seen, "shell setup report was not collected");
        assert!(
            first_seen,
            "first foreground report lost, padding={padding}"
        );
    }
}

async fn wait_for_app(transcript: &SharedPaneTranscript, app: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if transcript
            .lock()
            .program_status()
            .summary()
            .is_some_and(|record| record.app.as_deref() == Some(app))
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn program_status_read_attribution_preserves_old_batch_and_fragment_controls() {
    let child = ChildCommand::new("/bin/sh")
        .args(["-c", "read ready"])
        .spawn()
        .unwrap();
    let terminal = child.master().io();
    let group = child.child().pid().as_u32();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while rmux_os::process::unix::foreground_pid(terminal.as_fd()) != Some(group) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no foreground group"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let old_group = group + 1;
    let transcript = PaneTranscript::shared(10, TerminalSize { cols: 80, rows: 24 });
    let output = pane_output_channel();
    let session = SessionName::new("attributed-status").unwrap();
    let context = PanePublishContext {
        session_name: &session,
        pane_id: PaneId::new(1),
        transcript: &transcript,
        pane_output: &output,
        generation: None,
        pane_alert_callback: None,
        status_terminal: Some(terminal),
        emit_no_bell_alert: false,
    };
    transcript.lock().observe_foreground_group(Some(old_group));
    // A complete old-group batch delayed until after the handoff stays stale.
    try_publish_pane_bytes(
        context,
        PaneReadBatch {
            bytes: b"\x1b]7501;state=working:app=stale\x07".to_vec(),
            foreground_group: Some(old_group),
        },
        true,
    )
    .unwrap();
    assert!(transcript.lock().program_status().records().is_empty());

    // Even a new-group read cannot complete an old group's fragmented ST.
    {
        let mut t = transcript.lock();
        t.observe_foreground_group(Some(old_group));
        t.append_bytes(b"\x1b]7501;state=working:app=fragment\x1b");
        t.observe_foreground_group(Some(group));
    }
    try_publish_pane_bytes(
        context,
        PaneReadBatch {
            bytes: b"\\".to_vec(),
            foreground_group: Some(group),
        },
        true,
    )
    .unwrap();
    assert!(transcript.lock().program_status().records().is_empty());

    // A format observation can arm the handoff before fresh output arrives.
    {
        let mut t = transcript.lock();
        t.observe_foreground_group(Some(old_group));
        t.observe_foreground_group(Some(group));
        assert!(t.program_status_transition_pending());
    }
    try_publish_pane_bytes(
        context,
        PaneReadBatch {
            bytes: b"\x1b]7501;state=idle:app=fresh\x07".to_vec(),
            foreground_group: Some(group),
        },
        true,
    )
    .unwrap();
    let fresh = transcript.lock().program_status().summary();
    // macOS may wait for terminal output to drain while the child exits.
    let reader = open_pane_writer(child.master().try_clone().unwrap()).unwrap();
    let mut readiness = PaneReadinessState::default();
    let mut buffer = [0; 128];
    child.master().write_all(b"ready\n").unwrap();
    // The original master owns a startup slave guard. Close it so the reader
    // clone can observe EOF on Linux once the child exits.
    let (master, mut child) = child.into_parts();
    drop(master);
    while read_from_pane(&reader, &mut readiness, &mut buffer)
        .await
        .unwrap()
        != 0
    {}
    tokio::task::spawn_blocking(move || child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fresh.unwrap().app.as_deref(), Some("fresh"));
}
