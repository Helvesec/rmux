#![cfg(unix)]

use std::error::Error;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_proto::{
    HasSessionRequest, HookLifecycle, HookName, ListPanesRequest, ListSessionsRequest,
    NewSessionRequest, NewWindowRequest, Request, Response, ScopeSelector, SendKeysRequest,
    SetEnvironmentRequest, SetHookRequest, SetOptionMode, SetOptionRequest, ShowEnvironmentRequest,
    ShowOptionsRequest, SplitWindowRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};

const FILE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn list_sessions_uses_shared_formatter_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("listing-list-sessions");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for (session_name, size) in [
        (
            beta.clone(),
            TerminalSize {
                cols: 120,
                rows: 40,
            },
        ),
        (alpha.clone(), TerminalSize { cols: 80, rows: 24 }),
    ] {
        let created = send_request(
            harness.socket_path(),
            &Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(size),

                environment: None,
            }),
        )
        .await?;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let new_window = send_request(
        harness.socket_path(),
        &Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha,
            name: Some("logs".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })),
    )
    .await?;
    assert!(matches!(new_window, Response::NewWindow(_)));

    let listed = send_request(
        harness.socket_path(),
        &Request::ListSessions(ListSessionsRequest {
            format: Some(
                "#{session_name}:#{session_windows}:#{session_attached}:#{session_width}x#{session_height}"
                    .to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
            }),
    )
    .await?;

    let output = listed
        .command_output()
        .expect("list-sessions returns command output");
    assert_eq!(
        std::str::from_utf8(output.stdout()).expect("list-sessions output is utf-8"),
        "alpha:2:0:x\nbeta:1:0:x\n"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn list_panes_uses_shared_formatter_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("listing-list-panes");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let split = send_request(
        harness.socket_path(),
        &Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(split, Response::SplitWindow(_)));

    let new_window = send_request(
        harness.socket_path(),
        &Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("logs".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })),
    )
    .await?;
    assert!(matches!(new_window, Response::NewWindow(_)));

    let listed = send_request(
        harness.socket_path(),
        &Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha,
            format: Some(
                "#{session_name}:#{window_index}:#{pane_index}:#{pane_id}:#{pane_active}"
                    .to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })),
    )
    .await?;

    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    assert_eq!(
        std::str::from_utf8(output.stdout()).expect("list-panes output is utf-8"),
        "alpha:0:0:%0:0\nalpha:0:1:%1:1\nalpha:1:0:%2:1\n"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn rename_session_round_trips_and_migrates_session_scoped_state() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("listing-rename-session");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");
    let gamma = session_name("gamma");
    let hook_path = std::env::temp_dir().join(format!(
        "rmux-rename-hook-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the unix epoch")
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&hook_path);

    let created = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                name: "TERM".to_owned(),
                value: "screen".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })),
        )
        .await?,
        Response::SetEnvironment(_)
    ));
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::new(alpha.clone())),
                option: rmux_proto::OptionName::PaneBorderStyle,
                value: "red".to_owned(),
                mode: SetOptionMode::Replace,
            }),
        )
        .await?,
        Response::SetOption(_)
    ));
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(alpha.clone()),
                hook: HookName::AfterSendKeys,
                command: format!(
                    "run-shell {}",
                    shell_quote_str(&format!(
                        "printf renamed-hook > {}",
                        shell_quote(&hook_path)
                    ))
                ),
                lifecycle: HookLifecycle::Persistent,
            }),
        )
        .await?,
        Response::SetHook(_)
    ));

    let renamed = send_request(
        harness.socket_path(),
        &Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: alpha.clone(),
            new_name: gamma.clone(),
        }),
    )
    .await?;
    assert_eq!(
        renamed,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: gamma.clone(),
        })
    );

    assert_eq!(
        send_request(
            harness.socket_path(),
            &Request::HasSession(HasSessionRequest {
                target: alpha.clone(),
            }),
        )
        .await?,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );
    assert_eq!(
        send_request(
            harness.socket_path(),
            &Request::HasSession(HasSessionRequest {
                target: gamma.clone(),
            }),
        )
        .await?,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );

    let environment = send_request(
        harness.socket_path(),
        &Request::ShowEnvironment(ShowEnvironmentRequest {
            scope: ScopeSelector::Session(gamma.clone()),
            name: None,
            hidden: false,
            shell_format: false,
        }),
    )
    .await?;
    let environment_output = environment
        .command_output()
        .expect("show-environment returns command output");
    assert_eq!(
        std::str::from_utf8(environment_output.stdout()).expect("environment output is utf-8"),
        "TERM=screen\n"
    );

    let options = send_request(
        harness.socket_path(),
        &Request::ShowOptions(ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::Window(WindowTarget::new(gamma.clone())),
            name: None,
            value_only: false,
            include_inherited: true,
            quiet: false,
            include_hooks: false,
        }),
    )
    .await?;
    let options_output = options
        .command_output()
        .expect("show-options returns command output");
    assert!(std::str::from_utf8(options_output.stdout())
        .expect("options output is utf-8")
        .contains("pane-border-style red"));

    let sent = send_request(
        harness.socket_path(),
        &Request::SendKeys(SendKeysRequest {
            target: rmux_proto::PaneTarget::with_window(gamma.clone(), 0, 0),
            keys: vec!["printf noop".to_owned(), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(sent, Response::SendKeys(_)));
    wait_for_file_contents(&hook_path, "renamed-hook").await?;

    handle.shutdown().await?;
    let _ = std::fs::remove_file(&hook_path);
    Ok(())
}

async fn wait_for_file_contents(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + FILE_WAIT_TIMEOUT;

    while Instant::now() < deadline {
        match std::fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    Err(io::Error::other(format!(
        "timed out waiting for '{}' to contain '{}'",
        path.display(),
        expected
    ))
    .into())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn shell_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
