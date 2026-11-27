//! Native witnesses: exercise the reader, including its post-publication waits.
use std::sync::Arc;
use std::time::Duration;

use rmux_core::{PaneId, WindowId};
use rmux_proto::{SessionName, TerminalSize};
use rmux_pty::{ChildCommand, SpawnedPty};

use super::*;
use crate::pane_io::{pane_output_channel, PaneEffectSink, PaneOutputReceiver};
use crate::pane_rename_queue::PaneRenameQueue;
use crate::pane_transcript::PaneTranscript;

struct Reader {
    child: ReaderChild,
    task: tokio::task::JoinHandle<io::Result<()>>,
    output: PaneOutputSender,
    events: PaneOutputReceiver,
}

impl Reader {
    fn start(queue: Arc<PaneRenameQueue>, enabled: bool, script: &str) -> Self {
        let child = ChildCommand::new("/bin/sh")
            .args(["-c", script])
            .spawn()
            .expect("native PTY child");
        let master = child.master().try_clone().unwrap();
        let output = pane_output_channel();
        let events = output.subscribe();
        let transcript = PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 });
        transcript
            .lock()
            .set_rename_policy(enabled, Some(WindowId::new(1)));
        let effects = Arc::new(PaneEffectSink::new(|_| {}, queue));
        let task = tokio::spawn(read_pane_output(
            master,
            SessionName::new("rename-reader").unwrap(),
            PaneId::new(1),
            transcript.clone(),
            output.clone(),
            None,
            Some(effects),
            None,
        ));
        Self {
            child: ReaderChild(Some(child)),
            task,
            output,
            events,
        }
    }

    async fn published(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.events.recv())
            .await
            .expect("reader must publish without another pane's rename capacity");
    }
}

// Reaping must not block the current-thread runtime while its aborted reader
// still owns a PTY descriptor (macOS waits for those descriptors during exit).
struct ReaderChild(Option<SpawnedPty>);
impl std::ops::Deref for ReaderChild {
    type Target = SpawnedPty;
    fn deref(&self) -> &SpawnedPty {
        self.0.as_ref().unwrap()
    }
}
impl Drop for ReaderChild {
    fn drop(&mut self) {
        super::rename_admission_native_tests::reap_fixture(self.0.take().unwrap());
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn rename_admission_is_returned_before_control_wait() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
    let mut first = Reader::start(queue.clone(), true, "read x; printf first; read x");
    let mut control = first.output.subscribe();
    control.protect_control_cursor();
    first.child.master().write_all(b"go\n").unwrap();
    first.published().await;
    // Let the reader enter its real control wait. The protected cursor is
    // deliberately not advanced, so this feed cannot finish its iteration.
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(queue.stats().pending, 0);
    assert_eq!(
        queue.stats().admitted,
        0,
        "a control wait must not own rename admission"
    );
    let mut second = Reader::start(queue.clone(), true, "printf second; read x");
    second.published().await;
    assert!(control.try_recv_admitted(|_| Some(())).is_some());
}

#[tokio::test]
async fn rename_admission_off_pane_bypasses_another_panes_full_fifo() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
    let mut owner = Reader::start(queue.clone(), true, r"printf '\033kA\033\\'; read x");
    owner.published().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        queue.stats().pending,
        1,
        "A's accepted name fills the real FIFO"
    );
    assert_eq!(queue.stats().admitted, 0);
    let mut eligible = Reader::start(queue.clone(), true, "printf eligible; read x");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), eligible.events.recv())
            .await
            .is_err()
    );
    let mut inert = Reader::start(queue.clone(), false, "printf inert; read x");
    inert.published().await;
    assert_eq!(
        queue.stats().pending,
        1,
        "bypass must preserve A's queued name"
    );
    assert!(
        eligible.events.try_recv().is_none(),
        "eligible feed must remain held"
    );
    assert_eq!(
        queue
            .drain()
            .into_iter()
            .map(|r| r.name)
            .collect::<Vec<_>>(),
        ["A"]
    );
    eligible.published().await;
}

#[tokio::test]
async fn rename_admission_worker_holds_capacity_through_effects_then_returns_it() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 });
    transcript
        .lock()
        .set_rename_policy(true, Some(WindowId::new(1)));
    let output = pane_output_channel();
    let main_thread = std::thread::current().id();
    let observed_queue = queue.clone();
    let effects = Arc::new(PaneEffectSink::new(
        move |_| {
            assert_ne!(
                std::thread::current().id(),
                main_thread,
                "current-thread parser uses its worker"
            );
            assert_eq!(observed_queue.stats().admitted, 1);
        },
        queue.clone(),
    ));
    let session = SessionName::new("worker-admission").unwrap();
    let (replies, admission) = publish_pane_bytes_admitted(
        PanePublishContext {
            session_name: &session,
            pane_id: PaneId::new(1),
            transcript: &transcript,
            pane_output: &output,
            generation: None,
            pane_alert_callback: Some(&effects),
            status_terminal: None,
            emit_no_bell_alert: true,
        },
        b"\x1bkworker\x1b\\\x1b[c".to_vec(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(replies, b"\x1b[?1;2c");
    assert_eq!((queue.stats().pending, queue.stats().admitted), (1, 1));
    drop(admission);
    assert_eq!((queue.stats().pending, queue.stats().admitted), (1, 0));
    assert!(
        queue.try_admit().is_none(),
        "accepted names stay charged after returning the feed"
    );
    assert_eq!(queue.drain()[0].name, "worker");
    assert!(queue.try_admit().is_some());
}
