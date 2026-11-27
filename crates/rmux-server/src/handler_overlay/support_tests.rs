//! A popup job's shell command is handed to the job's shell as written, as tmux does.
//!
//! Measured on tmux 3.7b with a client attached to session B and `-t` naming the pane of session
//! A: with the shell-command omitted the job runs A's `default-command`, and with it given the job
//! runs that command; in both cases `s=#{session_name} p=#{pane_id}` reaches `default-shell -c`
//! unexpanded.

use rmux_proto::SessionName;

use super::super::super::RequestHandler;
use super::super::completion_tests::{cli_command, exit_status};
use super::{popup_shell_command, ParsedDisplayPopupCommand};

const DEFAULT_A: &str = "job a s=#{session_name} p=#{pane_id}";
const DEFAULT_B: &str = "job b s=#{session_name} p=#{pane_id}";
const EXPLICIT: &str = "job e s=#{session_name} p=#{pane_id}";

async fn run(handler: &RequestHandler, command: &str) {
    let response = handler.handle(cli_command(command.to_owned())).await;
    assert_eq!(exit_status(&response), None, "{response:?}");
}

/// Two sessions whose own default-commands hold formats.
async fn two_sessions() -> RequestHandler {
    let handler = RequestHandler::new();
    for (name, default_command) in [("dcf-a", DEFAULT_A), ("dcf-b", DEFAULT_B)] {
        run(
            &handler,
            &format!("new-session -d -s {name} -x 80 -y 24 'sleep 30'"),
        )
        .await;
        run(
            &handler,
            &format!("set-option -t {name} default-command '{default_command}'"),
        )
        .await;
    }
    handler
}

fn popup(command: Option<&str>, no_job: bool) -> ParsedDisplayPopupCommand {
    ParsedDisplayPopupCommand {
        target_client: None,
        target_pane: None,
        title: String::new(),
        x: None,
        y: None,
        width: None,
        height: None,
        style: None,
        border_style: None,
        border_lines: None,
        close_existing: false,
        close_on_exit: true,
        close_on_zero_exit: false,
        close_any_key: false,
        no_job,
        start_directory: None,
        environment: Vec::new(),
        command: command.map(str::to_owned),
    }
}

async fn shell_command(popup: ParsedDisplayPopupCommand) -> Option<String> {
    let handler = two_sessions().await;
    let state = handler.state.lock().await;
    let target = SessionName::new("dcf-a").expect("valid session name");
    popup_shell_command(&state, &target, &popup)
}

#[tokio::test]
async fn an_omitted_command_runs_the_target_default_command_as_written() {
    assert_eq!(
        shell_command(popup(None, false)).await.as_deref(),
        Some(DEFAULT_A)
    );
}

#[tokio::test]
async fn an_explicit_command_runs_as_written() {
    assert_eq!(
        shell_command(popup(Some(EXPLICIT), false)).await.as_deref(),
        Some(EXPLICIT)
    );
}

#[tokio::test]
async fn a_popup_without_a_job_has_no_command() {
    assert_eq!(shell_command(popup(Some(EXPLICIT), true)).await, None);
}
