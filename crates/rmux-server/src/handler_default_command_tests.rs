use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    BindKeyRequest, NewSessionExtRequest, NewSessionRequest, NewWindowRequest, OptionName,
    PaneTarget, ProcessCommand, Request, RespawnPaneRequest, RespawnWindowRequest, Response,
    ScopeSelector, SessionName, SetOptionMode, SetOptionRequest, SourceFileRequest, SplitDirection,
    SplitWindowRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

use super::RequestHandler;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn tagged_stdin_discard_command(tag: &str) -> String {
    #[cfg(unix)]
    {
        format!("cat >/dev/null # {tag}")
    }
    #[cfg(windows)]
    {
        crate::test_shell::powershell_encoded_command(&format!(
            "$tag='{}'; $inputStream=[Console]::OpenStandardInput(); $inputStream.CopyTo([System.IO.Stream]::Null)",
            tag.replace('\'', "''")
        ))
    }
}

fn unique_temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-default-command-{label}-{}-{unique}",
        std::process::id()
    ))
}

#[cfg(windows)]
fn expected_spawn_cwd(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let rendered = canonical.display().to_string();
    let normalized = if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        rendered
            .strip_prefix(r"\\?\")
            .unwrap_or(&rendered)
            .to_owned()
    };
    PathBuf::from(normalized)
}

#[cfg(unix)]
fn expected_spawn_cwd(path: &Path) -> PathBuf {
    path.to_path_buf()
}

async fn create_session(handler: &RequestHandler, session: &SessionName) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn create_grouped_session(
    handler: &RequestHandler,
    session: &SessionName,
    group_target: &SessionName,
) {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
}

async fn set_default_command(handler: &RequestHandler, scope: ScopeSelector, command: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope,
            option: OptionName::DefaultCommand,
            value: command.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn pane_process_command(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> Option<ProcessCommand> {
    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.pane_id_in_window(target.window_index(), target.pane_index()))
        .expect("target pane exists");
    state
        .pane_lifecycle(pane_id)
        .expect("target pane lifecycle exists")
        .process_command()
        .cloned()
}

async fn assert_default_command_on_new_and_split(
    handler: &RequestHandler,
    session: &SessionName,
    expected: &str,
) {
    let new_window = PaneTarget::with_window(session.clone(), 1, 0);
    let split = PaneTarget::with_window(session.clone(), 0, 1);
    assert_eq!(
        pane_process_command(handler, &new_window).await,
        Some(ProcessCommand::Shell(expected.to_owned()))
    );
    assert_eq!(
        pane_process_command(handler, &split).await,
        Some(ProcessCommand::Shell(expected.to_owned()))
    );
}

#[tokio::test]
async fn sdk_new_and_split_resolve_default_command_for_the_addressed_session() {
    let handler = RequestHandler::new();
    let owner = session_name("default-command-owner");
    let alias = session_name("default-command-alias");
    let fallback = session_name("default-command-fallback");
    create_session(&handler, &owner).await;
    create_grouped_session(&handler, &alias, &owner).await;
    create_session(&handler, &fallback).await;

    let global_command = tagged_stdin_discard_command("global");
    let owner_command = tagged_stdin_discard_command("owner");
    let alias_command = tagged_stdin_discard_command("alias");
    set_default_command(&handler, ScopeSelector::Global, &global_command).await;
    set_default_command(
        &handler,
        ScopeSelector::Session(owner.clone()),
        &owner_command,
    )
    .await;
    set_default_command(
        &handler,
        ScopeSelector::Session(alias.clone()),
        &alias_command,
    )
    .await;

    let owner_window = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: owner.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let owner_window = match owner_window {
        Response::NewWindow(response) => {
            PaneTarget::with_window(owner.clone(), response.target.window_index(), 0)
        }
        response => panic!("expected new-window success, got {response:?}"),
    };
    assert_eq!(
        pane_process_command(&handler, &owner_window).await,
        Some(ProcessCommand::Shell(owner_command))
    );

    let alias_window = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alias.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let alias_window = match alias_window {
        Response::NewWindow(response) => {
            PaneTarget::with_window(alias.clone(), response.target.window_index(), 0)
        }
        response => panic!("expected grouped new-window success, got {response:?}"),
    };
    assert_eq!(
        pane_process_command(&handler, &alias_window).await,
        Some(ProcessCommand::Shell(alias_command))
    );

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(owner_window),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    let split = match split {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };
    assert_eq!(
        pane_process_command(&handler, &split).await,
        Some(ProcessCommand::Shell(tagged_stdin_discard_command("owner")))
    );

    let fallback_window = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: fallback.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let fallback_window = match fallback_window {
        Response::NewWindow(response) => {
            PaneTarget::with_window(fallback, response.target.window_index(), 0)
        }
        response => panic!("expected fallback new-window success, got {response:?}"),
    };
    assert_eq!(
        pane_process_command(&handler, &fallback_window).await,
        Some(ProcessCommand::Shell(global_command))
    );
}

#[tokio::test]
async fn sdk_explicit_command_wins_and_local_empty_masks_global_default_command() {
    let handler = RequestHandler::new();
    let alpha = session_name("default-command-explicit");
    let masked = session_name("default-command-masked");
    create_session(&handler, &alpha).await;
    create_session(&handler, &masked).await;
    set_default_command(
        &handler,
        ScopeSelector::Global,
        &tagged_stdin_discard_command("global"),
    )
    .await;
    set_default_command(&handler, ScopeSelector::Session(masked.clone()), "").await;

    let explicit = ProcessCommand::Shell(tagged_stdin_discard_command("explicit"));
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha,
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: Some(explicit.clone()),
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let explicit_target = match response {
        Response::NewWindow(response) => PaneTarget::with_window(
            response.target.session_name().clone(),
            response.target.window_index(),
            0,
        ),
        response => panic!("expected explicit new-window success, got {response:?}"),
    };
    assert_eq!(
        pane_process_command(&handler, &explicit_target).await,
        Some(explicit)
    );

    let explicit_session = explicit_target.session_name().clone();
    let remain = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(explicit_session.clone()),
            option: OptionName::RemainOnExit,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(remain, Response::SetOption(_)), "{remain:?}");
    let empty = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: explicit_session,
            name: None,
            detached: true,
            environment: None,
            command: Some(vec![String::new()]),
            process_command: None,
            start_directory: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let empty_target = match empty {
        Response::NewWindow(response) => PaneTarget::with_window(
            response.target.session_name().clone(),
            response.target.window_index(),
            0,
        ),
        response => panic!("expected empty explicit new-window success, got {response:?}"),
    };
    assert_eq!(
        pane_process_command(&handler, &empty_target).await,
        Some(ProcessCommand::Shell(String::new()))
    );

    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(masked),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    let masked_target = match response {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected masked split-window success, got {response:?}"),
    };
    assert_eq!(pane_process_command(&handler, &masked_target).await, None);
}

#[tokio::test]
async fn default_command_preserves_requested_cwd_and_respawn_provenance() {
    let handler = RequestHandler::new();
    let alpha = session_name("default-command-cwd-respawn");
    let cwd = unique_temp_path("cwd");
    fs::create_dir_all(&cwd).expect("create default-command cwd");
    let cwd = fs::canonicalize(cwd).expect("canonical default-command cwd");
    create_session(&handler, &alpha).await;

    let original = tagged_stdin_discard_command("original");
    set_default_command(&handler, ScopeSelector::Global, &original).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            process_command: None,
            start_directory: Some(cwd.clone()),
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let target = match response {
        Response::NewWindow(response) => {
            PaneTarget::with_window(alpha.clone(), response.target.window_index(), 0)
        }
        response => panic!("expected cwd new-window success, got {response:?}"),
    };
    {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| {
                session.pane_id_in_window(target.window_index(), target.pane_index())
            })
            .expect("cwd pane exists");
        assert_eq!(
            state
                .pane_lifecycle(pane_id)
                .expect("cwd pane lifecycle exists")
                .working_directory(),
            Some(expected_spawn_cwd(&cwd).as_path())
        );
    }

    set_default_command(
        &handler,
        ScopeSelector::Global,
        &tagged_stdin_discard_command("changed"),
    )
    .await;
    let respawn_window = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), target.window_index()),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
        })))
        .await;
    assert!(
        matches!(respawn_window, Response::RespawnWindow(_)),
        "{respawn_window:?}"
    );
    assert_eq!(
        pane_process_command(&handler, &target).await,
        Some(ProcessCommand::Shell(original.clone()))
    );

    set_default_command(
        &handler,
        ScopeSelector::Global,
        &tagged_stdin_discard_command("changed-again"),
    )
    .await;
    let respawn_pane = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
        })))
        .await;
    assert!(
        matches!(respawn_pane, Response::RespawnPane(_)),
        "{respawn_pane:?}"
    );
    assert_eq!(
        pane_process_command(&handler, &target).await,
        Some(ProcessCommand::Shell(original))
    );

    drop(handler);
    let _ = fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn queued_source_and_binding_paths_apply_default_command() {
    let handler = RequestHandler::new();
    let queued = session_name("default-command-queued");
    let sourced = session_name("default-command-sourced");
    let bound = session_name("default-command-bound");
    for session in [&queued, &sourced, &bound] {
        create_session(&handler, session).await;
    }
    let default_command = tagged_stdin_discard_command("entry-paths");
    set_default_command(&handler, ScopeSelector::Global, &default_command).await;

    let parsed = CommandParser::new()
        .parse(&format!(
            "new-window -d -t {queued} ; split-window -d -t {queued}:0.0"
        ))
        .expect("queued window commands parse");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .expect("queued window commands execute");
    assert_default_command_on_new_and_split(&handler, &queued, &default_command).await;

    let root = unique_temp_path("source-entry-path");
    fs::create_dir_all(&root).expect("create source-file root");
    let config = root.join("windows.conf");
    fs::write(
        &config,
        format!("new-window -d -t {sourced}\nsplit-window -d -t {sourced}:0.0\n"),
    )
    .expect("write source-file commands");
    let source = handler
        .handle(Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![config.to_string_lossy().into_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: None,
        })))
        .await;
    assert!(matches!(source, Response::SourceFile(_)), "{source:?}");
    assert_default_command_on_new_and_split(&handler, &sourced, &default_command).await;

    let requester_pid = u32::MAX - 91;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, bound.clone(), control_tx)
        .await;
    for (key, command) in [
        (
            "N",
            vec![
                "new-window".to_owned(),
                "-d".to_owned(),
                "-t".to_owned(),
                bound.to_string(),
            ],
        ),
        (
            "S",
            vec![
                "split-window".to_owned(),
                "-d".to_owned(),
                "-t".to_owned(),
                format!("{bound}:0.0"),
            ],
        ),
    ] {
        let response = handler
            .handle(Request::BindKey(Box::new(BindKeyRequest {
                table_name: "prefix".to_owned(),
                key: key.to_owned(),
                note: Some("default-command regression".to_owned()),
                repeat: false,
                command: Some(command),
            })))
            .await;
        assert!(matches!(response, Response::BindKey(_)), "{response:?}");
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02N\x02S")
        .await
        .expect("attached bindings execute");
    assert_default_command_on_new_and_split(&handler, &bound, &default_command).await;

    drop(handler);
    let _ = fs::remove_dir_all(root);
}
