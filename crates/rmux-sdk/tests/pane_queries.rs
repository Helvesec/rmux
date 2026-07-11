#![cfg(unix)]

mod common;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use rmux_proto::{
    encode_frame, ClearHistoryRequest, FrameDecoder, HasSessionRequest, KillPaneRequest,
    LinkWindowRequest, ListPanesRequest, NewWindowRequest, OptionName, PaneTarget, Request,
    ResizeWindowRequest, Response, ScopeSelector, SendKeysRequest, SetOptionMode, SetOptionRequest,
    WindowTarget,
};
use rmux_sdk::{EnsureSession, PaneProcessState, PaneRef, PaneSnapshot, RmuxBuilder, SessionName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::Instant;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static LIVE_DAEMON_LOCK: common::unix_smoke::LiveDaemonLock =
    common::unix_smoke::LiveDaemonLock::new();
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn pane_id_info_and_snapshot_resolve_through_daemon_for_live_and_stale_slots() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-basic").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpanealpha");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;

    let pane = session.pane(0, 0);
    assert_eq!(pane.target(), &PaneRef::new(alpha.clone(), 0, 0));

    let pane_id = pane.id().await?.expect("live pane is listed");
    assert!(pane.exists().await?);
    let raw_id = raw_first_pane_id(harness.socket_path(), alpha.clone(), 0).await?;
    assert_eq!(pane_id.to_string(), raw_id);

    let info = pane.info().await?;
    assert_eq!(info.sessions.len(), 1);
    assert_eq!(info.windows.len(), 1);
    assert_eq!(info.panes.len(), 1);
    assert_eq!(info.panes[0].id, pane_id);
    assert_eq!(info.panes[0].window_id, info.windows[0].id);
    assert_eq!(info.panes[0].session_id, info.sessions[0].id);
    assert_eq!(info.panes[0].index, 0);
    assert!(matches!(
        info.panes[0].process,
        PaneProcessState::Running { .. } | PaneProcessState::Unknown
    ));
    assert!(info.panes[0].exit_state.is_none());
    assert_ne!(info.panes[0].revision, 0);
    assert!(info.panes[0].size.cols > 0);
    assert!(info.panes[0].size.rows > 0);

    let snapshot = pane.snapshot().await?;
    assert!(snapshot.is_row_major_shape());
    assert_eq!(snapshot.cols, info.panes[0].size.cols);
    assert_eq!(snapshot.rows, info.panes[0].size.rows);
    assert_ne!(snapshot.revision, 0);

    let stale = session.pane(99, 0);
    assert_eq!(stale.id().await?, None);
    assert!(!stale.exists().await?);
    let stale_info = stale.info().await?;
    assert_eq!(stale_info.sessions.len(), 1);
    assert!(stale_info.windows.is_empty());
    assert!(stale_info.panes.is_empty());
    let stale_snapshot = stale.snapshot().await?;
    assert_eq!(stale_snapshot, PaneSnapshot::default());
    assert_eq!(stale_snapshot.revision, 0);

    let stale_pane_in_window = session.pane(0, 99);
    assert_eq!(stale_pane_in_window.id().await?, None);
    let stale_pane_info = stale_pane_in_window.info().await?;
    assert_eq!(stale_pane_info.sessions.len(), 1);
    assert_eq!(stale_pane_info.windows.len(), 1);
    assert!(stale_pane_info.panes.is_empty());
    assert_eq!(
        stale_pane_in_window.snapshot().await?,
        PaneSnapshot::default()
    );

    let unknown_session_pane = rmux
        .pane(PaneRef::new(session_name("sdknotthere"), 0, 0))
        .await?;
    assert_eq!(unknown_session_pane.id().await?, None);
    let unknown_info = unknown_session_pane.info().await?;
    assert!(unknown_info.sessions.is_empty());
    assert!(unknown_info.windows.is_empty());
    assert!(unknown_info.panes.is_empty());
    assert_eq!(
        unknown_session_pane.snapshot().await?,
        PaneSnapshot::default()
    );

    harness.finish().await
}

// Regression guard for the by-slot index mismatch under a non-zero base-index: an
// index/slot handle (`session.pane(w, p)`) must resolve the real live pane even
// when the daemon runs with a non-zero `base-index`/`pane-base-index`, instead of
// silently degrading to a blank `PaneSnapshot::default()` (revision 0). Before the
// fix, a slot handle snapshot went to the by-slot endpoint, which resolves
// `PaneTarget.pane_index` as a *raw* pane index. Under pane-base-index 1 the live
// pane's visible index (1) differs from its raw index (0), so the by-slot lookup
// missed and the snapshot degraded to the blank stale default, while the by-id
// discovery path (which maps the stable pane id back to the raw index inside the
// daemon) returned real content.
#[tokio::test]
async fn index_pane_handle_resolves_live_pane_under_nonzero_base_index() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-base-index").await?;
    let rmux = harness.rmux();

    // Set base-index/pane-base-index globally *before* creating the session so the
    // session's sole window is keyed at daemon index 1 and its pane is reported at
    // pane index 1 (base-adjusted), exactly the environment that triggered the bug.
    raw_set_global_option(harness.socket_path(), OptionName::BaseIndex, "1").await?;
    raw_set_global_option(harness.socket_path(), OptionName::PaneBaseIndex, "1").await?;

    let alpha = session_name("sdkpanebaseidx");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;

    // The by-id discovery path has always resolved correctly (it matches on the
    // base-index-independent stable pane id). Use it as the source of truth for the
    // live pane's identity and content.
    let by_id = rmux.find_panes().session(alpha.as_str()).one().await?;
    let by_id_id = by_id.id().await?.expect("discovered pane is listed");
    let by_id_snapshot = by_id.snapshot().await?;
    assert_ne!(
        by_id_snapshot.revision, 0,
        "by-id handle must observe the live pane"
    );

    // The literal daemon-reported indices under base-index 1 / pane-base-index 1.
    let by_slot = session.pane(1, 1);
    assert_eq!(by_slot.target(), &PaneRef::new(alpha.clone(), 1, 1));

    let by_slot_id = by_slot
        .id()
        .await?
        .expect("index handle must resolve the live pane under non-zero base-index");
    assert_eq!(
        by_slot_id, by_id_id,
        "the index/slot handle must resolve to the same live pane as the by-id path"
    );

    let by_slot_snapshot = by_slot.snapshot().await?;
    assert_ne!(
        by_slot_snapshot.revision, 0,
        "index handle snapshot must not be the blank stale-slot default"
    );
    assert_eq!(
        (by_slot_snapshot.cols, by_slot_snapshot.rows),
        (by_id_snapshot.cols, by_id_snapshot.rows),
        "index and by-id snapshots must describe the same live pane grid"
    );
    assert_eq!(
        by_slot_snapshot.visible_text(),
        by_id_snapshot.visible_text(),
        "the index/slot snapshot content must match the by-id snapshot for the same pane"
    );

    // *Literal* index semantics are preserved (tmux-like): under base-index 1 no
    // window is keyed at literal index 0, so `pane(0, 0)` genuinely addresses an
    // absent slot and must still yield the documented stale default. The fix
    // resolves real panes at their base-adjusted indices; it does not reinterpret
    // `(0, 0)` as "first window / first pane".
    let literal_absent = session.pane(0, 0);
    assert_eq!(literal_absent.id().await?, None);
    assert_eq!(literal_absent.snapshot().await?, PaneSnapshot::default());
    assert_eq!(literal_absent.snapshot().await?.revision, 0);

    // A genuinely-absent slot must still yield the documented stale default,
    // protecting the contract exercised by the stale-slot assertions above.
    let genuinely_absent = session.pane(9, 9);
    assert_eq!(genuinely_absent.id().await?, None);
    assert_eq!(genuinely_absent.snapshot().await?, PaneSnapshot::default());
    assert_eq!(genuinely_absent.snapshot().await?.revision, 0);

    harness.finish().await
}

#[tokio::test]
async fn pane_snapshot_revision_changes_after_output_resize_clear_and_exit() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-revisions").await?;
    let rmux = harness.rmux();
    let alpha = session_name("sdkpanerev");
    let session = EnsureSession::named(alpha.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    raw_new_window(harness.socket_path(), alpha.clone(), 99).await?;
    let pane = session.pane(0, 0);
    let pane_id = pane.id().await?.expect("live pane");

    let baseline = pane.snapshot().await?;
    assert_ne!(baseline.revision, 0);

    let marker = "rmux_sdk_pane_rev_marker_alpha";
    raw_send_keys(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
        vec![format!("printf '{marker}\\n'"), "Enter".to_owned()],
    )
    .await?;
    let after_output = wait_for_revision_change(&pane, baseline.revision, marker).await?;
    assert_ne!(after_output.revision, baseline.revision);
    assert!(after_output.visible_text().contains(marker));

    let resized_size = (after_output.cols.saturating_add(4), after_output.rows);
    raw_resize_window(
        harness.socket_path(),
        WindowTarget::with_window(alpha.clone(), 0),
        resized_size.0,
        resized_size.1,
    )
    .await?;
    let after_resize = wait_for_pane_dimensions(&pane, resized_size).await?;
    assert_eq!(after_resize.cols, resized_size.0);
    assert_eq!(after_resize.rows, resized_size.1);
    assert_ne!(after_resize.revision, after_output.revision);

    let history_lines = usize::from(after_resize.rows).saturating_add(8);
    let history_markers = (0..history_lines)
        .map(|index| format!("rmux_sdk_pane_rev_history_{index}"))
        .collect::<Vec<_>>();
    let history_tail = history_markers
        .last()
        .expect("history marker list is non-empty")
        .clone();
    raw_send_keys(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
        vec![
            format!("printf '{}\\n'", history_markers.join("\\n")),
            "Enter".to_owned(),
        ],
    )
    .await?;
    let after_history =
        wait_for_revision_change(&pane, after_resize.revision, &history_tail).await?;
    assert!(
        after_history.visible_text().contains(&history_tail),
        "history tail must be visible before clear-history"
    );

    raw_clear_history(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
    )
    .await?;
    let after_clear = wait_for_revision_change(&pane, after_history.revision, "").await?;
    assert_ne!(after_clear.revision, after_history.revision);

    raw_kill_pane(
        harness.socket_path(),
        PaneTarget::with_window(alpha.clone(), 0, 0),
    )
    .await?;
    let after_exit = wait_for_pane_unlisted(&pane).await?;
    assert_eq!(after_exit, PaneSnapshot::default());
    assert_eq!(after_exit.revision, 0);
    assert_ne!(after_exit.revision, after_clear.revision);

    let exit_info = pane.info().await?;
    assert_eq!(exit_info.sessions.len(), 1);
    assert!(exit_info.panes.is_empty());

    raw_new_window(harness.socket_path(), alpha.clone(), 0).await?;
    let revived = wait_for_pane_listed(&pane).await?;
    assert_ne!(
        revived, pane_id,
        "a recycled (session, window, pane) slot must expose a fresh %N pane id"
    );
    let revived_snapshot = pane.snapshot().await?;
    assert_ne!(revived_snapshot.revision, 0);
    assert_ne!(
        revived_snapshot.revision, after_exit.revision,
        "a freshly revived pane must not collide with the prior unlisted-revision sentinel"
    );

    harness.finish().await
}

#[tokio::test]
async fn pane_id_resolves_to_same_identity_through_linked_window_views() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-linked").await?;
    let rmux = harness.rmux();

    let owner = session_name("sdkpanelinkown");
    let viewer = session_name("sdkpanelinkview");
    let owner_session = EnsureSession::named(owner.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    EnsureSession::named(viewer.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    raw_link_window(harness.socket_path(), owner.clone(), 0, viewer.clone(), 1).await?;

    let owner_pane = owner_session.pane(0, 0);
    let owner_id = owner_pane.id().await?.expect("owner pane is listed");

    let viewer_pane = rmux.pane(PaneRef::new(viewer.clone(), 1, 0)).await?;
    let viewer_id = viewer_pane
        .id()
        .await?
        .expect("linked viewer pane is listed");

    assert_eq!(
        owner_id, viewer_id,
        "linked windows must expose the same %N pane identity through every session"
    );

    let owner_info = owner_pane.info().await?;
    let viewer_info = viewer_pane.info().await?;
    assert_eq!(owner_info.panes.len(), 1);
    assert_eq!(viewer_info.panes.len(), 1);
    assert_eq!(owner_info.panes[0].id, viewer_info.panes[0].id);
    assert_eq!(
        owner_info.panes[0].window_id,
        viewer_info.panes[0].window_id
    );
    assert_ne!(
        owner_info.panes[0].session_id, viewer_info.panes[0].session_id,
        "the two views are owned by distinct sessions"
    );

    harness.finish().await
}

#[tokio::test]
async fn pane_id_resolves_to_same_identity_through_grouped_session_views() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("pane-grouped").await?;
    let rmux = harness.rmux();

    let primary = session_name("sdkpanegrouppri");
    let mirror = session_name("sdkpanegroupmir");
    let primary_session = EnsureSession::named(primary.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    raw_new_window(harness.socket_path(), primary.clone(), 1).await?;
    EnsureSession::named(mirror.clone())
        .create_only()
        .group_target(primary.clone())
        .ensure(&rmux)
        .await?;

    let primary_pane = primary_session.pane(1, 0);
    let primary_id = primary_pane.id().await?.expect("primary pane is listed");

    let mirror_pane = rmux.pane(PaneRef::new(mirror.clone(), 1, 0)).await?;
    let mirror_id = mirror_pane
        .id()
        .await?
        .expect("grouped mirror pane is listed");

    assert_eq!(
        primary_id, mirror_id,
        "grouped sessions must expose the same %N pane identity through every member"
    );

    let primary_info = primary_pane.info().await?;
    let mirror_info = mirror_pane.info().await?;
    assert_eq!(primary_info.panes.len(), 1);
    assert_eq!(mirror_info.panes.len(), 1);
    assert_eq!(primary_info.panes[0].id, mirror_info.panes[0].id);
    assert_eq!(
        primary_info.panes[0].window_id,
        mirror_info.panes[0].window_id
    );

    harness.finish().await
}

async fn wait_for_revision_change(
    pane: &rmux_sdk::Pane,
    previous_revision: u64,
    text_marker: &str,
) -> TestResult<PaneSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = pane.snapshot().await?;
        let text_ok = text_marker.is_empty() || snapshot.visible_text().contains(text_marker);
        if snapshot.revision != previous_revision && text_ok {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane revision did not change from {previous_revision} within deadline"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_pane_dimensions(
    pane: &rmux_sdk::Pane,
    expected: (u16, u16),
) -> TestResult<PaneSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = pane.snapshot().await?;
        if snapshot.cols == expected.0 && snapshot.rows == expected.1 {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane dimensions did not reach {expected:?} (got {}x{})",
                snapshot.cols, snapshot.rows
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_pane_unlisted(pane: &rmux_sdk::Pane) -> TestResult<PaneSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if pane.id().await?.is_none() {
            return Ok(pane.snapshot().await?);
        }
        if Instant::now() >= deadline {
            return Err("pane remained listed after kill".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_pane_listed(pane: &rmux_sdk::Pane) -> TestResult<rmux_sdk::PaneId> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(id) = pane.id().await? {
            return Ok(id);
        }
        if Instant::now() >= deadline {
            return Err("pane was not listed within deadline".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn raw_first_pane_id(
    socket_path: &Path,
    target: SessionName,
    window_index: u32,
) -> TestResult<String> {
    match framed_request(
        socket_path,
        Request::ListPanes(ListPanesRequest {
            target,
            target_window_index: Some(window_index),
            format: Some("#{pane_id}".to_owned()),
        }),
    )
    .await?
    {
        Response::ListPanes(response) => {
            let stdout = String::from_utf8_lossy(response.output.stdout()).to_string();
            stdout
                .lines()
                .next()
                .map(str::to_owned)
                .ok_or_else(|| "list-panes returned no rows".into())
        }
        response => Err(format!("unexpected list-panes response: {response:?}").into()),
    }
}

async fn raw_set_global_option(socket_path: &Path, option: OptionName, value: &str) -> TestResult {
    match framed_request(
        socket_path,
        Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }),
    )
    .await?
    {
        Response::SetOption(_) => Ok(()),
        response => Err(format!("unexpected set-option response: {response:?}").into()),
    }
}

async fn raw_send_keys(socket_path: &Path, target: PaneTarget, keys: Vec<String>) -> TestResult {
    match framed_request(
        socket_path,
        Request::SendKeys(SendKeysRequest { target, keys }),
    )
    .await?
    {
        Response::SendKeys(_) => Ok(()),
        response => Err(format!("unexpected send-keys response: {response:?}").into()),
    }
}

async fn raw_resize_window(
    socket_path: &Path,
    target: WindowTarget,
    width: u16,
    height: u16,
) -> TestResult {
    match framed_request(
        socket_path,
        Request::ResizeWindow(ResizeWindowRequest {
            target,
            width: Some(width),
            height: Some(height),
            adjustment: None,
        }),
    )
    .await?
    {
        Response::ResizeWindow(_) => Ok(()),
        response => Err(format!("unexpected resize-window response: {response:?}").into()),
    }
}

async fn raw_clear_history(socket_path: &Path, target: PaneTarget) -> TestResult {
    match framed_request(
        socket_path,
        Request::ClearHistory(ClearHistoryRequest {
            target,
            reset_hyperlinks: false,
        }),
    )
    .await?
    {
        Response::ClearHistory(_) => Ok(()),
        response => Err(format!("unexpected clear-history response: {response:?}").into()),
    }
}

async fn raw_kill_pane(socket_path: &Path, target: PaneTarget) -> TestResult {
    match framed_request(
        socket_path,
        Request::KillPane(KillPaneRequest {
            target,
            kill_all_except: false,
        }),
    )
    .await?
    {
        Response::KillPane(_) => Ok(()),
        response => Err(format!("unexpected kill-pane response: {response:?}").into()),
    }
}

async fn raw_new_window(socket_path: &Path, target: SessionName, index: u32) -> TestResult {
    match framed_request(
        socket_path,
        Request::NewWindow(Box::new(NewWindowRequest {
            target: target.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
        })),
    )
    .await?
    {
        Response::NewWindow(response) => {
            assert_eq!(response.target, WindowTarget::with_window(target, index));
            Ok(())
        }
        response => Err(format!("unexpected new-window response: {response:?}").into()),
    }
}

async fn raw_link_window(
    socket_path: &Path,
    source_session: SessionName,
    source_index: u32,
    target_session: SessionName,
    target_index: u32,
) -> TestResult {
    match framed_request(
        socket_path,
        Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(source_session, source_index),
            target: WindowTarget::with_window(target_session.clone(), target_index),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }),
    )
    .await?
    {
        Response::LinkWindow(response) => {
            assert_eq!(
                response.target,
                WindowTarget::with_window(target_session, target_index)
            );
            Ok(())
        }
        response => Err(format!("unexpected link-window response: {response:?}").into()),
    }
}

async fn framed_request(socket_path: &Path, request: Request) -> TestResult<Response> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let frame = encode_frame(&request)?;
    stream.write_all(&frame).await?;
    read_response(&mut stream).await
}

async fn read_response(stream: &mut UnixStream) -> TestResult<Response> {
    let mut decoder = FrameDecoder::new();
    let mut read_buffer = [0_u8; 8192];

    loop {
        if let Some(response) = decoder.next_frame::<Response>()? {
            return Ok(response);
        }

        let bytes_read = stream.read(&mut read_buffer).await?;
        if bytes_read == 0 {
            return Err("connection closed before response frame".into());
        }
        decoder.push_bytes(&read_buffer[..bytes_read]);
    }
}

struct Harness {
    _root: TestRoot,
    socket_path: PathBuf,
    child: Option<Child>,
}

impl Harness {
    async fn start(label: &str) -> TestResult<Self> {
        let root = TestRoot::new(label);
        let socket_path = root.path().join("daemon.sock");
        let mut child = Command::new(rmux_binary()?)
            .arg("--__internal-daemon")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_for_daemon_ready(&socket_path, &mut child).await?;

        Ok(Self {
            _root: root,
            socket_path,
            child: Some(child),
        })
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn rmux(&self) -> rmux_sdk::Rmux {
        RmuxBuilder::new().unix_socket(&self.socket_path).build()
    }

    async fn finish(self) -> TestResult {
        let shutdown = self.rmux().shutdown().await;
        wait_for_child_exit(self, "server did not exit during cleanup").await?;
        if let Err(error) = shutdown {
            let rendered = error.to_string();
            assert!(
                rendered.contains("connect to rmux daemon")
                    || rendered.contains("rmux daemon closed the transport")
                    || rendered.contains("rmux transport actor is closed")
                    || rendered.contains("Connection reset by peer"),
                "unexpected cleanup shutdown error: {rendered}"
            );
        }
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

async fn wait_for_child_exit(mut harness: Harness, timeout_message: &'static str) -> TestResult {
    let mut child = harness.child.take().expect("harness owns daemon child");
    let deadline = Instant::now() + Duration::from_secs(60);

    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success(), "daemon exited with status {status}");
            return Ok(());
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(timeout_message.into());
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_daemon_ready(socket_path: &Path, child: &mut Child) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(60);
    let probe = session_name("sdkprobe");

    loop {
        if let Some(status) = child.try_wait()? {
            return Err(format!("daemon exited before accepting RPC: {status}").into());
        }

        if matches!(
            framed_request(
                socket_path,
                Request::HasSession(HasSessionRequest {
                    target: probe.clone()
                })
            )
            .await,
            Ok(Response::HasSession(_))
        ) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "daemon at '{}' did not accept RPC before timeout",
                socket_path.display()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn rmux_binary() -> TestResult<&'static Path> {
    static RMUX_BINARY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match RMUX_BINARY.get_or_init(|| resolve_rmux_binary().map_err(|error| error.to_string())) {
        Ok(path) => Ok(path.as_path()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}

fn resolve_rmux_binary() -> TestResult<PathBuf> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_rmux") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let target_dir = target_dir()?;
    let candidate = target_dir.join("debug").join("rmux");
    let status =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .arg("build")
            .arg("--bin")
            .arg("rmux")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(workspace_root().join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", &target_dir)
            .status()?;
    if !status.success() {
        return Err(format!("failed to build rmux binary for daemon tests: {status}").into());
    }
    if !candidate.is_file() {
        return Err(format!(
            "rmux daemon build succeeded but '{}' was not created",
            candidate.display()
        )
        .into());
    }

    Ok(candidate)
}

fn target_dir() -> TestResult<PathBuf> {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(target_dir));
    }

    let current = std::env::current_exe()?;
    current
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "test executable is not under a target directory".into())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("rmux-sdk manifest lives under crates/rmux-sdk")
        .to_path_buf()
}

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from("/tmp").join(format!(
            "rmux-sdk-pane-{}-{}-{unique_id}",
            compact_label(label),
            std::process::id()
        ));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn compact_label(label: &str) -> String {
    let compact = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if compact.is_empty() {
        "x".to_owned()
    } else {
        compact
    }
}
