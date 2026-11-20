use super::frs1::{cleanup, floating_rect, window};
use super::frs1_sibling_producers::{
    grant_unattached_write_access, setup_sibling_fixture, setup_tiled_source_fixture,
    spawn_requester,
};
use super::*;
use rmux_proto::{PaneId, SessionId};

fn canonical_commands(input: &str) -> Request {
    Request::SourceFile(Box::new(SourceFileRequest {
        paths: vec![rmux_proto::INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
        quiet: false,
        parse_only: false,
        verbose: false,
        expand_paths: false,
        target: None,
        caller_cwd: None,
        stdin: Some(input.to_owned()),
    }))
}

async fn inherited_ids(
    handler: &RequestHandler,
    primary: &SessionName,
    unrelated: &SessionName,
) -> (SessionId, PaneId, SessionId) {
    let state = handler.state.lock().await;
    let primary_session = state.sessions.session(primary).expect("primary session");
    (
        primary_session.id(),
        primary_session
            .window_at(0)
            .and_then(|window| window.pane(1))
            .expect("primary pane")
            .id(),
        state
            .sessions
            .session(unrelated)
            .expect("unrelated session")
            .id(),
    )
}

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = PathBuf::from(system_root).join("System32").join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn create_unrelated_witness(handler: &RequestHandler, session: &SessionName) {
    // Whole-window equality needs a witness without independent prompt output or renames.
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: Some("unrelated-witness".to_owned()),
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
}

#[tokio::test]
async fn canonical_sequences_refresh_only_before_an_implicit_consumer() {
    let (handler, primary) = setup_sibling_fixture().await;
    let unrelated = session_name("frs-boundary-unrelated");
    create_unrelated_witness(&handler, &unrelated).await;
    let (primary_id, pane_id, unrelated_id) = inherited_ids(&handler, &primary, &unrelated).await;
    let unrelated_before = window(&handler, &unrelated).await;

    let root = temp_root("sibling-canonical-boundaries");
    fs::create_dir_all(&root).expect("private canonical root");
    let socket_path = root.join("server.sock");
    handler.set_socket_path(&socket_path);
    let pane_environment = pane_id.to_string();
    let valid_rmux = format!("{},1,{}", socket_path.display(), primary_id.as_u32());
    let stale_rmux = format!("{},1,{}", socket_path.display(), unrelated_id.as_u32());
    let valid = spawn_requester(&valid_rmux, &pane_environment).await;
    let stale = spawn_requester(&stale_rmux, &pane_environment).await;
    let _valid_events = grant_unattached_write_access(&handler, valid.0.id()).await;
    let _stale_events = grant_unattached_write_access(&handler, stale.0.id()).await;

    let qualified = handler
        .dispatch(
            stale.0.id(),
            canonical_commands(
                "display-message -p -t frs-sibling-primary:0.1 '#{pane_id}' ; \
                 display-message -p -t frs-sibling-primary:0.1 '#{pane_id}'",
            ),
        )
        .await
        .response;
    let Response::SourceFile(qualified) = qualified else {
        panic!("two qualified canonical commands: {qualified:?}");
    };
    assert_eq!(qualified.exit_status(), None, "{qualified:?}");
    assert!(qualified.stderr().is_empty(), "{qualified:?}");
    assert_eq!(
        qualified
            .command_output()
            .expect("two qualified outputs")
            .stdout(),
        format!("{pane_id}\n{pane_id}\n").as_bytes()
    );

    let valid_implicit = handler
        .dispatch(
            valid.0.id(),
            canonical_commands(
                "display-message -p -t frs-sibling-primary:0.1 '#{pane_id}' ; \
                 display-message -p '#{pane_id}'",
            ),
        )
        .await
        .response;
    let Response::SourceFile(valid_implicit) = valid_implicit else {
        panic!("valid implicit canonical continuation: {valid_implicit:?}");
    };
    assert_eq!(valid_implicit.exit_status(), None, "{valid_implicit:?}");
    assert!(valid_implicit.stderr().is_empty(), "{valid_implicit:?}");
    assert_eq!(
        valid_implicit
            .command_output()
            .expect("qualified and implicit outputs")
            .stdout(),
        format!("{pane_id}\n{pane_id}\n").as_bytes()
    );

    let stale_implicit = handler
        .dispatch(
            stale.0.id(),
            canonical_commands(
                "display-message -p -t frs-sibling-primary:0.1 '#{pane_id}' ; \
                 resize-pane -x35",
            ),
        )
        .await
        .response;
    let Response::SourceFile(stale_implicit) = stale_implicit else {
        panic!("stale implicit canonical continuation: {stale_implicit:?}");
    };
    assert_eq!(stale_implicit.exit_status(), Some(1), "{stale_implicit:?}");
    assert!(
        String::from_utf8_lossy(stale_implicit.stderr())
            .contains("not present in inherited session"),
        "{stale_implicit:?}"
    );
    assert_eq!(
        stale_implicit
            .command_output()
            .expect("first qualified output remains visible")
            .stdout(),
        format!("{pane_id}\n").as_bytes()
    );
    assert_eq!(
        floating_rect(&window(&handler, &primary).await, 1).cols(),
        30,
        "implicit stale resize must not run"
    );
    assert_eq!(window(&handler, &unrelated).await, unrelated_before);

    cleanup(&handler).await;
    drop(stale);
    drop(valid);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn qualified_next_source_batch_runs_after_the_inherited_pane_retires() {
    let (handler, primary) = setup_sibling_fixture().await;
    let unrelated = session_name("frs-batch-unrelated");
    create_background_identity_session(&handler, unrelated.clone()).await;
    let (primary_id, pane_id, _) = inherited_ids(&handler, &primary, &unrelated).await;
    let root_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&primary)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("primary root pane")
            .id()
    };

    let root = temp_root("sibling-multi-batch");
    fs::create_dir_all(&root).expect("private multi-batch root");
    let socket_path = root.join("server.sock");
    handler.set_socket_path(&socket_path);
    let pane_environment = pane_id.to_string();
    let valid_rmux = format!("{},1,{}", socket_path.display(), primary_id.as_u32());
    let valid = spawn_requester(&valid_rmux, &pane_environment).await;
    let _valid_events = grant_unattached_write_access(&handler, valid.0.id()).await;

    let retire = root.join("retire.conf");
    let qualified = root.join("qualified.conf");
    write_config(&retire, "kill-pane -t frs-sibling-primary:0.1\n");
    write_config(
        &qualified,
        "display-message -p -t frs-sibling-primary:0.0 '#{pane_id}'\n",
    );
    let response = handler
        .dispatch(
            valid.0.id(),
            source_file_request(
                vec![
                    retire.to_string_lossy().into_owned(),
                    qualified.to_string_lossy().into_owned(),
                ],
                Some(root.clone()),
            ),
        )
        .await
        .response;
    let Response::SourceFile(response) = response else {
        panic!("qualified second source batch: {response:?}");
    };
    assert_eq!(response.exit_status(), None, "{response:?}");
    assert!(response.stderr().is_empty(), "{response:?}");
    assert_eq!(
        response
            .command_output()
            .expect("second batch qualified output")
            .stdout(),
        format!("{root_pane_id}\n").as_bytes()
    );
    assert!(
        window(&handler, &primary).await.pane(1).is_none(),
        "first batch retires the inherited float"
    );

    cleanup(&handler).await;
    drop(valid);
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn explicit_nested_source_returns_to_the_implicit_parent_context() {
    let (handler, primary) = setup_tiled_source_fixture().await;
    let unrelated = session_name("frs-insertion-unrelated");
    create_unrelated_witness(&handler, &unrelated).await;
    let donor = session_name("frs-insertion-donor");
    create_background_identity_session(&handler, donor.clone()).await;
    handler.wait_for_initial_panes_for_test().await;
    let unrelated_before = window(&handler, &unrelated).await;
    let (primary_id, pane_id, _) = inherited_ids(&handler, &primary, &unrelated).await;
    let donor_pane_id = window(&handler, &donor)
        .await
        .pane(0)
        .expect("donor pane before nested source")
        .id();
    assert_ne!(
        donor_pane_id, pane_id,
        "fixture has distinct pane identities"
    );
    let root_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&primary)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .expect("primary root pane")
            .id()
    };

    let root = temp_root("sibling-explicit-insertion");
    fs::create_dir_all(&root).expect("private insertion root");
    let socket_path = root.join("server.sock");
    handler.set_socket_path(&socket_path);
    let pane_environment = pane_id.to_string();
    let valid_rmux = format!("{},1,{}", socket_path.display(), primary_id.as_u32());
    let valid = spawn_requester(&valid_rmux, &pane_environment).await;
    let _valid_events = grant_unattached_write_access(&handler, valid.0.id()).await;

    write_config(
        &root.join("inner.conf"),
        "display-message -p 'INNER-#{pane_id}'\n",
    );
    write_config(
        &root.join("outer.conf"),
        "source-file -t frs-sibling-primary:0.0 inner.conf ; \
         display-message -p 'OUTER-#{pane_id}'\n",
    );
    let response = handler
        .dispatch(
            valid.0.id(),
            source_file_request(vec!["outer.conf".to_owned()], Some(root.clone())),
        )
        .await
        .response;
    let Response::SourceFile(response) = response else {
        panic!("explicit nested source response: {response:?}");
    };
    assert_eq!(response.exit_status(), None, "{response:?}");
    assert!(response.stderr().is_empty(), "{response:?}");
    assert_eq!(
        response
            .command_output()
            .expect("nested and parent output")
            .stdout(),
        format!("INNER-{root_pane_id}\nOUTER-{pane_id}\n").as_bytes()
    );
    assert_eq!(
        i32::from(
            window(&handler, &primary)
                .await
                .pane(1)
                .expect("pane")
                .geometry()
                .cols()
        ),
        30
    );

    write_config(
        &root.join("replace-inner.conf"),
        "swap-pane -d -s frs-sibling-primary:0.1 -t frs-insertion-donor:0.0 ; \
         display-message -p -t frs-sibling-primary:0.1 'REPLACEMENT-#{pane_id}'\n",
    );
    write_config(
        &root.join("replace-outer.conf"),
        "source-file -t frs-sibling-primary:0.0 replace-inner.conf ; \
         resize-pane -x35\n",
    );
    let replacement_requester = spawn_requester(&valid_rmux, &pane_environment).await;
    let _replacement_events =
        grant_unattached_write_access(&handler, replacement_requester.0.id()).await;
    let stale_parent = handler
        .dispatch(
            replacement_requester.0.id(),
            source_file_request(vec!["replace-outer.conf".to_owned()], Some(root.clone())),
        )
        .await
        .response;
    let Response::SourceFile(stale_parent) = stale_parent else {
        panic!("stale implicit parent after explicit insertion: {stale_parent:?}");
    };
    let primary_after_replacement = window(&handler, &primary).await;
    let replacement = primary_after_replacement
        .pane(1)
        .expect("replacement reuses pane index one");
    assert_ne!(
        replacement.id(),
        pane_id,
        "replacement must have a new PaneId: {stale_parent:?}"
    );
    assert_eq!(
        replacement.id(),
        donor_pane_id,
        "nested swap moved the donor"
    );
    assert_eq!(
        window(&handler, &donor)
            .await
            .pane(0)
            .expect("swapped pane")
            .id(),
        pane_id,
        "the inherited pane left the parent session"
    );
    assert_eq!(
        stale_parent
            .command_output()
            .expect("qualified inner display output remains visible")
            .stdout(),
        format!("REPLACEMENT-{}\n", replacement.id()).as_bytes()
    );
    assert_eq!(
        i32::from(
            primary_after_replacement
                .pane(1)
                .expect("pane")
                .geometry()
                .cols()
        ),
        30,
        "stale parent resize must not target the replacement"
    );
    assert_eq!(window(&handler, &unrelated).await, unrelated_before);
    assert_eq!(stale_parent.exit_status(), Some(1), "{stale_parent:?}");
    assert!(
        String::from_utf8_lossy(stale_parent.stderr()).contains(&format!(
            "inherited pane {pane_id} is not present in inherited session"
        )),
        "{stale_parent:?}"
    );

    cleanup(&handler).await;
    drop(replacement_requester);
    drop(valid);
    let _ = fs::remove_dir_all(root);
}
