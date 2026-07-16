use super::*;

#[cfg(any(unix, windows))]
#[test]
fn live_signal_guard_is_disarmed_by_detach_without_killing_the_session() {
    let (runtime, owned, mut daemon, state) = signal_install_fixture("retry-signal-owner");

    assert_signal_install_fails_without_latching(&owned, &state);

    let guard = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect("installation can be retried inside a Tokio runtime");
    let duplicate = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect_err("an installed guard must keep the uniqueness reservation");
    assert!(
        duplicate.to_string().contains("already installed"),
        "unexpected duplicate-install error: {duplicate}"
    );
    let detached = runtime
        .block_on(owned.detach_owned())
        .expect("detaching ownership must disarm the live signal guard");
    assert!(state.is_disarmed(), "live signal cleanup must be disarmed");
    assert!(
        !state.try_begin_cleanup(),
        "a signal after detach must not begin cleanup"
    );

    runtime.block_on(async {
        assert!(
            tokio::time::timeout(Duration::from_millis(250), daemon.read_request())
                .await
                .is_err(),
            "detach with a live signal guard must not kill the session"
        );
    });
    drop(guard);
    drop(detached);
}

#[cfg(any(unix, windows))]
#[test]
fn live_signal_guard_is_disarmed_by_preserve_without_killing_the_session() {
    let (runtime, owned, mut daemon, state) = signal_install_fixture("preserve-signal-owner");

    assert_signal_install_fails_without_latching(&owned, &state);

    let guard = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect("installation can be retried inside a Tokio runtime");
    let preserved = runtime
        .block_on(owned.preserve())
        .expect("preserving ownership must disarm the live signal guard");
    assert!(state.is_disarmed(), "live signal cleanup must be disarmed");
    assert!(
        !state.try_begin_cleanup(),
        "a signal after preserve must not begin cleanup"
    );
    let keepalive = preserved.session().transport().clone();

    runtime.block_on(async {
        assert!(
            tokio::time::timeout(Duration::from_millis(250), daemon.read_request())
                .await
                .is_err(),
            "preserve with a live signal guard must not kill the session"
        );
    });
    drop(guard);
    drop(preserved);
    drop(keepalive);
}

#[cfg(any(unix, windows))]
#[test]
fn detach_rejects_ownership_release_after_signal_cleanup_starts() {
    let (runtime, owned, _daemon, state) = signal_install_fixture("detach-during-signal-cleanup");
    let guard = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect("signal handlers install inside a Tokio runtime");
    assert!(state.try_begin_cleanup(), "signal begins cleanup once");

    drop(guard);

    let error = runtime
        .block_on(owned.detach_owned())
        .expect_err("detach must not release ownership after signal cleanup starts");
    assert!(
        error.to_string().contains("already in progress"),
        "unexpected detach error: {error}"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn preserve_rejects_ownership_release_after_signal_cleanup_starts() {
    let (runtime, owned, _daemon, state) = signal_install_fixture("preserve-during-signal-cleanup");
    let guard = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect("signal handlers install inside a Tokio runtime");
    assert!(state.try_begin_cleanup(), "signal begins cleanup once");

    drop(guard);

    let error = runtime
        .block_on(owned.preserve())
        .expect_err("preserve must not release ownership after signal cleanup starts");
    assert!(
        error.to_string().contains("already in progress"),
        "unexpected preserve error: {error}"
    );
}

#[cfg(any(unix, windows))]
fn signal_install_fixture(
    name: &str,
) -> (
    tokio::runtime::Runtime,
    OwnedSession,
    FakeDaemon,
    Arc<signals::SignalHandlerState>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    let (owned, daemon, state) = runtime.block_on(async {
        let (client_stream, server_stream) = tokio::io::duplex(1024);
        let state = Arc::new(signals::SignalHandlerState::default());
        let owned = OwnedSession {
            session: Some(Session::new(
                SessionName::new(name).expect("valid session name"),
                crate::RmuxEndpoint::Default,
                None,
                TransportClient::spawn(client_stream),
                true,
                None,
            )),
            session_id: SessionId::new(42),
            cleanup_policy: CleanupPolicy::KillOnDrop,
            lease: None,
            signal_handler_state: Arc::clone(&state),
        };
        (owned, FakeDaemon::new(server_stream), state)
    });
    (runtime, owned, daemon, state)
}

#[cfg(any(unix, windows))]
fn assert_signal_install_fails_without_latching(
    owned: &OwnedSession,
    state: &signals::SignalHandlerState,
) {
    let error = owned
        .install_default_signal_handlers()
        .expect_err("installation outside a Tokio runtime must fail");
    assert!(
        error.to_string().contains("require a Tokio runtime"),
        "unexpected installation error: {error}"
    );
    assert!(
        !state.is_installed(),
        "failed installation must release the single-handler reservation"
    );
}

#[tokio::test]
async fn released_owner_rejects_signal_handlers_without_latching_installation() {
    let (client_stream, _server_stream) = tokio::io::duplex(1024);
    let state = Arc::new(signals::SignalHandlerState::default());
    let owned = OwnedSession {
        session: Some(Session::new(
            SessionName::new("preserved-owner").expect("valid session name"),
            crate::RmuxEndpoint::Default,
            None,
            TransportClient::spawn(client_stream),
            true,
            None,
        )),
        session_id: SessionId::new(42),
        cleanup_policy: CleanupPolicy::Preserve,
        lease: None,
        signal_handler_state: Arc::clone(&state),
    };

    let error = owned
        .install_default_signal_handlers()
        .expect_err("released ownership cannot install token-guarded signal cleanup");

    assert!(
        error
            .to_string()
            .contains("owned session ownership has already been released"),
        "unexpected error: {error}"
    );
    assert!(
        !state.is_installed(),
        "rejected installation must not latch the single-handler flag"
    );
}
