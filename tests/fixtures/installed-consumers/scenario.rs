//! Frozen handwritten external witness. Never regenerated from an API/schema.
use rmux_sdk::{
    EnsureSession, Pane, PaneOutputChunk, PaneStateEvent, PaneStateEventsOptions, PaneSurfaceState,
    Rmux, SessionName, TerminalSizeSpec,
};
use std::{error::Error, time::Duration};

pub type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
pub const MARKER: &str = "INSTALLED_CONSUMER_MARKER";

async fn observe(pane: &Pane) -> Result {
    let id = pane.id().await?.ok_or("missing stable pane id")?;
    let mut states = pane.state_events(PaneStateEventsOptions::default()).await?;
    match states.next().await? {
        Some(PaneStateEvent::Snapshot { pane_id, .. }) if pane_id == id => {}
        value => return Err(format!("wrong pane state identity: {value:?}").into()),
    }
    println!("state snapshot identity={id:?}");
    drop(states);

    let mut output = pane.output_stream().await?;
    let mut surface = pane.surface_stream().await?;
    let mut render = pane.render_stream().await?;
    // The private child disabled terminal echo before READY: only its output
    // can satisfy this witness, not an echoed printf command or input string.
    pane.send_text(format!("{MARKER}\n")).await?;
    let mut bytes = Vec::new();
    while !bytes
        .windows(MARKER.len())
        .any(|part| part == MARKER.as_bytes())
    {
        match output.next().await? {
            Some(PaneOutputChunk::Bytes { bytes: chunk, .. }) => bytes.extend(chunk),
            value => return Err(format!("unexpected output event: {value:?}").into()),
        }
        if bytes.len() > 65536 {
            return Err("output witness exceeded its byte limit".into());
        }
    }
    println!("owned output bytes={}", bytes.len());
    let mut state = PaneSurfaceState::default();
    loop {
        state.apply(&surface.next().await?.ok_or("surface closed")?)?;
        if state
            .frame()
            .is_some_and(|frame| frame.snapshot.grid.visible_text().contains(MARKER))
        {
            break;
        }
    }
    println!("owned surface frame observed");
    loop {
        let snapshot = render.next().await?.ok_or("render closed")?.into_snapshot();
        if snapshot.visible_text().contains(MARKER) {
            println!("owned render snapshot revision={}", snapshot.revision);
            break;
        }
    }
    drop((output, surface, render));
    crate::view(pane).await
}

pub async fn run() -> Result {
    let socket = std::env::args_os()
        .nth(1)
        .ok_or("explicit private socket required")?;
    let rmux = Rmux::builder()
        .unix_socket(socket)
        .default_timeout(Duration::from_secs(5))
        .connect()
        .await?;
    let session = rmux.ensure_session(
        EnsureSession::named(SessionName::new("installed_consumer")?).create_only()
            .detached(true).size(TerminalSizeSpec::new(80, 12))
            .argv(["/usr/bin/python3", "-u", "-c", "import sys,termios,collections; a=termios.tcgetattr(0); a[3] &= ~termios.ECHO; termios.tcsetattr(0,termios.TCSANOW,a); print('READY'); collections.deque(map(lambda line: print(line,end='',flush=True),sys.stdin),maxlen=0)"]),
    ).await?;
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        if std::env::args_os().nth(2).is_some() {
            record_panes()?;
        }
        let slot = session.pane(0, 0);
        slot.wait_for_text("READY").await?;
        let id = slot.id().await?.ok_or("missing initial pane id")?;
        let pane = rmux.pane_by_id(session.name().clone(), id).await?;
        assert_eq!(pane.id().await?, Some(id));
        println!("resolved stable pane identity={id:?}");
        observe(&pane).await?;
        if std::env::args_os().nth(2).is_some() {
            projection(&rmux, &session).await?;
        }
        Ok::<(), Box<dyn Error>>(())
    })
    .await;
    // Execute cleanup even when the observation returned an error or timed out.
    let killed = session.kill().await;
    let shutdown = rmux.shutdown().await;
    println!("cleanup session={killed:?} daemon={shutdown:?}");
    result??;
    if !killed? {
        return Err("owned session was not removed".into());
    }
    shutdown?;
    Ok(())
}

fn cli(args: &[&str]) -> Result {
    cli_expect(args, None)?;
    if matches!(args.first(), Some(&"new-pane") | Some(&"new-window")) {
        record_panes()?;
    }
    Ok(())
}

fn cli_expect(args: &[&str], rejection: Option<&str>) -> Result {
    cli_output(args, rejection).map(|_| ())
}

fn record_panes() -> Result {
    let output = cli_output(
        &[
            "list-panes",
            "-s",
            "-t",
            "installed_consumer",
            "-F",
            "#{pane_pid}",
        ],
        None,
    )?;
    for line in output.lines() {
        let pid: u32 = line.parse()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let birth = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap();
        println!("owned pane pid={pid} birth={birth}");
    }
    Ok(())
}

fn cli_output(args: &[&str], rejection: Option<&str>) -> Result<String> {
    let binary = std::env::args_os().nth(2).ok_or("private CLI required")?;
    let socket = std::env::args_os()
        .nth(1)
        .ok_or("private socket required")?;
    let child = std::process::Command::new(binary)
        .arg("-S")
        .arg(socket)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let birth = std::fs::read_to_string(format!("/proc/{pid}/stat"));
    let output = child.wait_with_output()?;
    let birth = birth?;
    let started = birth
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap();
    println!(
        "owned CLI pid={pid} birth={started} waited={}",
        output.status
    );
    println!(
        "cli {args:?} exit={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(message) = rejection {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
    } else if !output.status.success() {
        return Err("ordinary CLI operation failed".into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

async fn projection(rmux: &Rmux, session: &rmux_sdk::Session) -> Result {
    let window = session.window(0);
    let baseline = window.scene().await?;
    assert_eq!(baseline.panes.len(), 1);
    assert!(!baseline.panes[0].floating);
    let mut state = rmux_sdk::SceneState::default();
    assert!(state.apply(baseline.clone()).reset);
    let quiet = "printf 'FLOAT_READY\n'; while IFS= read -r line; do :; done";
    cli(&[
        "new-pane",
        "-d",
        "-t",
        "installed_consumer:0.0",
        "-x12",
        "-y4",
        "-X-3",
        "-Y1",
        quiet,
    ])?;
    cli(&[
        "new-pane",
        "-d",
        "-t",
        "installed_consumer:0.0",
        "-x12",
        "-y4",
        "-X4",
        "-Y2",
        quiet,
    ])?;
    session.pane(0, 1).wait_for_text("FLOAT_READY").await?;
    session.pane(0, 2).wait_for_text("FLOAT_READY").await?;
    cli(&["select-pane", "-t", "installed_consumer:0.2"])?;
    let scene = window.scene_for(&baseline).await?;
    assert_eq!(scene.panes.len(), 3);
    assert_eq!(scene.panes.iter().filter(|p| p.floating).count(), 2);
    assert_eq!(state.apply(scene.clone()).created.len(), 2);
    assert_eq!(scene, window.scene_for(&scene).await?);
    assert_ne!(scene.revision, baseline.revision);
    assert!(scene
        .panes
        .iter()
        .any(|p| p.content.x == -3 && p.content_clip.x == 0));
    let focused = scene
        .panes
        .iter()
        .find(|p| p.focused)
        .ok_or("missing focus")?;
    assert_eq!(Some(focused.id), scene.active_pane_id);
    capture_scene(rmux, session, &scene, "created").await?;
    let float_id = scene.panes.iter().find(|p| p.index == 1).unwrap().id;
    cli_expect(
        &[
            "resize-pane",
            "-t",
            "installed_consumer:0.1",
            "-x10",
            "-y3",
            "-X2",
            "-Y1",
        ],
        Some("resize-pane -X/-Y cannot be combined with -x"),
    )?;
    assert_eq!(window.scene_for(&scene).await?, scene);
    println!("negative mixed size/position rejected without scene or revision mutation");
    cli(&["resize-pane", "-t", "installed_consumer:0.1", "-x10", "-y3"])?;
    let resized = window.scene_for(&scene).await?;
    let rect = &resized
        .panes
        .iter()
        .find(|p| p.id == float_id)
        .unwrap()
        .content;
    assert_eq!((rect.x, rect.y, rect.cols, rect.rows), (-3, 1, 10, 3));
    let sibling = resized.panes.iter().find(|p| p.index == 2).unwrap();
    assert_eq!((sibling.content.cols, sibling.content.rows), (12, 5));
    assert_ne!(resized.revision, scene.revision);
    assert!(state.apply(resized.clone()).changed.contains(&float_id));
    capture_scene(rmux, session, &resized, "resized").await?;
    cli(&["resize-pane", "-t", "installed_consumer:0.1", "-X2", "-Y1"])?;
    let moved = window.scene_for(&resized).await?;
    let rect = &moved
        .panes
        .iter()
        .find(|p| p.id == float_id)
        .unwrap()
        .content;
    assert_eq!((rect.x, rect.y, rect.cols, rect.rows), (2, 1, 10, 3));
    assert_ne!(moved.revision, resized.revision);
    assert!(state.apply(moved.clone()).changed.contains(&float_id));
    capture_scene(rmux, session, &moved, "moved").await?;
    cli(&[
        "link-window",
        "-s",
        "installed_consumer:0",
        "-t",
        "installed_consumer:7",
    ])?;
    let linked = session.window(7).scene_for(&moved).await?;
    assert_eq!(linked.window_id, moved.window_id);
    assert_eq!(linked.panes, moved.panes);
    assert_eq!(linked.window_index, 7);
    assert!(state.apply(linked.clone()).selection_changed);
    capture_scene(rmux, session, &linked, "linked").await?;
    cli(&["unlink-window", "-t", "installed_consumer:7"])?;
    assert!(
        state
            .apply(window.scene_for(&moved).await?)
            .selection_changed
    );
    cli(&["kill-pane", "-t", "installed_consumer:0.1"])?;
    let removed = window.scene_for(&moved).await?;
    assert_eq!(state.apply(removed.clone()).removed, vec![float_id]);
    assert!(!removed.panes.iter().any(|p| p.id == float_id));
    capture_scene(rmux, session, &removed, "removed").await?;
    let reset = state.resynchronize(removed.clone());
    assert!(reset.reset && reset.changed.is_empty() && reset.removed.is_empty());
    assert_eq!(
        reset.created,
        removed.panes.iter().map(|p| p.id).collect::<Vec<_>>()
    );
    println!("polled full resynchronization after an explicit gap; no unseen mutations fabricated");
    // Reconnect uses a full, identity-guarded read; no mutation is retried.
    let socket = std::env::args_os().nth(1).unwrap();
    let reconnected = Rmux::builder().unix_socket(socket).connect().await?;
    assert_eq!(
        reconnected
            .session(session.name().clone())
            .await?
            .window(0)
            .scene_for(&removed)
            .await?,
        removed
    );
    println!("normal SDK reconnect returned the complete identity-guarded snapshot");
    let surviving = removed.panes.iter().find(|p| p.id == focused.id).unwrap();
    println!(
        "surviving pane stable_id={:?} current_slot={}",
        surviving.id, surviving.index
    );
    cli(&["kill-pane", "-t", &format!("%{}", surviving.id.as_u32())])?;
    cli(&["new-window", "-d", "-t", "installed_consumer:1", quiet])?;
    let previous = session.window(1).scene().await?;
    cli(&["kill-window", "-t", "installed_consumer:1"])?;
    cli(&["new-window", "-d", "-t", "installed_consumer:1", quiet])?;
    let replacement = session.window(1).scene().await?;
    assert_ne!(replacement.window_id, previous.window_id);
    let rejected = session.window(1).scene_for(&previous).await.unwrap_err();
    assert!(rejected
        .to_string()
        .contains("scene selection changed identity"));
    assert_eq!(session.window(1).scene().await?, replacement);
    println!(
        "recycled WindowId {:?}->{:?}: guarded read rejected; replacement unchanged",
        previous.window_id, replacement.window_id
    );
    println!("projection PASS zero/float/signed/occlusion/focus/move/resize/link/removal/reconnect/recycled-window revision={}", removed.revision);
    Ok(())
}

async fn capture_scene(
    rmux: &Rmux,
    session: &rmux_sdk::Session,
    scene: &rmux_sdk::SceneSnapshot,
    stage: &str,
) -> Result {
    let mut snapshots = Vec::new();
    let mut cells = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    for p in &scene.panes {
        assert!(ids.insert(p.id.as_u32()));
        for span in &p.spans {
            assert!(
                span.y < scene.rows
                    && u32::from(span.x) + u32::from(span.cols) <= u32::from(scene.cols)
            );
            for x in span.x..span.x + span.cols {
                assert!(cells.insert((x, span.y)), "overlapping visible spans");
            }
        }
        let pane = rmux.pane_by_id(session.name().clone(), p.id).await?;
        snapshots.push((p.id, pane.snapshot().await?));
    }
    for b in &scene.borders {
        assert!(cells.insert((b.x, b.y)), "frame overlaps visible pane");
    }
    crate::view_scene(scene, &snapshots)?;
    println!(
        "scene witness {stage}: window={:?} panes={} revision={}",
        scene.window_id,
        scene.panes.len(),
        scene.revision
    );
    Ok(())
}
