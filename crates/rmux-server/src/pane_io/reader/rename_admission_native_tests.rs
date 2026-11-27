//! Both native readers, including real PTY reply backpressure and policy races.
use std::io::{Read, Write};
use std::sync::{mpsc, Arc};
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use rmux_core::{events::OutputCursorItem, WindowId};
use rmux_proto::TerminalSize;
use rmux_pty::{ChildCommand, SpawnedPty};

use super::*;
use crate::pane_io::{pane_output_channel, PaneEffectSink, PaneOutputReceiver};
use crate::pane_rename_queue::PaneRenameQueue;
use crate::pane_transcript::PaneTranscript;

const FIXTURE: &str = "pane_io::reader::rename_admission_native_tests::reader_fixture";
const MODE: &str = "RMUX_RENAME_READER_FIXTURE_MODE";

#[test]
fn reader_fixture() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    #[cfg(unix)]
    {
        use rustix::termios::{tcgetattr, tcsetattr, OptionalActions};
        let input = std::io::stdin();
        let mut attributes = tcgetattr(&input).unwrap();
        attributes.make_raw();
        tcsetattr(&input, OptionalActions::Now, &attributes).unwrap();
    }
    #[cfg(windows)]
    rmux_pty::test_support::enable_raw_console_for_test().unwrap();
    let mut output = std::io::stdout();
    match mode.as_str() {
        "query" => output
            .write_all(b"\x1b[c".repeat(32_768).as_slice())
            .unwrap(),
        "a" => output.write_all(b"\x1bkA\x1b\\A-DONE").unwrap(),
        "b" => output.write_all(b"\x1bkB\x1b\\B-DONE").unwrap(),
        "split" => {
            output.write_all(b"SPLIT-READY\x1bkB").unwrap();
            output.flush().unwrap();
            std::io::stdin().read_exact(&mut [0]).unwrap();
            output.write_all(b"\x1b\\SPLIT-DONE").unwrap();
        }
        "plain" => output.write_all(b"PLAIN-DONE").unwrap(),
        _ => panic!("unknown fixture mode"),
    }
    output.flush().unwrap();
    // In particular, query mode never drains terminal replies from stdin.
    loop {
        std::thread::park();
    }
}

struct NativeReader {
    child: Option<SpawnedPty>,
    #[cfg(unix)]
    task: Option<tokio::task::JoinHandle<io::Result<()>>>,
    #[cfg(windows)]
    task: Option<std::thread::JoinHandle<io::Result<()>>>,
    output: PaneOutputSender,
    events: PaneOutputReceiver,
    transcript: SharedPaneTranscript,
    queue: Arc<PaneRenameQueue>,
    #[cfg(windows)]
    reply_pause: Option<Arc<super::windows_reply_probe::ReplyPause>>,
}

impl NativeReader {
    fn prepared(queue: Arc<PaneRenameQueue>, enabled: bool) -> Self {
        let output = pane_output_channel();
        let events = output.subscribe();
        let transcript = PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 });
        transcript
            .lock()
            .set_rename_policy(enabled, Some(WindowId::new(1)));
        Self {
            child: None,
            task: None,
            output,
            events,
            transcript,
            queue,
            #[cfg(windows)]
            reply_pause: None,
        }
    }

    fn start(&mut self, mode: &str) {
        let child = ChildCommand::new(std::env::current_exe().unwrap())
            .args(["--exact", FIXTURE, "--nocapture"])
            .env(MODE, mode)
            .spawn()
            .expect("native PTY fixture");
        let master = child.master().try_clone().unwrap();
        let transcript = self.transcript.clone();
        let output = self.output.clone();
        let effects = Some(Arc::new(PaneEffectSink::new(|_| {}, self.queue.clone())));
        let session = rmux_proto::SessionName::new("native-rename-reader").unwrap();
        #[cfg(unix)]
        let task = tokio::spawn(read_pane_output(
            master,
            session,
            PaneId::new(1),
            transcript,
            output,
            None,
            effects,
            None,
        ));
        #[cfg(windows)]
        let pause = self.reply_pause.clone();
        #[cfg(windows)]
        let task = std::thread::spawn(move || {
            let _scope = super::windows_reply_probe::install(pause);
            read_pane_output_blocking(
                master,
                session,
                PaneId::new(1),
                transcript,
                output,
                None,
                PaneOutputEofState::default(),
                effects,
                None,
            )
        });
        self.child = Some(child);
        self.task = Some(task);
    }

    async fn through(&mut self, marker: &[u8]) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut bytes = Vec::new();
            loop {
                if let OutputCursorItem::Event(event) = self.events.recv().await {
                    bytes.extend_from_slice(event.bytes());
                    if bytes.windows(marker.len()).any(|w| w == marker) {
                        return bytes;
                    }
                }
            }
        })
        .await
        .expect("native reader publication")
    }

    async fn held(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(60), self.events.recv())
                .await
                .is_err(),
            "eligible feed must remain held, without publishing a partial effect"
        );
    }
}

impl Drop for NativeReader {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(pause) = &self.reply_pause {
            pause.release();
        }
        self.transcript
            .lock()
            .set_rename_policy(false, Some(WindowId::new(1)));
        self.queue.drain();
        self.output.retire_reader();
        #[cfg(unix)]
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(child) = self.child.take() {
            reap_fixture(child);
        }
        #[cfg(windows)]
        if let Some(task) = self.task.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !task.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(task.is_finished(), "owned blocking reader must exit");
            let _ = task.join().unwrap();
        }
    }
}

// Runtime shutdown joins this blocking reaper after cancelled async readers
// release their PTY descriptors. No detached thread or child survives the test.
pub(super) fn reap_fixture(spawned: SpawnedPty) {
    let (master, mut child) = spawned.into_parts();
    child.terminate_forcefully().unwrap();
    #[cfg(windows)]
    child.close_pseudoconsole();
    drop(master);
    tokio::task::spawn_blocking(move || {
        child.wait().expect("reap owned rename test child");
    });
}

// Holds the real publication mutex while the pane's policy changes. The gate
// drops before either reader can parse; its Drop also makes failure cleanup safe.
struct PublicationGate(
    Option<mpsc::Sender<()>>,
    Option<std::thread::JoinHandle<()>>,
);
impl PublicationGate {
    fn hold(output: &PaneOutputSender) -> Self {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let output = output.clone();
        let thread = std::thread::spawn(move || {
            output.capture_with_observer(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        Self(Some(release_tx), Some(thread))
    }
}
impl Drop for PublicationGate {
    fn drop(&mut self) {
        self.0.take().unwrap().send(()).unwrap();
        self.1.take().unwrap().join().unwrap();
    }
}

#[tokio::test]
async fn rename_native_reply_backpressure_returns_admission_and_other_pane_publishes() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
    let mut first = NativeReader::prepared(queue.clone(), true);
    #[cfg(windows)]
    let pause = {
        let pause = Arc::new(super::windows_reply_probe::ReplyPause::default());
        first.reply_pause = Some(pause.clone());
        pause
    };
    first.start("query");
    // Unix fills real PTY input with replies to a child that never reads.
    #[cfg(unix)]
    {
        first.through(b"\x1b[c").await;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    // ConPTY can absorb the whole reply burst on some hosts. Pause the actual
    // blocking reader at its reply write instead of depending on pipe capacity.
    #[cfg(windows)]
    assert!(
        pause.wait_until_entered(Duration::from_secs(3)),
        "native reader reached a nonempty parser reply write"
    );
    assert_eq!(queue.stats().pending, 0);
    assert_eq!(
        queue.stats().admitted,
        0,
        "PTY reply wait must not own rename capacity"
    );
    let mut second = NativeReader::prepared(queue.clone(), true);
    second.start("plain");
    second.through(b"PLAIN-DONE").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_native_full_fifo_bypasses_off_but_holds_on() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
    let mut a = NativeReader::prepared(queue.clone(), true);
    a.start("a");
    a.through(b"A-DONE").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!((queue.stats().pending, queue.stats().admitted), (1, 0));
    let mut eligible = NativeReader::prepared(queue.clone(), true);
    eligible.start("plain");
    eligible.held().await;
    let mut b = NativeReader::prepared(queue.clone(), false);
    b.start("b");
    b.through(b"B-DONE").await;
    eligible.held().await;
    assert_eq!(
        queue
            .drain()
            .into_iter()
            .map(|r| r.name)
            .collect::<Vec<_>>(),
        ["A"]
    );
    eligible.through(b"PLAIN-DONE").await;
    assert_eq!(queue.stats().pending, 0, "off pane's B was never accepted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_native_policy_is_revalidated_at_publication_in_both_directions() {
    for enabled_at_parse in [true, false] {
        let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
        let mut a = NativeReader::prepared(queue.clone(), true);
        a.start("a");
        a.through(b"A-DONE").await;
        let mut b = NativeReader::prepared(queue.clone(), !enabled_at_parse);
        let gate = PublicationGate::hold(&b.output);
        b.start("b");
        // This gate deliberately blocks a Tokio worker's synchronous mutex.
        // Do not await that runtime's timer driver before releasing the gate.
        std::thread::sleep(Duration::from_millis(60));
        b.transcript
            .lock()
            .set_rename_policy(enabled_at_parse, Some(WindowId::new(2)));
        drop(gate);
        if enabled_at_parse {
            b.held().await;
            assert_eq!(
                queue
                    .drain()
                    .into_iter()
                    .map(|r| r.name)
                    .collect::<Vec<_>>(),
                ["A"]
            );
            b.through(b"B-DONE").await;
            let names = queue.drain();
            assert_eq!(names.len(), 1);
            assert_eq!(
                (&names[0].name, names[0].owner),
                (&"B".to_owned(), WindowId::new(2))
            );
        } else {
            b.through(b"B-DONE").await;
            assert_eq!(
                queue
                    .drain()
                    .into_iter()
                    .map(|r| r.name)
                    .collect::<Vec<_>>(),
                ["A"]
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_native_split_name_waits_after_off_to_on_without_loss() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));
    let mut a = NativeReader::prepared(queue.clone(), true);
    a.start("a");
    a.through(b"A-DONE").await;
    let mut b = NativeReader::prepared(queue.clone(), false);
    b.start("split");
    b.through(b"SPLIT-READY\x1bkB").await;
    b.transcript
        .lock()
        .set_rename_policy(true, Some(WindowId::new(2)));
    b.child.as_ref().unwrap().master().write_all(b"x").unwrap();
    b.held().await;
    assert_eq!(
        queue
            .drain()
            .into_iter()
            .map(|r| r.name)
            .collect::<Vec<_>>(),
        ["A"]
    );
    b.through(b"SPLIT-DONE").await;
    assert_eq!(
        queue
            .drain()
            .into_iter()
            .map(|r| (r.name, r.owner))
            .collect::<Vec<_>>(),
        [("B".to_owned(), WindowId::new(2))]
    );
}
