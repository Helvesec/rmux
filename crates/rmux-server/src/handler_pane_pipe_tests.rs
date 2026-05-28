use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::RequestHandler;
use rmux_proto::{
    DisplayMessageRequest, NewSessionRequest, PaneTarget, PipePaneRequest, Request, Response,
    SendKeysRequest, SessionName, Target, TerminalSize,
};
use tokio::time::sleep;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn unique_temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-pane-pipe-{label}-{}-{unique}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn pipe_to_file_command(path: &Path) -> String {
    format!("cat > {}", crate::test_shell::sh_quote_path(path))
}

#[cfg(windows)]
fn pipe_to_file_command(path: &Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "$out=[System.IO.File]::Open({}, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite); try {{ $buf=New-Object byte[] 4096; $inputStream=[Console]::OpenStandardInput(); while (($n=$inputStream.Read($buf,0,$buf.Length)) -gt 0) {{ $out.Write($buf,0,$n); $out.Flush() }} }} finally {{ $out.Dispose() }}",
        crate::test_shell::powershell_quote_path(path)
    ))
}

fn pipe_discard_command() -> String {
    crate::test_shell::stdin_discard_command()
}

#[cfg(unix)]
fn pane_print_command(text: &str) -> String {
    format!("printf '{}\\n'", text.replace('\'', r"'\''"))
}

#[cfg(windows)]
fn pane_print_command(text: &str) -> String {
    format!("echo {text}")
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
}

async fn display_pane_format(
    handler: &RequestHandler,
    target: PaneTarget,
    message: &str,
) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some(message.to_owned()),
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    String::from_utf8_lossy(output.stdout())
        .trim_end()
        .to_owned()
}

async fn pipe_pane(
    handler: &RequestHandler,
    target: PaneTarget,
    once: bool,
    command: Option<String>,
) {
    let response = handler
        .handle(Request::PipePane(PipePaneRequest {
            target,
            stdin: false,
            stdout: true,
            once,
            command,
        }))
        .await;
    assert!(matches!(response, Response::PipePane(_)));
}

async fn wait_for_file_contains(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents.contains(expected) => return,
            Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                sleep(Duration::from_millis(25)).await;
            }
            Ok(contents) => panic!(
                "timed out waiting for {} to contain {:?}, got {:?}",
                path.display(),
                expected,
                contents
            ),
            Err(error) => panic!(
                "timed out waiting for {} to exist containing {:?}: {error}",
                path.display(),
                expected
            ),
        }
    }
}

#[tokio::test]
async fn pipe_pane_once_closes_existing_pipe_without_reopening() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let first_output = unique_temp_path("once-first");
    let second_output = unique_temp_path("once-second");
    create_session(&handler, "alpha").await;

    pipe_pane(
        &handler,
        target.clone(),
        false,
        Some(pipe_to_file_command(&first_output)),
    )
    .await;
    let sent = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec![pane_print_command("pipe-one"), "Enter".to_owned()],
        }))
        .await;
    assert!(matches!(sent, Response::SendKeys(_)));
    wait_for_file_contains(&first_output, "pipe-one").await;

    pipe_pane(
        &handler,
        target.clone(),
        true,
        Some(pipe_to_file_command(&second_output)),
    )
    .await;
    let sent = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec![pane_print_command("pipe-two"), "Enter".to_owned()],
        }))
        .await;
    assert!(matches!(sent, Response::SendKeys(_)));
    sleep(Duration::from_millis(250)).await;

    let first_contents = fs::read_to_string(&first_output).expect("first pipe output exists");
    assert!(first_contents.contains("pipe-one"));
    assert!(!first_contents.contains("pipe-two"));
    assert!(!second_output.exists() || fs::read_to_string(&second_output).unwrap().is_empty());

    let _ = fs::remove_file(first_output);
    let _ = fs::remove_file(second_output);
}

#[tokio::test]
async fn pane_pipe_format_reports_active_pipe_state() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    create_session(&handler, "alpha").await;

    assert_eq!(
        display_pane_format(&handler, target.clone(), "#{pane_pipe}").await,
        "0"
    );
    pipe_pane(
        &handler,
        target.clone(),
        false,
        Some(pipe_discard_command()),
    )
    .await;
    assert_eq!(
        display_pane_format(&handler, target.clone(), "#{pane_pipe}").await,
        "1"
    );
    pipe_pane(&handler, target.clone(), false, None).await;
    assert_eq!(
        display_pane_format(&handler, target, "#{pane_pipe}").await,
        "0"
    );
}
