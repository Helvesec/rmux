#![cfg(unix)]

use std::error::Error;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use rmux_core::events::{SubscriptionLimits, DEFAULT_OUTPUT_RING_CAPACITY};
use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, KillPaneRequest, ListPanesRequest,
    NewSessionExtRequest, NewSessionRequest, PaneOutputCursorRequest, PaneOutputLagResponse,
    PaneOutputSubscriptionId, PaneOutputSubscriptionStart, PaneSnapshotRequest,
    PaneSnapshotResponse, PaneTarget, Request, Response, SendKeysRequest, SessionName,
    SubscribePaneOutputRequest, SubscribePaneOutputResponse, TerminalSize,
    UnsubscribePaneOutputRequest,
};
use rmux_server::{DaemonConfig, ServerDaemon, ServerHandle};

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

struct Harness {
    runtime: tokio::runtime::Runtime,
    handle: Option<ServerHandle>,
    socket_path: PathBuf,
}

struct Connection {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl Harness {
    fn new(label: &str, limits: SubscriptionLimits) -> Result<Self, Box<dyn Error>> {
        let socket_path = unique_socket_path(label);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let config = DaemonConfig::new(socket_path.clone()).with_subscription_limits(limits);
        let handle = runtime.block_on(ServerDaemon::new(config).bind())?;
        Ok(Self {
            runtime,
            handle: Some(handle),
            socket_path,
        })
    }

    fn connect(&self) -> Result<Connection, Box<dyn Error>> {
        Ok(Connection::connect(&self.socket_path)?)
    }
}

impl Connection {
    fn connect(socket_path: &std::path::Path) -> io::Result<Self> {
        let stream = UnixStream::connect(socket_path)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
        })
    }

    fn roundtrip(&mut self, request: &Request) -> Result<Response, Box<dyn Error>> {
        let frame = encode_frame(request)?;
        self.stream.write_all(&frame)?;
        let mut buffer = [0_u8; 8192];
        loop {
            match self.decoder.next_frame::<Response>() {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(error) => return Err(error.into()),
            }

            let bytes_read = self.stream.read(&mut buffer)?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed before response",
                )
                .into());
            }
            self.decoder.push_bytes(&buffer[..bytes_read]);
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = self.runtime.block_on(handle.shutdown());
        }
        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[test]
fn concurrent_subscribe_unsubscribe_and_caps_cross_server_boundary() -> Result<(), Box<dyn Error>> {
    let limits = SubscriptionLimits::new(2, 8, 2, Duration::from_secs(300));
    let harness = Harness::new("subscriptions-concurrent", limits)?;
    let session = session_name("concurrent");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_session(&harness, session)?;

    let mut threads = Vec::new();
    for _ in 0..6 {
        let socket_path = harness.socket_path.clone();
        let target = target.clone();
        threads.push(thread::spawn(move || -> Result<(), String> {
            let mut connection =
                Connection::connect(&socket_path).map_err(|error| error.to_string())?;
            let subscription =
                subscribe(&mut connection, target).map_err(|error| error.to_string())?;
            let response = connection
                .roundtrip(&Request::UnsubscribePaneOutput(
                    UnsubscribePaneOutputRequest {
                        subscription_id: subscription.subscription_id,
                    },
                ))
                .map_err(|error| error.to_string())?;
            match response {
                Response::UnsubscribePaneOutput(response) if response.removed => Ok(()),
                other => Err(format!("unexpected unsubscribe response: {other:?}")),
            }
        }));
    }
    for thread in threads {
        thread
            .join()
            .expect("thread should not panic")
            .map_err(std::io::Error::other)?;
    }

    let mut capped = harness.connect()?;
    let _first = subscribe(&mut capped, target.clone())?;
    let _second = subscribe(&mut capped, target.clone())?;
    assert_limit_error(capped.roundtrip(&Request::SubscribePaneOutput(
        SubscribePaneOutputRequest {
            target: target.clone(),
            start: PaneOutputSubscriptionStart::Now,
        },
    ))?);

    drop(capped);

    let pane_session = session_name("pane-cap");
    let pane_target = PaneTarget::with_window(pane_session.clone(), 0, 0);
    create_session(&harness, pane_session)?;
    let mut pane_connections = Vec::new();
    for _ in 0..limits.max_per_pane() {
        let mut connection = harness.connect()?;
        let _ = subscribe(&mut connection, pane_target.clone())?;
        pane_connections.push(connection);
    }
    let mut rejected = harness.connect()?;
    assert_limit_error(rejected.roundtrip(&Request::SubscribePaneOutput(
        SubscribePaneOutputRequest {
            target: pane_target,
            start: PaneOutputSubscriptionStart::Now,
        },
    ))?);

    Ok(())
}

#[test]
fn disconnect_ttl_and_pane_removal_cleanup_release_subscription_caps() -> Result<(), Box<dyn Error>>
{
    let limits = SubscriptionLimits::new(4, 1, 2, Duration::from_millis(30));
    let harness = Harness::new("subscriptions-cleanup", limits)?;

    let disconnect_session = session_name("disconnect");
    let disconnect_target = PaneTarget::with_window(disconnect_session.clone(), 0, 0);
    create_session(&harness, disconnect_session)?;
    {
        let mut connection = harness.connect()?;
        let _ = subscribe(&mut connection, disconnect_target.clone())?;
    }
    eventually_subscribes(&harness, disconnect_target.clone())?;

    let ttl_session = session_name("ttl");
    let ttl_target = PaneTarget::with_window(ttl_session.clone(), 0, 0);
    create_session(&harness, ttl_session)?;
    let mut stale_connection = harness.connect()?;
    let stale = subscribe(&mut stale_connection, ttl_target.clone())?;
    thread::sleep(Duration::from_millis(60));
    let mut replacement = harness.connect()?;
    let _ = subscribe(&mut replacement, ttl_target)?;
    assert!(matches!(
        stale_connection.roundtrip(&Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: stale.subscription_id,
            max_events: Some(1),
        }))?,
        Response::Error(_)
    ));

    let pane_session = session_name("pane-removal");
    let pane_target = PaneTarget::with_window(pane_session.clone(), 0, 0);
    create_session(&harness, pane_session)?;
    let mut subscribed = harness.connect()?;
    let removed = subscribe(&mut subscribed, pane_target.clone())?;
    let mut killer = harness.connect()?;
    assert!(matches!(
        killer.roundtrip(&Request::KillPane(KillPaneRequest {
            target: pane_target,
            kill_all_except: false,
        }))?,
        Response::KillPane(_)
    ));
    assert!(matches!(
        subscribed.roundtrip(&Request::UnsubscribePaneOutput(
            UnsubscribePaneOutputRequest {
                subscription_id: removed.subscription_id,
            },
        ))?,
        Response::UnsubscribePaneOutput(response) if !response.removed
    ));

    Ok(())
}

#[test]
fn automatic_pane_exit_cleanup_releases_connection_subscription_cap() -> Result<(), Box<dyn Error>>
{
    let limits = SubscriptionLimits::new(1, 1, 2, Duration::from_secs(300));
    let harness = Harness::new("subscriptions-auto-exit-cleanup", limits)?;

    let stable_session = session_name("auto-exit-stable");
    let stable_target = PaneTarget::with_window(stable_session.clone(), 0, 0);
    create_session(&harness, stable_session)?;

    let short_session = session_name("auto-exit");
    let short_target = PaneTarget::with_window(short_session.clone(), 0, 0);
    create_short_lived_shell_session(&harness, short_session)?;

    let mut connection = harness.connect()?;
    let _ = subscribe(&mut connection, short_target)?;
    let _ = eventually_subscribes_on_connection(&mut connection, stable_target)?;

    Ok(())
}

#[test]
fn slow_subscriber_batch_cap_then_lag_resumes_at_oldest_retained_event(
) -> Result<(), Box<dyn Error>> {
    let limits = SubscriptionLimits::new(4, 4, 1, Duration::from_secs(300));
    let harness = Harness::new("subscriptions-lag", limits)?;
    let session = session_name("lag");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    create_interactive_shell_session(&harness, session)?;

    let mut subscriber = harness.connect()?;
    let subscription = subscribe(&mut subscriber, target.clone())?;

    let mut sender = harness.connect()?;
    send_keys(&mut sender, target.clone(), "printf rmux_subscription_one")
        .map_err(|error| io::Error::other(format!("first send-keys failed: {error}")))?;
    thread::sleep(Duration::from_millis(80));
    send_keys(&mut sender, target.clone(), "printf rmux_subscription_two")
        .map_err(|error| io::Error::other(format!("second send-keys failed: {error}")))?;
    let cursor_sequence = wait_for_limited_cursor(&mut subscriber, subscription.subscription_id)
        .map_err(|error| io::Error::other(format!("limited cursor failed: {error}")))?;

    send_keys(
        &mut sender,
        target.clone(),
        "yes rmux_subscription_lag | head -c 10000000",
    )
    .map_err(|error| io::Error::other(format!("lag send-keys failed: {error}")))?;
    wait_for_output_sequence(
        &mut sender,
        target.clone(),
        cursor_sequence + u64::try_from(DEFAULT_OUTPUT_RING_CAPACITY)? + 16,
    )
    .map_err(|error| io::Error::other(format!("lag output failed: {error}")))?;
    let lag = wait_for_lag(&mut subscriber, subscription.subscription_id)
        .map_err(|error| io::Error::other(format!("lag cursor failed: {error}")))?;
    let resume_sequence = lag.lag.resume_sequence;
    let newest_sequence = lag.lag.newest_sequence;
    assert!(resume_sequence <= newest_sequence);
    assert!(!lag.lag.recent.bytes.is_empty());
    assert_eq!(lag.lag.recent.newest_sequence, Some(newest_sequence));

    let snapshot = snapshot_response(&mut sender, target.clone())?;
    assert_snapshot_shape(&snapshot);
    assert_ne!(snapshot.revision, 0);
    assert!(
        snapshot_text(&snapshot).contains("rmux_subscription_lag"),
        "fresh snapshot should recover current pane cells after output lag"
    );

    let listed_sequence = listed_output_sequence(&mut sender, target.clone())?;
    assert!(
        listed_sequence > newest_sequence,
        "list-panes should remain a fresh state lane after output cursor lag"
    );

    let (resume_sequence, cursor) = wait_for_cursor_after_lag(
        &mut subscriber,
        subscription.subscription_id,
        resume_sequence,
    )?;
    assert_eq!(cursor.events.len(), 1);
    assert_eq!(cursor.events[0].sequence, resume_sequence);
    assert!(
        cursor.cursor.next_sequence > resume_sequence,
        "cursor should advance after delivering the oldest retained event"
    );

    Ok(())
}

fn create_short_lived_shell_session(
    harness: &Harness,
    session_name: SessionName,
) -> Result<(), Box<dyn Error>> {
    let mut connection = harness.connect()?;
    let response = connection.roundtrip(&Request::NewSessionExt(NewSessionExtRequest {
        session_name: Some(session_name),
        working_directory: None,
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
        group_target: None,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: None,
        print_session_info: false,
        print_format: None,
        command: Some(vec!["sh".to_owned(), "-c".to_owned(), "sleep 1".to_owned()]),
        process_command: None,
    }))?;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    Ok(())
}

fn create_session(harness: &Harness, session_name: SessionName) -> Result<(), Box<dyn Error>> {
    let mut connection = harness.connect()?;
    let response = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name,
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
    }))?;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    Ok(())
}

fn create_interactive_shell_session(
    harness: &Harness,
    session_name: SessionName,
) -> Result<(), Box<dyn Error>> {
    let mut connection = harness.connect()?;
    let response = connection.roundtrip(&Request::NewSessionExt(NewSessionExtRequest {
        session_name: Some(session_name),
        working_directory: None,
        detached: true,
        size: Some(TerminalSize {
            cols: 100,
            rows: 30,
        }),
        environment: None,
        group_target: None,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: None,
        print_session_info: false,
        print_format: None,
        command: Some(vec!["sh".to_owned(), "-i".to_owned()]),
        process_command: None,
    }))?;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    thread::sleep(Duration::from_millis(100));
    Ok(())
}

fn subscribe(
    connection: &mut Connection,
    target: PaneTarget,
) -> Result<SubscribePaneOutputResponse, Box<dyn Error>> {
    match connection.roundtrip(&Request::SubscribePaneOutput(SubscribePaneOutputRequest {
        target,
        start: PaneOutputSubscriptionStart::Now,
    }))? {
        Response::SubscribePaneOutput(response) => Ok(response),
        other => Err(format!("unexpected subscribe response: {other:?}").into()),
    }
}

fn eventually_subscribes(harness: &Harness, target: PaneTarget) -> Result<(), Box<dyn Error>> {
    for _ in 0..100 {
        let mut connection = harness.connect()?;
        match subscribe(&mut connection, target.clone()) {
            Ok(_) => return Ok(()),
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    Err("subscription cap was not released after disconnect".into())
}

fn eventually_subscribes_on_connection(
    connection: &mut Connection,
    target: PaneTarget,
) -> Result<SubscribePaneOutputResponse, Box<dyn Error>> {
    for _ in 0..200 {
        match subscribe(connection, target.clone()) {
            Ok(response) => return Ok(response),
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
    Err("subscription cap was not released after pane exit".into())
}

fn send_keys(
    connection: &mut Connection,
    target: PaneTarget,
    command: &str,
) -> Result<(), Box<dyn Error>> {
    let response = connection.roundtrip(&Request::SendKeys(SendKeysRequest {
        target,
        keys: vec![command.to_owned(), "Enter".to_owned()],
    }))?;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
    Ok(())
}

fn wait_for_limited_cursor(
    connection: &mut Connection,
    subscription_id: PaneOutputSubscriptionId,
) -> Result<u64, Box<dyn Error>> {
    for _ in 0..100 {
        let response =
            connection.roundtrip(&Request::PaneOutputCursor(PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(1),
            }))?;
        match response {
            Response::PaneOutputCursor(cursor) => {
                assert!(cursor.events.len() <= 1);
                if cursor.limited {
                    return Ok(cursor.cursor.next_sequence);
                }
            }
            Response::PaneOutputLag(lag) => return Ok(lag.cursor.next_sequence),
            Response::Error(ErrorResponse { error }) => return Err(error.into()),
            other => return Err(format!("unexpected cursor response: {other:?}").into()),
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("cursor response never hit the batch cap".into())
}

fn wait_for_output_sequence(
    connection: &mut Connection,
    target: PaneTarget,
    minimum_sequence: u64,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..200 {
        let response = connection.roundtrip(&Request::ListPanes(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some("#{pane_output_sequence}".to_owned()),
            target_window_index: Some(target.window_index()),
        }))?;
        let Response::ListPanes(response) = response else {
            return Err(format!("unexpected list-panes response: {response:?}").into());
        };
        let stdout = String::from_utf8(response.output.stdout().to_vec())?;
        let sequence = stdout.trim().parse::<u64>()?;
        if sequence >= minimum_sequence {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("pane output sequence did not reach {minimum_sequence}").into())
}

fn wait_for_lag(
    connection: &mut Connection,
    subscription_id: PaneOutputSubscriptionId,
) -> Result<PaneOutputLagResponse, Box<dyn Error>> {
    for _ in 0..200 {
        let response =
            connection.roundtrip(&Request::PaneOutputCursor(PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(1),
            }))?;
        match response {
            Response::PaneOutputLag(lag) => {
                assert_eq!(lag.cursor.next_sequence, lag.lag.resume_sequence);
                assert_eq!(lag.cursor.missed_events, lag.lag.missed_events);
                return Ok(lag);
            }
            Response::PaneOutputCursor(cursor) => {
                assert!(cursor.events.len() <= 1);
            }
            Response::Error(ErrorResponse { error }) => return Err(error.into()),
            other => return Err(format!("unexpected cursor response: {other:?}").into()),
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err("subscription did not report lag".into())
}

fn snapshot_response(
    connection: &mut Connection,
    target: PaneTarget,
) -> Result<PaneSnapshotResponse, Box<dyn Error>> {
    match connection.roundtrip(&Request::PaneSnapshot(PaneSnapshotRequest { target }))? {
        Response::PaneSnapshot(snapshot) => Ok(snapshot),
        other => Err(format!("unexpected snapshot response: {other:?}").into()),
    }
}

fn assert_snapshot_shape(snapshot: &PaneSnapshotResponse) {
    assert_eq!(
        snapshot.cells.len(),
        usize::from(snapshot.cols) * usize::from(snapshot.rows)
    );
    assert!(snapshot.cursor.row < snapshot.rows);
    assert!(snapshot.cursor.col < snapshot.cols);
}

fn snapshot_text(snapshot: &PaneSnapshotResponse) -> String {
    snapshot
        .cells
        .chunks(usize::from(snapshot.cols))
        .map(|row| {
            row.iter()
                .filter(|cell| !cell.padding)
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn listed_output_sequence(
    connection: &mut Connection,
    target: PaneTarget,
) -> Result<u64, Box<dyn Error>> {
    let response = connection.roundtrip(&Request::ListPanes(ListPanesRequest {
        target: target.session_name().clone(),
        format: Some("#{pane_output_sequence}".to_owned()),
        target_window_index: Some(target.window_index()),
    }))?;
    let Response::ListPanes(response) = response else {
        return Err(format!("unexpected list-panes response: {response:?}").into());
    };
    Ok(String::from_utf8(response.output.stdout().to_vec())?
        .trim()
        .parse::<u64>()?)
}

fn wait_for_cursor_after_lag(
    connection: &mut Connection,
    subscription_id: PaneOutputSubscriptionId,
    mut resume_sequence: u64,
) -> Result<(u64, rmux_proto::PaneOutputCursorResponse), Box<dyn Error>> {
    for _ in 0..400 {
        let response =
            connection.roundtrip(&Request::PaneOutputCursor(PaneOutputCursorRequest {
                subscription_id,
                max_events: Some(1),
            }))?;
        match response {
            Response::PaneOutputCursor(cursor) if !cursor.events.is_empty() => {
                return Ok((resume_sequence, cursor));
            }
            Response::PaneOutputCursor(cursor) => {
                assert!(cursor.events.is_empty());
            }
            Response::PaneOutputLag(lag) => {
                assert_eq!(lag.cursor.next_sequence, lag.lag.resume_sequence);
                assert!(lag.cursor.missed_events >= lag.lag.missed_events);
                resume_sequence = lag.lag.resume_sequence;
            }
            Response::Error(ErrorResponse { error }) => return Err(error.into()),
            other => return Err(format!("unexpected cursor response: {other:?}").into()),
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err("cursor did not deliver the oldest retained event after lag (10s timeout)".into())
}

fn assert_limit_error(response: Response) {
    match response {
        Response::Error(ErrorResponse { error }) => {
            assert!(
                error.to_string().contains("subscription limit exceeded"),
                "unexpected error: {error}"
            );
        }
        other => panic!("expected cap rejection, got {other:?}"),
    }
}

fn session_name(label: &str) -> SessionName {
    let unique = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    SessionName::new(format!("sub-{label}-{unique}")).expect("valid session")
}

fn unique_socket_path(label: &str) -> PathBuf {
    let unique = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    PathBuf::from("/tmp")
        .join(format!(
            "rmux-subscriptions-{label}-{}-{unique}",
            std::process::id()
        ))
        .join("s.sock")
}
