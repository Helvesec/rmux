use super::*;
use crate::input_keys::MouseForwardEvent;
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use rmux_core::{input::InputParser, PaneId, Screen};
use rmux_proto::RmuxError;
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
enum PromptCase {
    CommandForeground,
    CommandBackground,
    CommandIncremental,
    ConfirmForeground,
    ConfirmBackground,
}

impl PromptCase {
    const ALL: [Self; 5] = [
        Self::CommandForeground,
        Self::CommandBackground,
        Self::CommandIncremental,
        Self::ConfirmForeground,
        Self::ConfirmBackground,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::CommandForeground => "command-foreground",
            Self::CommandBackground => "command-background",
            Self::CommandIncremental => "command-incremental",
            Self::ConfirmForeground => "confirm-foreground",
            Self::ConfirmBackground => "confirm-background",
        }
    }

    fn is_detached(self) -> bool {
        matches!(
            self,
            Self::CommandBackground | Self::CommandIncremental | Self::ConfirmBackground
        )
    }

    fn command(self, action: &str) -> String {
        match self {
            Self::CommandForeground => format!("command-prompt -1 -pvalue {{ {action} }}"),
            Self::CommandBackground => format!("command-prompt -b -1 -pvalue {{ {action} }}"),
            Self::CommandIncremental => format!("command-prompt -i -pvalue {{ {action} }}"),
            Self::ConfirmForeground => format!("confirm-before -pconfirm {{ {action} }}"),
            Self::ConfirmBackground => format!("confirm-before -b -pconfirm {{ {action} }}"),
        }
    }

    fn response(self) -> &'static [u8] {
        match self {
            Self::CommandForeground | Self::CommandBackground => b"x",
            Self::CommandIncremental => b"\x1b",
            Self::ConfirmForeground | Self::ConfirmBackground => b"y",
        }
    }
}

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn fixture(name: &str) -> (RequestHandler, SessionName, PaneTarget) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 6 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    (handler, session, target)
}

fn mouse_event(target: &PaneTarget) -> AttachedMouseEvent {
    AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 0,
            lb: 0,
            x: 1,
            y: 1,
            lx: 1,
            ly: 1,
            sgr_b: 0,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 1,
        window_id: Some(1),
        pane_id: Some(PaneId::new(0)),
        pane_target: Some(target.clone()),
        location: MouseLocation::Pane,
        status_at: None,
        status_lines: 0,
        ignore: false,
    }
}

async fn register_attach(
    handler: &RequestHandler,
    session: &SessionName,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), session.clone(), control_tx)
        .await;
    control_rx
}

async fn start_prompt(
    handler: &RequestHandler,
    case: PromptCase,
    command: &str,
    context: QueueExecutionContext,
) -> Option<tokio::task::JoinHandle<Result<(), RmuxError>>> {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("command {command:?} parses: {error}"));
    let execution_handler = handler.clone();
    let task = tokio::spawn(async move {
        execution_handler
            .execute_parsed_commands(std::process::id(), parsed, context)
            .await
            .map(|_| ())
    });

    tokio::time::timeout(background_shell_test_timeout(), async {
        loop {
            if handler
                .attached_prompt_render(std::process::id())
                .await
                .is_some()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("prompt becomes active");

    if case.is_detached() {
        task.await
            .expect("background prompt task joins")
            .expect("background prompt starts");
        None
    } else {
        Some(task)
    }
}

async fn finish_prompt(
    handler: &RequestHandler,
    case: PromptCase,
    foreground: Option<tokio::task::JoinHandle<Result<(), RmuxError>>>,
) {
    handler
        .handle_attached_live_input_for_test(std::process::id(), case.response())
        .await
        .unwrap_or_else(|error| panic!("{} input succeeds: {error}", case.label()));
    if let Some(task) = foreground {
        task.await
            .expect("foreground prompt task joins")
            .unwrap_or_else(|error| panic!("{} executes: {error}", case.label()));
    } else if !matches!(case, PromptCase::CommandIncremental) {
        wait_for_prompt_task(handler, "rmux-prompt-finish").await;
    }
}

async fn wait_for_prompt_task(handler: &RequestHandler, name: &'static str) {
    tokio::task::yield_now().await;
    tokio::time::timeout(background_shell_test_timeout(), async {
        while handler.background_task_running_for_test(name) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("background task {name} did not finish"));
}

async fn prepare_copy_cursor(handler: &RequestHandler, target: &PaneTarget) {
    let transcript = {
        let state = handler.state.lock().await;
        state.transcript_handle(target).expect("pane transcript")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex")
        .history_limit();
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 6 }, history_limit);
    let mut parser = InputParser::new();
    parser.parse(
        b"zero one two three\r\nalpha beta gamma\r\nomega sigma tau\r\n",
        &mut screen,
    );
    transcript
        .lock()
        .expect("pane transcript mutex")
        .set_screen_for_test(screen);
    let command = format!(
        "copy-mode -t {target}; send-keys -Xt {target} history-top; \
         send-keys -Xt {target} start-of-line; send-keys -N6 -Xt {target} cursor-right"
    );
    let parsed = CommandParser::new()
        .parse(&command)
        .expect("copy setup parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("copy setup executes");
}

async fn selection_coordinates(
    handler: &RequestHandler,
    session: &SessionName,
) -> Option<(u32, usize)> {
    let state = handler.state.lock().await;
    state
        .pane_copy_mode_summary(session, PaneId::new(0))
        .and_then(|summary| summary.selection_start)
        .map(|position| (position.x, position.y))
}

#[tokio::test]
async fn detached_prompts_drop_mouse_event_but_foreground_prompts_preserve_it() {
    // Oracle tmux 3.7b: background and incremental prompt callbacks append a
    // command with a fresh cmdq state; foreground callbacks reuse the item state.
    for case in PromptCase::ALL {
        let name = format!("prompt-event-{}", case.label());
        let (handler, session, target) = fixture(&name).await;
        let _control_rx = register_attach(&handler, &session).await;
        prepare_copy_cursor(&handler, &target).await;
        let context = QueueExecutionContext::without_caller_cwd()
            .with_current_target(Some(Target::Pane(target.clone())))
            .with_mouse_event(Some(mouse_event(&target)));
        let foreground = start_prompt(
            &handler,
            case,
            &case.command("send-keys -X begin-selection"),
            context,
        )
        .await;
        if matches!(case, PromptCase::CommandIncremental) {
            wait_for_prompt_task(&handler, "rmux-prompt-dispatch").await;
        }
        finish_prompt(&handler, case, foreground).await;

        let expected = if case.is_detached() {
            Some((6, 0))
        } else {
            Some((1, 1))
        };
        assert_eq!(
            selection_coordinates(&handler, &session).await,
            expected,
            "{} mouse event semantics",
            case.label()
        );
    }
}

#[tokio::test]
async fn detached_prompts_drop_mouse_target_but_foreground_prompts_preserve_it() {
    // `=` is tmux's current mouse target. A detached prompt callback has no
    // mouse target and therefore cannot select the clicked pane.
    for case in PromptCase::ALL {
        let name = format!("prompt-target-{}", case.label());
        let (handler, session, current) = fixture(&name).await;
        let _control_rx = register_attach(&handler, &session).await;
        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
        let selected = handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: current.clone(),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await;
        assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");

        let mouse_target = PaneTarget::with_window(session.clone(), 0, 1);
        let context = QueueExecutionContext::without_caller_cwd()
            .with_current_target(Some(Target::Pane(current)))
            .with_mouse_target(Some(Target::Pane(mouse_target)));
        let foreground =
            start_prompt(&handler, case, &case.command("select-pane -t ="), context).await;
        if matches!(case, PromptCase::CommandIncremental) {
            wait_for_prompt_task(&handler, "rmux-prompt-dispatch").await;
        }
        finish_prompt(&handler, case, foreground).await;

        let active = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&session)
                .expect("session exists")
                .window()
                .active_pane_index()
        };
        assert_eq!(
            active,
            if case.is_detached() { 0 } else { 1 },
            "{} mouse target semantics",
            case.label()
        );
    }
}
