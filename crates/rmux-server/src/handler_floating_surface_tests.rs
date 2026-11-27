//! Where a mouse report on a floating surface goes: the pane it selects and
//! raises, the runtime its bytes and the following keys reach, and the panes it
//! must not reach.
//!
//! tmux 3.7b finds the pane under the pointer front to back through the
//! z-index, a float with all its borders (window.c:623-676). A content press
//! runs `select-pane -t=; send -M` (key-bindings.c:446), and selecting a float
//! raises it (window.c:577-620). An unbound report is forwarded to the pane
//! under the pointer (server-client.c:1398-1423), and so is every report when
//! `mouse` is off, before any binding (server-client.c:1225-1226). It is encoded
//! relative to that pane, below a top status line, and only for a pane that
//! asked for mouse tracking (cmd.c:762-793, input-keys.c:713-815).
//! focus-follows-mouse selects the pane under a motion (server-client.c:
//! 997-1008), and a read-only client's mouse is dropped
//! (server-client.c:1204-1206). The tiled pane 0 fills the window; float 1
//! covers (10,5)-(39,12) and float 2, in front of it, (30,8)-(49,13).

use std::time::Duration;

use rmux_core::command_parser::CommandParser;
use rmux_proto::{PaneTarget, SessionName};
use tokio::sync::mpsc;

use super::RequestHandler;
use crate::client_flags::ClientFlags;

const PID: u32 = 7_501;
const FLOAT_1: &str = "-x30 -y8 -X10 -Y5";
const FLOAT_2: &str = "-x20 -y6 -X30 -Y8";
const MOUSE_SGR: &[u8] = b"\x1b[?1000h\x1b[?1006h";
const FOCUS_REPORTING: &[u8] = b"\x1b[?1004h";
/// A press and release at float 1's cell (2,2), as a pane asking for SGR reads it.
const PRESS_AT_FLOAT_1: &[u8] = b"\x1b[<0;3;3M\x1b[<0;3;3m";
/// What every pane prints once its command runs, before it reads its input.
const READY: &str = "fsurf-ready";
const READY_WAIT: Duration = Duration::from_secs(15);

/// One session with its floats, an attached client and its pending input.
struct Surface {
    handler: RequestHandler,
    session: SessionName,
    pending: Vec<u8>,
    /// Whether each pane asks for focus reports, as this fixture made it.
    focus_reporting: Vec<bool>,
}

/// The whole destination of a gesture: the active pane, the floats back to
/// front, and what each of panes 0, 1 and 2 received.
#[derive(Debug, PartialEq)]
struct Destination {
    active: u32,
    stack: Vec<u32>,
    received: [Vec<u8>; 3],
}

impl Surface {
    async fn new(label: &str, options: &[&str], floats: &[&str]) -> Self {
        let surface = Self {
            handler: RequestHandler::new(),
            session: SessionName::new(label).expect("valid session name"),
            pending: Vec::new(),
            focus_reporting: vec![false; floats.len() + 1],
        };
        let quiet = ready_then_quiet_command();
        surface
            .queue(&format!(
                "set-option -g default-command {}",
                crate::test_shell::command_quote(&quiet)
            ))
            .await;
        for option in options {
            surface.queue(option).await;
        }
        surface
            .queue(&format!("new-session -d -s {label} -x80 -y24"))
            .await;
        for geometry in floats {
            surface
                .queue(&format!("new-pane -d -t {label}:0.0 {geometry}"))
                .await;
        }
        // The fixture owns each pane's focus mode. A quiet child leaves it off
        // on a Unix PTY, but a ConPTY writes ESC[?1004h at startup, before its
        // child runs. A pane's output is applied in the order it was written,
        // so once the child's marker shows, the mode is set after that
        // startup output.
        for index in 0..=floats.len() as u32 {
            let target = surface.target(&surface.session, index);
            surface
                .handler
                .wait_for_pane_startup_to_finish_for_test(&target)
                .await;
            surface.wait_until_ready(&target).await;
            surface
                .handler
                .disable_pane_focus_reports_for_test(&target)
                .await;
        }
        surface
    }

    /// Waits until the pane at `target` shows `READY`: the transcript is read
    /// again after each output the pane publishes, never on a timer.
    async fn wait_until_ready(&self, target: &PaneTarget) {
        let mut output = self
            .handler
            .state
            .lock()
            .await
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("pane output exists")
            .subscribe();
        tokio::time::timeout(READY_WAIT, async {
            while !self.shows_ready(target).await {
                let _ = output.recv_observed().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("pane {target} never printed its ready marker"));
    }

    async fn shows_ready(&self, target: &PaneTarget) -> bool {
        let transcript = self
            .handler
            .state
            .lock()
            .await
            .transcript_handle(target)
            .expect("pane transcript exists");
        let screen = transcript
            .lock()
            .screen()
            .capture_transcript(Default::default(), Default::default());
        screen
            .windows(READY.len())
            .any(|window| window == READY.as_bytes())
    }

    async fn queue(&self, command: &str) {
        self.handler
            .execute_parsed_commands_for_test(
                std::process::id(),
                CommandParser::new().parse(command).expect("command parses"),
            )
            .await
            .unwrap_or_else(|error| panic!("{command} should execute: {error}"));
    }

    fn target(&self, session: &SessionName, index: u32) -> PaneTarget {
        PaneTarget::with_window(session.clone(), 0, index)
    }

    /// Makes pane `index` print `bytes`, as its application would.
    async fn print(&self, index: u32, bytes: &[u8]) {
        let transcript = self
            .handler
            .state
            .lock()
            .await
            .transcript_handle(&self.target(&self.session, index))
            .expect("pane transcript exists");
        let _ = transcript.lock().append_bytes_with_effects(bytes);
    }

    /// Makes pane `index` ask for focus reports, as its application would.
    async fn ask_for_focus_reports(&mut self, index: u32) {
        self.print(index, FOCUS_REPORTING).await;
        self.focus_reporting[index as usize] = true;
    }

    /// Whether each pane's terminal asks for focus reports right now.
    async fn focus_reporting_in_panes(&self) -> Vec<bool> {
        let state = self.handler.state.lock().await;
        (0..self.focus_reporting.len() as u32)
            .map(|index| {
                let mode = state
                    .transcript_handle(&self.target(&self.session, index))
                    .expect("pane transcript exists")
                    .lock()
                    .mode();
                mode & rmux_core::input::mode::MODE_FOCUSON != 0
            })
            .collect()
    }

    async fn attach(&self, session: &SessionName) {
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        self.handler
            .register_attach(PID, session.clone(), control_tx)
            .await;
        tokio::spawn(async move { while control_rx.recv().await.is_some() {} });
    }

    async fn set_read_only(&self) {
        let mut active_attach = self.handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&PID)
            .expect("the client is attached");
        active.flags.insert(ClientFlags::READONLY);
    }

    /// Starts capturing what each pane receives under every session name given.
    ///
    /// Focus reports reach a pane only while it asks for them (window.c:
    /// 498-533), so before a gesture every pane must ask exactly as this
    /// fixture made it; any other focus mode is not the gesture's doing.
    async fn capture(&self, sessions: &[&SessionName]) {
        assert_eq!(
            self.focus_reporting_in_panes().await,
            self.focus_reporting,
            "each pane's focus reporting before the gesture, against what the fixture set"
        );
        for session in sessions {
            for index in 0..3 {
                self.handler
                    .start_attached_input_capture_for_test(&self.target(session, index))
                    .await;
            }
        }
    }

    async fn report(&mut self, bytes: &[u8]) {
        self.handler
            .handle_attached_live_input(PID, &mut self.pending, bytes)
            .await
            .expect("the attached client accepts its input");
    }

    async fn click(&mut self, (x, y): (u16, u16)) {
        let press = format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1);
        self.report(press.as_bytes()).await;
    }

    async fn motion(&mut self, (x, y): (u16, u16)) {
        self.report(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes())
            .await;
    }

    async fn destination(&self, sessions: &[&SessionName]) -> Destination {
        let mut received: [Vec<u8>; 3] = Default::default();
        for session in sessions {
            for (index, bytes) in received.iter_mut().enumerate() {
                let target = self.target(session, index as u32);
                if let Some(captured) = self.handler.attached_input_capture_for_test(&target).await
                {
                    bytes.extend_from_slice(&captured);
                }
            }
        }
        let state = self.handler.state.lock().await;
        let window = state
            .sessions
            .session(&self.session)
            .and_then(|session| session.window_at(0))
            .expect("window 0 exists");
        let stack = window
            .floating_stack()
            .placements()
            .iter()
            .filter_map(|placement| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id() == placement.pane_id())
                    .map(|pane| pane.index())
            })
            .collect();
        Destination {
            active: window.active_pane_index(),
            stack,
            received,
        }
    }
}

#[cfg(unix)]
fn ready_then_quiet_command() -> String {
    format!(
        "printf {}; {}",
        READY,
        crate::test_shell::stdin_discard_command()
    )
}

#[cfg(windows)]
fn ready_then_quiet_command() -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[Console]::Out.Write('{}'); $inputStream=[Console]::OpenStandardInput(); $inputStream.CopyTo([System.IO.Stream]::Null)",
        READY
    ))
}

#[derive(Debug, Clone, Copy)]
enum Click {
    Writable,
    FocusEvents,
    ReadOnly,
    RearFloatInCopyMode,
}

#[tokio::test]
async fn a_click_on_a_rear_floats_visible_content_gives_it_focus_and_the_keys() {
    let rows = [
        Click::Writable,
        Click::FocusEvents,
        Click::ReadOnly,
        Click::RearFloatInCopyMode,
    ];
    let mut mismatches = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let mut options = vec!["set-option -g mouse on", "set-option -g status off"];
        if matches!(row, Click::FocusEvents) {
            options.push("set-option -s focus-events on");
        }
        let label = format!("fsurf-click-{index}");
        let mut surface = Surface::new(&label, &options, &[FLOAT_1, FLOAT_2]).await;
        let session = surface.session.clone();
        if matches!(row, Click::FocusEvents) {
            surface.ask_for_focus_reports(0).await;
            surface.ask_for_focus_reports(1).await;
        }
        if matches!(row, Click::RearFloatInCopyMode) {
            surface.queue(&format!("copy-mode -t {session}:0.1")).await;
        }
        surface.attach(&session).await;
        if matches!(row, Click::ReadOnly) {
            surface.set_read_only().await;
        }
        surface.capture(&[&session]).await;

        surface.click((15, 6)).await;
        surface.report(b"z").await;

        let want = match row {
            Click::Writable => Destination {
                active: 1,
                stack: vec![2, 1],
                received: [Vec::new(), b"z".to_vec(), Vec::new()],
            },
            Click::FocusEvents => Destination {
                active: 1,
                stack: vec![2, 1],
                received: [b"\x1b[O".to_vec(), b"\x1b[Iz".to_vec(), Vec::new()],
            },
            Click::ReadOnly => Destination {
                active: 0,
                stack: vec![1, 2],
                received: Default::default(),
            },
            Click::RearFloatInCopyMode => Destination {
                active: 1,
                stack: vec![2, 1],
                received: Default::default(),
            },
        };
        let got = surface.destination(&[&session]).await;
        if got != want {
            mismatches.push(format!("{row:?}: wanted {want:?}, got {got:?}"));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[derive(Debug, Clone, Copy)]
enum Masked {
    StatusOff,
    StatusTop,
    StatusBottom,
    GroupedSession,
}

#[tokio::test]
async fn a_press_where_a_float_masks_a_tiled_pane_reaches_only_the_float() {
    let rows = [
        Masked::StatusOff,
        Masked::StatusTop,
        Masked::StatusBottom,
        Masked::GroupedSession,
    ];
    let mut mismatches = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let status: &[&str] = match row {
            Masked::StatusTop => &[
                "set-option -g status on",
                "set-option -g status-position top",
            ],
            Masked::StatusBottom => &[
                "set-option -g status on",
                "set-option -g status-position bottom",
            ],
            Masked::StatusOff | Masked::GroupedSession => &["set-option -g status off"],
        };
        let mut options = vec!["set-option -g mouse on"];
        options.extend_from_slice(status);
        let label = format!("fsurf-masked-{index}");
        let mut surface = Surface::new(&label, &options, &[FLOAT_1]).await;
        let session = surface.session.clone();
        surface.print(0, MOUSE_SGR).await;
        surface.print(1, MOUSE_SGR).await;
        let view = SessionName::new(format!("{label}-view")).expect("valid session name");
        let client_session = if matches!(row, Masked::GroupedSession) {
            surface
                .queue(&format!("new-session -d -t {session} -s {view}"))
                .await;
            &view
        } else {
            &session
        };
        surface.attach(client_session).await;
        surface.capture(&[&session, &view]).await;

        let at = if matches!(row, Masked::StatusTop) {
            (12, 8)
        } else {
            (12, 7)
        };
        surface.click(at).await;

        let want = Destination {
            active: 1,
            stack: vec![1],
            received: [Vec::new(), PRESS_AT_FLOAT_1.to_vec(), Vec::new()],
        };
        let got = surface.destination(&[&session, &view]).await;
        if got != want {
            mismatches.push(format!("{row:?}: wanted {want:?}, got {got:?}"));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[tokio::test]
async fn with_mouse_off_a_report_over_a_masked_zone_goes_to_the_float_only() {
    let mut surface = Surface::new(
        "fsurf-mouse-off",
        &["set-option -g mouse off", "set-option -g status off"],
        &[FLOAT_1],
    )
    .await;
    let session = surface.session.clone();
    surface.print(0, MOUSE_SGR).await;
    surface.print(1, MOUSE_SGR).await;
    surface.attach(&session).await;
    surface.capture(&[&session]).await;

    surface.click((12, 7)).await;

    assert_eq!(
        surface.destination(&[&session]).await,
        Destination {
            active: 0,
            stack: vec![1],
            received: [Vec::new(), PRESS_AT_FLOAT_1.to_vec(), Vec::new()],
        }
    );
}

#[tokio::test]
async fn focus_follows_mouse_selects_and_raises_the_surface_under_the_pointer() {
    let mut surface = Surface::new(
        "fsurf-follow",
        &[
            "set-option -g mouse on",
            "set-option -g status off",
            "set-option -g focus-follows-mouse on",
        ],
        &[FLOAT_1, FLOAT_2],
    )
    .await;
    let session = surface.session.clone();
    surface.attach(&session).await;
    surface.capture(&[&session]).await;

    let mut steps = Vec::new();
    for at in [(35, 10), (15, 6), (5, 2)] {
        surface.motion(at).await;
        steps.push((at, surface.destination(&[&session]).await));
    }

    let quiet: [Vec<u8>; 3] = Default::default();
    assert_eq!(
        steps,
        vec![
            (
                (35, 10),
                Destination {
                    active: 2,
                    stack: vec![1, 2],
                    received: quiet.clone(),
                }
            ),
            (
                (15, 6),
                Destination {
                    active: 1,
                    stack: vec![2, 1],
                    received: quiet.clone(),
                }
            ),
            (
                (5, 2),
                Destination {
                    active: 0,
                    stack: vec![2, 1],
                    received: quiet,
                }
            ),
        ]
    );
}

#[tokio::test]
async fn floating_border_drag_moves_without_resizing_the_pane() {
    // Native tmux 3.7b: top moves; sides and corners resize locally.
    let gestures = [
        ((15, 4), (18, 6), (13, 7, 30, 8)),
        ((40, 8), (43, 10), (10, 5, 33, 8)),
        ((9, 8), (12, 10), (13, 5, 27, 8)),
        ((20, 13), (23, 15), (10, 5, 30, 10)),
        ((9, 4), (12, 6), (13, 7, 27, 6)),
        ((40, 4), (43, 6), (10, 7, 33, 6)),
        ((9, 13), (12, 15), (13, 5, 27, 10)),
        ((40, 13), (43, 15), (10, 5, 33, 10)),
    ];
    let mut failures = Vec::new();
    for (index, (start, end, expected)) in gestures.into_iter().enumerate() {
        for variant in 0..3 {
            let label = format!("fsurf-drag-{index}-{variant}");
            let mut surface = Surface::new(
                &label,
                &["set-option -g mouse on", "set-option -g status off"],
                &[FLOAT_1],
            )
            .await;
            let session = surface.session.clone();
            let linked = SessionName::new(format!("{label}-linked")).unwrap();
            let offset = u16::from(variant == 1);
            if variant == 1 {
                surface
                    .queue(&format!("new-session -d -t {session} -s {linked}"))
                    .await;
                surface
                    .queue(&format!("set-option -t {linked} status on"))
                    .await;
                surface
                    .queue(&format!("set-option -t {linked} status-position top"))
                    .await;
                let (tx, mut rx) = mpsc::unbounded_channel();
                surface
                    .handler
                    .register_attach(PID + 1, session.clone(), tx)
                    .await;
                tokio::spawn(async move { while rx.recv().await.is_some() {} });
            }
            surface
                .attach(if variant == 1 { &linked } else { &session })
                .await;
            if variant == 2 {
                surface.set_read_only().await;
            }
            let target = surface.target(&session, 1);
            let (calls, tile) = {
                let state = surface.handler.state.lock().await;
                (
                    state.pane_native_resize_calls_for_test(&target),
                    state
                        .sessions
                        .session(&session)
                        .unwrap()
                        .window_at(0)
                        .unwrap()
                        .pane(0)
                        .unwrap()
                        .geometry(),
                )
            };
            for (button, (x, y), suffix) in [(0, start, 'M'), (32, end, 'M'), (0, end, 'm')] {
                surface
                    .report(
                        format!("\x1b[<{button};{};{}{suffix}", x + 1, y + offset + 1).as_bytes(),
                    )
                    .await;
            }
            let state = surface.handler.state.lock().await;
            let window = state
                .sessions
                .session(&session)
                .unwrap()
                .window_at(0)
                .unwrap();
            let pane = window.pane(1).unwrap();
            let rect = window.floating_stack().placement(pane.id()).unwrap().rect();
            let got = (rect.x(), rect.y(), rect.cols(), rect.rows());
            let want = if variant == 2 {
                (10, 5, 30, 8)
            } else {
                expected
            };
            let resize_calls = state.pane_native_resize_calls_for_test(&target) - calls;
            let want_calls = u64::from(variant != 2 && index != 0);
            if got != want
                || resize_calls != want_calls
                || window.pane(0).unwrap().geometry() != tile
            {
                failures.push(format!("gesture {index} variant {variant}: rect={got:?} want={want:?}; PTY calls={resize_calls} want={want_calls}; tile={:?} want={tile:?}", window.pane(0).unwrap().geometry()));
            }
            if variant == 1 {
                let other = state
                    .sessions
                    .session(&linked)
                    .unwrap()
                    .window_at(0)
                    .unwrap();
                assert_eq!(other.id(), window.id());
                assert_eq!(other.floating_stack(), window.floating_stack());
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[tokio::test]
async fn floating_border_drag_uses_the_clients_panned_scene() {
    let mut surface = Surface::new(
        "fsurf-panned-drag",
        &["set-option -g mouse on", "set-option -g status off"],
        &["-x30 -y8 -X10 -Y10"],
    )
    .await;
    let session = surface.session.clone();
    surface
        .queue(&format!("set-option -t {session} window-size manual"))
        .await;
    surface
        .queue(&format!("resize-window -t {session}:0 -x80 -y40"))
        .await;
    surface.attach(&session).await;
    surface
        .handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get_mut(&PID)
        .unwrap()
        .set_declared_client_size(rmux_proto::TerminalSize { cols: 80, rows: 24 });
    surface
        .queue(&format!("refresh-client -t {PID} -D 3"))
        .await;
    assert_eq!(
        surface.handler.active_attach.lock().await.by_pid[&PID]
            .viewport
            .y,
        3
    );
    surface.report(b"\x1b[<0;16;7M").await;
    surface.report(b"\x1b[<32;19;9M").await;
    surface.report(b"\x1b[<32;21;10M").await;
    surface.report(b"\x1b[<0;21;10m").await;
    let state = surface.handler.state.lock().await;
    let window = state
        .sessions
        .session(&session)
        .unwrap()
        .window_at(0)
        .unwrap();
    let rect = window
        .floating_stack()
        .placement(window.pane(1).unwrap().id())
        .unwrap()
        .rect();
    assert_eq!(
        (rect.x(), rect.y(), rect.cols(), rect.rows()),
        (15, 13, 30, 8)
    );
}

#[tokio::test]
async fn floating_border_drag_raises_and_focuses_its_input_destination() {
    let mut surface = Surface::new(
        "fsurf-drag-focus",
        &[
            "set-option -g mouse on",
            "set-option -g status off",
            "set-option -s focus-events on",
        ],
        &[FLOAT_1, FLOAT_2],
    )
    .await;
    let session = surface.session.clone();
    surface.ask_for_focus_reports(0).await;
    surface.ask_for_focus_reports(1).await;
    surface.attach(&session).await;
    surface.capture(&[&session]).await;
    surface.report(b"\x1b[<0;16;5M").await;
    surface.report(b"\x1b[<32;19;7M").await;
    surface.report(b"\x1b[<0;19;7m").await;
    surface.report(b"K").await;
    assert_eq!(
        surface.destination(&[&session]).await,
        Destination {
            active: 1,
            stack: vec![2, 1],
            received: [b"\x1b[O".to_vec(), b"\x1b[IK".to_vec(), Vec::new()],
        }
    );
}
