#![cfg(windows)]

use std::error::Error;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_pty::{write_windows_console_mouse_drag, ChildCommand, SpawnedPty, TerminalSize};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

const ATTACH_READY_DELAY: Duration = Duration::from_millis(700);
const MOUSE_SETTLE_DELAY: Duration = Duration::from_millis(900);
const MOUSE_GEOMETRY_CHANGE_TIMEOUT: Duration = Duration::from_secs(6);
const ATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const RMUX_MOUSE_BORDER_RMUX_BIN_ENV: &str = "RMUX_MOUSE_BORDER_RMUX_BIN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneGeometry {
    index: u16,
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

#[test]
fn mouse_drag_on_vertical_border_resizes_horizontal_split_through_attach_binding(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-horizontal")?;
    let label = unique_label("mouse-border-resize-h")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70h";

    create_split_session(&label, session, "-h")?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let right = pane_by_index(&before, 1)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    let after = drag_until_geometry_changes(
        &mut attach,
        &label,
        session,
        &before,
        MouseDrag::new(border_x, y, border_x.saturating_add(5), y),
    )?;
    let resized_left = pane_by_index(&after, 0)?;
    let resized_right = pane_by_index(&after, 1)?;

    assert!(
        resized_left.width > left.width,
        "left pane should grow after dragging the vertical border right; before={before:?} after={after:?}"
    );
    assert!(
        resized_right.left > right.left,
        "right pane should move right after border drag; before={before:?} after={after:?}"
    );

    Ok(())
}

#[test]
fn mouse_drag_on_vertical_border_shrinks_horizontal_split_through_attach_binding(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-horizontal-shrink")?;
    let label = unique_label("mouse-border-resize-h-shrink")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70hs";

    create_split_session(&label, session, "-h")?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let right = pane_by_index(&before, 1)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    let after = drag_until_geometry_changes(
        &mut attach,
        &label,
        session,
        &before,
        MouseDrag::new(border_x, y, border_x.saturating_sub(5), y),
    )?;
    let resized_left = pane_by_index(&after, 0)?;
    let resized_right = pane_by_index(&after, 1)?;

    assert!(
        resized_left.width < left.width,
        "left pane should shrink after dragging the vertical border left; before={before:?} after={after:?}"
    );
    assert!(
        resized_right.left < right.left,
        "right pane should move left after border drag; before={before:?} after={after:?}"
    );

    Ok(())
}

#[test]
fn mouse_drag_on_horizontal_border_resizes_vertical_split_through_attach_binding(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-vertical")?;
    let label = unique_label("mouse-border-resize-v")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70v";

    create_split_session(&label, session, "-v")?;
    let before = pane_geometries(&label, session)?;
    let top = pane_by_index(&before, 0)?;
    let bottom = pane_by_index(&before, 1)?;
    let x = top.left.saturating_add(1);
    let border_y = top.top.saturating_add(top.height);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    let after = drag_until_geometry_changes(
        &mut attach,
        &label,
        session,
        &before,
        MouseDrag::new(x, border_y, x, border_y.saturating_add(3)),
    )?;
    let resized_top = pane_by_index(&after, 0)?;
    let resized_bottom = pane_by_index(&after, 1)?;

    assert!(
        resized_top.height > top.height,
        "top pane should grow after dragging the horizontal border down; before={before:?} after={after:?}"
    );
    assert!(
        resized_bottom.top > bottom.top,
        "bottom pane should move down after border drag; before={before:?} after={after:?}"
    );

    Ok(())
}

#[test]
fn mouse_drag_on_horizontal_border_shrinks_vertical_split_through_attach_binding(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-vertical-shrink")?;
    let label = unique_label("mouse-border-resize-v-shrink")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70vs";

    create_split_session(&label, session, "-v")?;
    let before = pane_geometries(&label, session)?;
    let top = pane_by_index(&before, 0)?;
    let bottom = pane_by_index(&before, 1)?;
    let x = top.left.saturating_add(1);
    let border_y = top.top.saturating_add(top.height);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    let after = drag_until_geometry_changes(
        &mut attach,
        &label,
        session,
        &before,
        MouseDrag::new(x, border_y, x, border_y.saturating_sub(3)),
    )?;
    let resized_top = pane_by_index(&after, 0)?;
    let resized_bottom = pane_by_index(&after, 1)?;

    assert!(
        resized_top.height < top.height,
        "top pane should shrink after dragging the horizontal border up; before={before:?} after={after:?}"
    );
    assert!(
        resized_bottom.top < bottom.top,
        "bottom pane should move up after border drag; before={before:?} after={after:?}"
    );

    Ok(())
}

#[test]
fn mouse_drag_border_does_not_resize_when_mouse_option_is_off() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-mouse-off")?;
    let label = unique_label("mouse-border-resize-off")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70off";

    create_split_session_with_mouse(&label, session, "-h", false)?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    attach.write_mouse_drag(border_x, y, border_x.saturating_add(5), y)?;
    thread::sleep(MOUSE_SETTLE_DELAY);

    let after = pane_geometries(&label, session)?;
    assert_eq!(
        before, after,
        "mouse-off border drag must not mutate pane geometry"
    );

    Ok(())
}

#[test]
fn mouse_drag_border_does_not_resize_when_binding_is_unbound() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-unbound")?;
    let label = unique_label("mouse-border-resize-unbound")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70unbound";

    create_split_session(&label, session, "-h")?;
    assert_success(
        rmux_command(&label)
            .args(["unbind-key", "-T", "root", "MouseDrag1Border"])
            .stdin(Stdio::null())
            .output()?,
        "unbind MouseDrag1Border",
    )?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    attach.write_mouse_drag(border_x, y, border_x.saturating_add(5), y)?;
    thread::sleep(MOUSE_SETTLE_DELAY);

    let after = pane_geometries(&label, session)?;
    assert_eq!(
        before, after,
        "unbound MouseDrag1Border must not mutate pane geometry"
    );

    Ok(())
}

#[test]
fn mouse_drag_border_read_only_attach_does_not_resize() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-read-only")?;
    let label = unique_label("mouse-border-resize-readonly")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70readonly";

    create_split_session(&label, session, "-h")?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, true)?;
    attach.write_mouse_drag(border_x, y, border_x.saturating_add(5), y)?;
    thread::sleep(MOUSE_SETTLE_DELAY);

    let after = pane_geometries(&label, session)?;
    assert_eq!(
        before, after,
        "read-only attach border drag must not mutate pane geometry"
    );

    Ok(())
}

#[test]
fn mouse_drag_border_binding_pipeline_preserves_mouse_event() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-binding-pipeline")?;
    let label = unique_label("mouse-border-resize-pipeline")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70pipeline";

    create_split_session(&label, session, "-h")?;
    assert_success(
        rmux_command(&label)
            .args([
                "bind-key",
                "-n",
                "MouseDrag1Border",
                "display-message dragged ; resize-pane -M",
            ])
            .stdin(Stdio::null())
            .output()?,
        "bind MouseDrag1Border pipeline",
    )?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    let after = drag_until_geometry_changes(
        &mut attach,
        &label,
        session,
        &before,
        MouseDrag::new(border_x, y, border_x.saturating_add(5), y),
    )?;
    assert_ne!(
        before, after,
        "MouseDrag1Border must preserve mouse_event through a command pipeline before resize-pane -M"
    );

    Ok(())
}

#[test]
fn mouse_drag_border_resizes_inside_three_pane_layout() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-three-pane")?;
    let label = unique_label("mouse-border-resize-three")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70three";

    create_split_session(&label, session, "-h")?;
    assert_success(
        rmux_command(&label)
            .args(["split-window", "-v", "-t", &format!("{session}:0.0")])
            .stdin(Stdio::null())
            .output()?,
        "second split-window",
    )?;
    thread::sleep(Duration::from_millis(400));
    let before = pane_geometries(&label, session)?;
    assert_eq!(before.len(), 3, "test setup should create three panes");
    let top_left = pane_by_index(&before, 0)?;
    let border_x = top_left.left.saturating_add(top_left.width);
    let y = top_left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    let after = drag_until_geometry_changes(
        &mut attach,
        &label,
        session,
        &before,
        MouseDrag::new(border_x, y, border_x.saturating_add(4), y),
    )?;
    assert_eq!(after.len(), 3, "border drag must not create or drop panes");
    assert_ne!(
        before, after,
        "border drag in a three-pane layout should mutate geometry; before={before:?} after={after:?}"
    );

    Ok(())
}

#[test]
fn malformed_mouse_sgr_does_not_resize_or_crash_attach() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("mouse-border-resize-malformed")?;
    let label = unique_label("mouse-border-resize-malformed")?;
    let _server = ServerGuard::new(label.clone());
    let session = "i70malformed";

    create_split_session(&label, session, "-h")?;
    let before = pane_geometries(&label, session)?;
    let left = pane_by_index(&before, 0)?;
    let border_x = left.left.saturating_add(left.width);
    let y = left.top.saturating_add(1);

    let mut attach = AttachGuard::spawn(&label, session, false)?;
    attach.write_bytes(b"\x1b[<0;not-a-number;1M\x1b[<32;999999;999999M\x1b[<0;")?;
    attach.write_sgr_drag(border_x.saturating_add(1), y, border_x.saturating_add(4), y)?;
    thread::sleep(MOUSE_SETTLE_DELAY);

    let after = pane_geometries(&label, session)?;
    assert_eq!(
        before, after,
        "malformed SGR and off-border drag must not mutate pane geometry"
    );

    Ok(())
}

fn create_split_session(
    label: &str,
    session: &str,
    split_flag: &str,
) -> Result<(), Box<dyn Error>> {
    create_split_session_with_mouse(label, session, split_flag, true)
}

fn create_split_session_with_mouse(
    label: &str,
    session: &str,
    split_flag: &str,
    mouse_enabled: bool,
) -> Result<(), Box<dyn Error>> {
    let cmd = cmd_exe();
    let mut new_session = rmux_command(label);
    assert_success(
        new_session
            .args(["new-session", "-d", "-s", session, "-x", "80", "-y", "24"])
            .arg(cmd)
            .args(["/d", "/q"])
            .stdin(Stdio::null())
            .output()?,
        "new-session",
    )?;
    assert_success(
        rmux_command(label)
            .args([
                "set-option",
                "-g",
                "mouse",
                if mouse_enabled { "on" } else { "off" },
            ])
            .stdin(Stdio::null())
            .output()?,
        "set mouse",
    )?;
    assert_success(
        rmux_command(label)
            .args(["set-option", "-g", "status", "off"])
            .stdin(Stdio::null())
            .output()?,
        "set status off",
    )?;
    let target = format!("{session}:0.0");
    assert_success(
        rmux_command(label)
            .args(["split-window", split_flag, "-t", &target])
            .stdin(Stdio::null())
            .output()?,
        "split-window",
    )?;
    thread::sleep(Duration::from_millis(400));
    Ok(())
}

fn pane_geometries(label: &str, session: &str) -> Result<Vec<PaneGeometry>, Box<dyn Error>> {
    let output = rmux_command(label)
        .args([
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_index}:#{pane_left}:#{pane_top}:#{pane_width}:#{pane_height}",
        ])
        .stdin(Stdio::null())
        .output()?;
    assert_success(output, "list-panes").and_then(parse_pane_geometries)
}

fn parse_pane_geometries(output: Output) -> Result<Vec<PaneGeometry>, Box<dyn Error>> {
    let stdout = String::from_utf8(output.stdout)?;
    stdout
        .lines()
        .map(|line| {
            let parts = line.split(':').collect::<Vec<_>>();
            if parts.len() != 5 {
                return Err(format!("malformed pane geometry line: {line:?}").into());
            }
            Ok(PaneGeometry {
                index: parts[0].parse()?,
                left: parts[1].parse()?,
                top: parts[2].parse()?,
                width: parts[3].parse()?,
                height: parts[4].parse()?,
            })
        })
        .collect()
}

fn pane_by_index(panes: &[PaneGeometry], index: u16) -> Result<PaneGeometry, Box<dyn Error>> {
    panes
        .iter()
        .copied()
        .find(|pane| pane.index == index)
        .ok_or_else(|| format!("missing pane index {index}; panes={panes:?}").into())
}

#[derive(Clone, Copy, Debug)]
struct MouseDrag {
    start_x: u16,
    start_y: u16,
    end_x: u16,
    end_y: u16,
}

impl MouseDrag {
    fn new(start_x: u16, start_y: u16, end_x: u16, end_y: u16) -> Self {
        Self {
            start_x,
            start_y,
            end_x,
            end_y,
        }
    }
}

fn drag_until_geometry_changes(
    attach: &mut AttachGuard,
    label: &str,
    session: &str,
    before: &[PaneGeometry],
    drag: MouseDrag,
) -> Result<Vec<PaneGeometry>, Box<dyn Error>> {
    let deadline = Instant::now() + MOUSE_GEOMETRY_CHANGE_TIMEOUT;
    let mut after = before.to_vec();

    while Instant::now() < deadline {
        attach.write_mouse_drag(drag.start_x, drag.start_y, drag.end_x, drag.end_y)?;
        thread::sleep(MOUSE_SETTLE_DELAY);
        after = pane_geometries(label, session)?;
        if after != before {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    Ok(after)
}

fn wait_for_attach_client(
    label: &str,
    session: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = rmux_command(label)
            .args(["list-clients", "-t", session, "-F", "#{client_session}"])
            .stdin(Stdio::null())
            .output()?;
        if output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == session)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "attach client for session {session:?} did not appear before timeout; stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

struct AttachGuard {
    label: String,
    child: SpawnedPty,
}

impl AttachGuard {
    fn spawn(label: &str, session: &str, read_only: bool) -> Result<Self, Box<dyn Error>> {
        let mut args = vec![
            "-L".to_owned(),
            label.to_owned(),
            "attach-session".to_owned(),
        ];
        if read_only {
            args.push("-r".to_owned());
        }
        args.extend(["-t".to_owned(), session.to_owned()]);
        let child = ChildCommand::new(rmux_binary())
            .args(args)
            .size(TerminalSize::new(80, 24))
            .spawn()?;
        wait_for_attach_client(label, session, ATTACH_READY_DELAY)?;
        Ok(Self {
            label: label.to_owned(),
            child,
        })
    }

    fn write_sgr_drag(
        &mut self,
        start_x: u16,
        start_y: u16,
        end_x: u16,
        end_y: u16,
    ) -> Result<(), Box<dyn Error>> {
        let payload = format!(
            "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<0;{};{}m",
            start_x.saturating_add(1),
            start_y.saturating_add(1),
            end_x.saturating_add(1),
            end_y.saturating_add(1),
            end_x.saturating_add(1),
            end_y.saturating_add(1),
        );
        self.write_bytes(payload.as_bytes())
    }

    fn write_mouse_drag(
        &mut self,
        start_x: u16,
        start_y: u16,
        end_x: u16,
        end_y: u16,
    ) -> Result<(), Box<dyn Error>> {
        write_windows_console_mouse_drag(
            self.child.child().pid(),
            i16::try_from(start_x)?,
            i16::try_from(start_y)?,
            i16::try_from(end_x)?,
            i16::try_from(end_y)?,
        )?;
        Ok(())
    }

    fn write_bytes(&mut self, payload: &[u8]) -> Result<(), Box<dyn Error>> {
        self.child.master().write_all(payload)?;
        Ok(())
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        if wait_for_child_exit(&mut self.child, ATTACH_EXIT_TIMEOUT) {
            return;
        }
        let _ = rmux_command(&self.label)
            .arg("detach-client")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if wait_for_child_exit(&mut self.child, ATTACH_EXIT_TIMEOUT) {
            return;
        }
        let _ = self.child.child_mut().terminate_forcefully();
        let _ = self.child.child_mut().wait();
    }
}

struct ServerGuard {
    label: String,
}

impl ServerGuard {
    fn new(label: String) -> Self {
        Self { label }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = rmux_command(&self.label)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn rmux_command(label: &str) -> Command {
    let mut command = Command::new(rmux_binary());
    command.args(["-L", label]);
    command
}

fn rmux_binary() -> std::path::PathBuf {
    std::env::var_os(RMUX_MOUSE_BORDER_RMUX_BIN_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_rmux").into())
}

fn assert_success(output: Output, context: &str) -> Result<Output, Box<dyn Error>> {
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "{context} failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn wait_for_child_exit(child: &mut SpawnedPty, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.child_mut().try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return true,
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn unique_label(prefix: &str) -> Result<String, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn cmd_exe() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
}
