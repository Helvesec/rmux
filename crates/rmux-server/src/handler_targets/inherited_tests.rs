use super::*;
use rmux_proto::{ResolveTargetRequest, SessionName, TerminalSize};

#[cfg(unix)]
#[tokio::test]
async fn requester_environment_is_read_once_per_memoized_request() {
    let socket = std::env::temp_dir().join(format!("memo-requester-{}.sock", std::process::id()));
    // The test-binary helper, not /bin/sleep: macOS hides a platform binary's
    // environment from KERN_PROCARGS2. The helper reports readiness from its
    // own code, after exec, so its exec-time environment is already readable.
    let mut requester = crate::handler::display_message_tests::spawn_requester_with_environment(&[
        ("RMUX", format!("{},5,9", socket.display())),
        ("RMUX_PANE", "%11".to_owned()),
    ])
    .await;
    let pid = requester.0.id();
    let inherited = RequesterEnvironmentContext {
        pane_id: Some(11),
        session_id: Some(SessionId::new(9)),
        source_depth: None,
    };
    let socket_in_memo = socket.clone();
    let memoized_after_exit = with_requester_environment_memo(async move {
        assert_eq!(
            requester_environment_context(pid, &socket_in_memo),
            inherited
        );
        requester.0.kill().expect("kill requester");
        requester.0.wait().expect("reap requester");
        // The memo answers without reading the process environment again.
        requester_environment_context(pid, &socket_in_memo)
    })
    .await;
    assert_eq!(memoized_after_exit, inherited);
    // Outside a memoized request every call reads the live process again.
    assert_eq!(
        requester_environment_context(pid, &socket),
        RequesterEnvironmentContext::default()
    );
}

#[cfg(unix)]
#[test]
fn requester_context_matches_missing_bare_socket_spellings() {
    let name = format!("review-requester-{}.sock", std::process::id());
    let absolute = std::env::current_dir().unwrap().join(&name);
    assert!(!absolute.exists());
    for inherited in [&name, &format!("./{name}"), absolute.to_str().unwrap()] {
        let environment = HashMap::from([
            ("RMUX".to_owned(), format!("{inherited},123,7")),
            ("RMUX_PANE".to_owned(), "%42".to_owned()),
            ("RMUX_SOURCE_DEPTH".to_owned(), "3".to_owned()),
        ]);
        for path in [Path::new(&name), &Path::new(".").join(&name), &absolute] {
            assert_eq!(
                requester_environment_context_from_map(&environment, path),
                RequesterEnvironmentContext {
                    pane_id: Some(42),
                    session_id: Some(SessionId::new(7)),
                    source_depth: Some(3),
                }
            );
        }
        assert_eq!(
            requester_environment_context_from_map(
                &environment,
                &absolute.with_file_name("other.sock")
            ),
            RequesterEnvironmentContext::default()
        );
    }
}

#[test]
fn requester_context_reads_tmux_pane_without_rmux_unless_tmux_names_another_server() {
    let socket = Path::new("/run/rmux-review/server.sock");
    let context = |vars: &[(&str, &str)]| {
        let environment = vars
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        requester_environment_context_from_map(&environment, socket)
    };
    let pane = RequesterEnvironmentContext {
        pane_id: Some(4),
        session_id: None,
        source_depth: None,
    };
    let none = RequesterEnvironmentContext::default();
    // tmux 3.7b cmd_find_inside_pane: TMUX_PANE names the pane without RMUX.
    assert_eq!(context(&[("TMUX_PANE", "%4")]), pane);
    assert_eq!(
        context(&[
            ("TMUX", "/run/rmux-review/server.sock,9,2"),
            ("TMUX_PANE", "%4")
        ]),
        pane
    );
    assert_eq!(context(&[("TMUX", ""), ("TMUX_PANE", "%4")]), pane);
    assert_eq!(
        context(&[("TMUX_PANE", "%4"), ("RMUX_SOURCE_DEPTH", "2")]),
        pane
    );
    // A TMUX naming another server owns its TMUX_PANE (rmux inside tmux).
    assert_eq!(
        context(&[("TMUX", "/tmp/tmux-1000/default,9,2"), ("TMUX_PANE", "%4")]),
        none
    );
    // RMUX still decides when present; RMUX_PANE stays bound to it.
    assert_eq!(
        context(&[("RMUX", "/tmp/other.sock,9,2"), ("TMUX_PANE", "%4")]),
        none
    );
    assert_eq!(context(&[("RMUX_PANE", "%4")]), none);
    assert_eq!(context(&[("TMUX_PANE", "4")]), none);
}

fn fixture() -> (ManagedSessionStore, SessionName, SessionId, u32) {
    let mut sessions = ManagedSessionStore::new();
    let name = SessionName::new("primary").unwrap();
    sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let session = sessions.session(&name).unwrap();
    let id = session.id();
    let pane = session.window_at(0).unwrap().pane(0).unwrap().id().as_u32();
    (sessions, name, id, pane)
}

fn context(session_id: Option<SessionId>, pane_id: u32) -> RequesterEnvironmentContext {
    RequesterEnvironmentContext {
        pane_id: Some(pane_id),
        session_id,
        source_depth: None,
    }
}

#[test]
fn missing_pane_in_a_live_inherited_session_is_only_an_obsolete_hint() {
    let (sessions, _, id, _) = fixture();
    assert_eq!(
        inherited_pane_target(&sessions, context(Some(id), 999999), None),
        Ok(None)
    );
}

#[test]
fn a_live_pane_in_another_session_is_not_an_obsolete_hint() {
    let (mut sessions, _, _, pane) = fixture();
    let other = SessionName::new("other").unwrap();
    sessions
        .create_session(other.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let id = sessions.session(&other).unwrap().id();
    for requested in [None, Some("."), Some("%999999")] {
        assert!(inherited_pane_target(&sessions, context(Some(id), pane), requested).is_err());
    }
}

#[test]
fn an_ambiguous_linked_pane_is_still_live_and_must_not_fall_back() {
    let (mut sessions, primary, primary_id, pane) = fixture();
    let alias = SessionName::new("alias").unwrap();
    let unrelated = SessionName::new("unrelated").unwrap();
    for name in [&alias, &unrelated] {
        sessions
            .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
            .unwrap();
    }
    let window_id = sessions
        .session(&primary)
        .unwrap()
        .window_at(0)
        .unwrap()
        .id();
    sessions
        .link_window(&WindowTarget::with_window(alias.clone(), 1), window_id)
        .unwrap();
    // Two sessions hold this pane's window now. A bare `%PaneId` is no longer
    // ambiguous: tmux finds the pane in its one global tree and then chooses
    // among the sessions that hold its window - the most recently active of
    // them, the first by name on a tie (`cmd-find.c:498-512` through
    // `cmd_find_best_session_with_window` at 176-197). Which session is the
    // most recent is said here rather than inherited from the creation order.
    sessions.touch_activity(&alias).unwrap();
    assert_eq!(
        pane_id_target(&sessions, pane),
        Some(Target::Pane(PaneTarget::with_window(alias, 1, 0))),
        "the global lookup chooses the most recently active session holding the window"
    );
    // The inherited path is session-scoped and does not move: a session that
    // does not hold the pane is still refused, and the one that does still
    // answers its own occurrence.
    let unrelated_id = sessions.session(&unrelated).unwrap().id();
    assert!(inherited_pane_target(&sessions, context(Some(unrelated_id), pane), None).is_err());
    assert_eq!(
        inherited_pane_target(&sessions, context(Some(primary_id), pane), None),
        Ok(Some(Target::Pane(PaneTarget::with_window(primary, 0, 0))))
    );
}

#[test]
fn missing_inherited_session_stays_invalid_even_when_the_pane_is_missing() {
    let (sessions, _, _, pane) = fixture();
    for pane in [pane, 999999] {
        assert!(inherited_pane_target(
            &sessions,
            context(Some(SessionId::new(999999)), pane),
            None
        )
        .is_err());
    }
}

#[test]
fn valid_and_sessionless_inherited_panes_keep_their_existing_resolution() {
    let (sessions, name, id, pane) = fixture();
    for id in [Some(id), None] {
        assert_eq!(
            inherited_pane_target(&sessions, context(id, pane), None),
            Ok(Some(Target::Pane(PaneTarget::with_window(
                name.clone(),
                0,
                0
            ))))
        );
    }
    assert_eq!(
        inherited_pane_target(&sessions, context(None, 999999), None),
        Ok(None)
    );
}

#[tokio::test]
async fn obsolete_hint_does_not_silence_an_explicit_missing_pane_operand() {
    let (sessions, name, id, _) = fixture();
    let handler = RequestHandler::new();
    handler.state.lock().await.sessions = sessions;
    let error = handler
        .resolve_target_for_requester_with_environment(
            ResolveTargetRequest {
                target: Some("%999999".to_owned()),
                target_type: ResolveTargetType::Pane,
                window_index: false,
                prefer_unattached: false,
            },
            context(Some(id), 999999),
            None,
            Some(name),
            |_, target| Ok(target),
        )
        .await
        .expect_err("the explicit operand still has to resolve");
    assert!(error.to_string().contains("999999"), "{error}");
}
