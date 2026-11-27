//! Exact-name session calls against a live daemon (R3513 H1).
//!
//! A bare session name is a tmux target that the daemon resolves by unique
//! prefix, pattern or special token; the SDK's exact-name calls must not.
//! A canonical name that contains a backslash is never matched by an `=`
//! target, because the daemon escapes the target's backslashes again (R3516).

use std::path::Path;

use rmux_proto::{ListSessionsRequest, Request, Response};
use rmux_sdk::{EnsureSession, Session, SessionName};

use super::{
    assert_session_not_found, framed_request, raw_list_session_names, raw_new_session,
    session_name, Harness, TestResult, LIVE_DAEMON_LOCK,
};

#[tokio::test]
async fn exact_name_checks_never_bind_a_prefix_or_pattern_match() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("exact-check").await?;
    let rmux = harness.rmux();
    let worker = session_name("r3513worker");
    raw_new_session(harness.socket_path(), worker.clone()).await?;

    for probe in ["r3513work", "r3513wor*", "=r3513worker", "~", "{current}"] {
        assert!(
            !rmux.has_session(session_name(probe)).await?,
            "has_session({probe}) matched r3513worker"
        );
    }
    let reuse_error = rmux
        .session(session_name("r3513work"))
        .await
        .expect_err("session() must not bind a prefix of a live session");
    assert_session_not_found(&reuse_error, "r3513work");

    let literal = session_name("=r3513eq");
    let created = EnsureSession::named(literal.clone())
        .create_only()
        .ensure(&rmux)
        .await?;
    assert!(
        created.exists().await?,
        "a literal leading = is part of the name"
    );
    assert!(rmux.has_session(literal.clone()).await?);
    assert_eq!(rmux.session(literal.clone()).await?.name(), &literal);
    assert!(created.kill().await?);
    assert_eq!(
        raw_list_session_names(harness.socket_path()).await?,
        [worker]
    );
    harness.finish().await
}

#[tokio::test]
async fn exact_name_kill_never_reaches_a_longer_session() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("exact-kill").await?;
    let worker = session_name("r3513worker");
    let session = killed_prefix_session(&harness, &worker).await?;

    let stale_kill = session
        .kill()
        .await
        .expect_err("kill() of a missing exact name must not resolve a prefix");
    assert_session_not_found(&stale_kill, "r3513work");
    assert_eq!(
        raw_list_session_names(harness.socket_path()).await?,
        [worker],
        "kill() on r3513work destroyed r3513worker"
    );
    harness.finish().await
}

#[tokio::test]
async fn exact_name_exists_never_reports_a_longer_session() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("exact-exists").await?;
    let worker = session_name("r3513worker");
    let session = killed_prefix_session(&harness, &worker).await?;

    assert!(
        !session.exists().await?,
        "exists() of a killed session followed the prefix to r3513worker"
    );
    assert_eq!(
        raw_list_session_names(harness.socket_path()).await?,
        [worker]
    );
    harness.finish().await
}

#[tokio::test]
async fn exact_name_owned_replace_never_kills_a_longer_session() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("exact-replace").await?;
    let rmux = harness.rmux();
    let worker = session_name("r3513worker");
    let work = session_name("r3513work");
    raw_new_session(harness.socket_path(), worker.clone()).await?;

    let mut replaced = rmux
        .owned_session(work.clone())
        .replace_existing(true)
        .await?;
    assert_eq!(
        raw_list_session_names(harness.socket_path()).await?,
        [work, worker],
        "replace_existing() killed the session it only prefixes"
    );
    assert!(replaced.cleanup().await?);
    harness.finish().await
}

/// Leaves `worker` live beside a handle for `r3513work`, a prefix of it,
/// whose session was created and then killed through that handle.
async fn killed_prefix_session(harness: &Harness, worker: &SessionName) -> TestResult<Session> {
    let rmux = harness.rmux();
    raw_new_session(harness.socket_path(), worker.clone()).await?;
    let session = EnsureSession::named(session_name("r3513work"))
        .create_only()
        .ensure(&rmux)
        .await?;
    assert!(session.exists().await?);
    assert!(session.kill().await?);
    Ok(session)
}

#[tokio::test]
async fn exact_name_ensure_reports_creation_beside_a_longer_session() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("exact-ensure").await?;
    let rmux = harness.rmux();
    let worker = session_name("r3513worker");
    let work = session_name("r3513work");
    raw_new_session(harness.socket_path(), worker.clone()).await?;

    let ensured = EnsureSession::named(work.clone()).ensure(&rmux).await?;
    assert_eq!(ensured.name(), &work);
    assert!(
        ensured.was_created(),
        "ensure() reported a fresh session as reused because r3513worker exists"
    );
    let reused = EnsureSession::named(work.clone()).ensure(&rmux).await?;
    assert!(!reused.was_created());
    assert_eq!(
        raw_list_session_names(harness.socket_path()).await?,
        [work, worker]
    );
    harness.finish().await
}

#[tokio::test]
async fn exact_name_calls_keep_a_leading_dollar_in_the_name() -> TestResult {
    let _lock = LIVE_DAEMON_LOCK.lock().await;
    let harness = Harness::start("exact-dollar").await?;
    let rmux = harness.rmux();
    let first = session_name("r3513first");
    let dollar = session_name("$0");
    raw_new_session(harness.socket_path(), first.clone()).await?;

    assert!(
        !rmux.has_session(dollar.clone()).await?,
        "has_session($0) matched the session whose id is $0"
    );
    assert!(!rmux.has_session(session_name("$x")).await?);
    let created = EnsureSession::named(dollar.clone()).ensure(&rmux).await?;
    assert!(created.was_created());
    assert!(created.exists().await?);
    assert!(created.kill().await?);
    assert_eq!(
        raw_list_session_names(harness.socket_path()).await?,
        [first],
        "kill() on $0 destroyed the session whose id is $0"
    );
    let stale_kill = created
        .kill()
        .await
        .expect_err("kill() of a missing exact name must not resolve an id");
    assert_session_not_found(&stale_kill, "$0");
    harness.finish().await
}

/// Raw names whose canonical form contains a backslash: a literal one, and
/// the escapes of a tab, a control and a bidi override.
const BACKSLASH_NAMES: [&str; 4] = [
    "r3516back\\slash",
    "r3516tab\tx",
    "r3516ctl\u{1}x",
    "r3516\u{202e}x",
];

#[tokio::test]
async fn exact_name_calls_find_and_kill_a_name_with_a_backslash() -> TestResult {
    for raw in BACKSLASH_NAMES {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let harness = Harness::start("exact-backslash").await?;
        let rmux = harness.rmux();
        let name = session_name(raw);
        assert!(
            name.as_str().contains('\\'),
            "{raw:?} has a canonical backslash"
        );
        let longer = SessionName::from_canonical(format!("{}tail", name.as_str()))?;
        raw_new_session(harness.socket_path(), longer.clone()).await?;

        let created = EnsureSession::named(name.clone())
            .create_only()
            .ensure(&rmux)
            .await?;
        assert_eq!(created.name(), &name);
        assert!(
            rmux.has_session(name.clone()).await?,
            "has_session({raw:?})"
        );
        assert_eq!(rmux.session(name.clone()).await?.name(), &name, "{raw:?}");
        assert!(created.exists().await?, "exists({raw:?})");
        let reused = EnsureSession::named(name.clone()).ensure(&rmux).await?;
        assert!(
            !reused.was_created(),
            "ensure({raw:?}) recreated a live session"
        );

        assert!(created.kill().await?, "kill({raw:?})");
        assert_eq!(
            canonical_session_names(harness.socket_path()).await?,
            std::slice::from_ref(&longer),
            "kill({raw:?}) left the session or reached the longer one"
        );
        assert!(!rmux.has_session(name.clone()).await?, "{raw:?} is gone");
        assert!(!created.exists().await?, "{raw:?} is gone");
        let stale_kill = created
            .kill()
            .await
            .expect_err("kill() of a missing exact name must not resolve a prefix");
        assert_session_not_found(&stale_kill, name.as_str());
        harness.finish().await?;
    }
    Ok(())
}

#[tokio::test]
async fn exact_name_owned_replace_kills_a_live_name_with_a_backslash() -> TestResult {
    for raw in BACKSLASH_NAMES {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let harness = Harness::start("exact-backslash-replace").await?;
        let rmux = harness.rmux();
        let name = session_name(raw);
        let longer = SessionName::from_canonical(format!("{}tail", name.as_str()))?;
        raw_new_session(harness.socket_path(), longer.clone()).await?;
        raw_new_session(harness.socket_path(), name.clone()).await?;

        let mut replaced = rmux
            .owned_session(name.clone())
            .replace_existing(true)
            .await?;
        assert_eq!(
            canonical_session_names(harness.socket_path()).await?,
            [name.clone(), longer.clone()],
            "replace_existing({raw:?})"
        );
        assert!(replaced.cleanup().await?);
        assert_eq!(
            canonical_session_names(harness.socket_path()).await?,
            [longer]
        );
        harness.finish().await?;
    }
    Ok(())
}

/// Lists the live session names as the daemon renders them, without the
/// SDK and without escaping their backslashes again.
async fn canonical_session_names(socket_path: &Path) -> TestResult<Vec<SessionName>> {
    match framed_request(
        socket_path,
        Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: Some("name".to_owned()),
            reversed: false,
        }),
    )
    .await?
    {
        Response::ListSessions(response) => Ok(String::from_utf8_lossy(response.output.stdout())
            .lines()
            .map(SessionName::from_canonical)
            .collect::<Result<Vec<_>, _>>()?),
        response => Err(format!("unexpected list-sessions response: {response:?}").into()),
    }
}
