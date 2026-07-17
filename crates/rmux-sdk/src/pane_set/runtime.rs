use std::future::Future;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::{Pane, PaneId, PaneRef, PaneSnapshot, Result, RmuxError};

use super::{PaneSetBatch, PaneSetFailure, PaneSetSuccess, VisibleSetMatcher};

type PaneTaskOutcome<T> = (PaneRef, Option<PaneId>, Result<T>);

pub(super) async fn wait_visible_text_for_pane(
    pane: Pane,
    matcher: VisibleSetMatcher,
    timeout_override: Option<Duration>,
    poll_interval: Option<Duration>,
) -> PaneTaskOutcome<PaneSnapshot> {
    let target = pane.target().clone();
    let timeout = crate::wait::resolved_wait_timeout_override(
        timeout_override,
        pane.configured_default_timeout(),
    );
    let deadline = crate::wait::wait_deadline(timeout);
    let (pane, pane_id) = match crate::wait::with_wait_deadline(
        crate::wait::WAIT_FOR_TEXT_OPERATION,
        timeout,
        deadline,
        pane.pin_to_current_identity(),
    )
    .await
    {
        Ok(identity) => identity,
        Err(error) if crate::wait::is_wait_deadline_error(&error) => {
            return (target, None, Err(error));
        }
        Err(error) => return (target, None, Err(error)),
    };
    let result = wait_visible_text(pane, matcher, timeout, deadline, poll_interval).await;
    (target, pane_id, result)
}

async fn wait_visible_text(
    pane: Pane,
    matcher: VisibleSetMatcher,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
    poll_interval: Option<Duration>,
) -> Result<PaneSnapshot> {
    match matcher {
        VisibleSetMatcher::Contains(pattern) => {
            let wait = pane.expect_visible_text().to_contain(pattern);
            apply_visible_options(wait, timeout, deadline, poll_interval).await
        }
        VisibleSetMatcher::Any(patterns) => {
            let wait = pane.expect_visible_text().to_match_any(patterns);
            apply_visible_options(wait, timeout, deadline, poll_interval).await
        }
        VisibleSetMatcher::All(patterns) => {
            let wait = pane.expect_visible_text().to_match_all(patterns);
            apply_visible_options(wait, timeout, deadline, poll_interval).await
        }
    }
}

async fn apply_visible_options(
    mut wait: crate::VisibleTextWait<'_>,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
    poll_interval: Option<Duration>,
) -> Result<PaneSnapshot> {
    if let Some(poll_interval) = poll_interval {
        wait = wait.poll_interval(poll_interval);
    }
    wait.run_with_deadline(timeout, deadline).await
}

pub(super) async fn run_all<T, Fut>(
    panes: Vec<Pane>,
    operation: impl Fn(Pane) -> Fut + Clone + Send + Sync + 'static,
) -> PaneSetBatch<T>
where
    T: Send + 'static,
    Fut: Future<Output = PaneTaskOutcome<T>> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    for (index, pane) in panes.into_iter().enumerate() {
        let operation = operation.clone();
        tasks.spawn(async move {
            let (target, pane_id, result) = operation(pane).await;
            (index, target, pane_id, result)
        });
    }

    let mut outcomes = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => outcomes.push((
                usize::MAX,
                PaneRef::new(
                    crate::SessionName::new("unknown").expect("static session name"),
                    0,
                    0,
                ),
                None,
                Err(RmuxError::transport(
                    "join pane-set worker task",
                    std::io::Error::other(error.to_string()),
                )),
            )),
        }
    }
    outcomes.sort_by_key(|(index, _, _, _)| *index);

    let mut successes = Vec::new();
    let mut failures = Vec::new();
    for (_, target, pane_id, result) in outcomes {
        match result {
            Ok(value) => successes.push(PaneSetSuccess::new(target, pane_id, value)),
            Err(error) => failures.push(PaneSetFailure::new(target, pane_id, error)),
        }
    }
    PaneSetBatch::new(successes, failures)
}

pub(super) fn join_failure(error: tokio::task::JoinError) -> PaneSetFailure {
    PaneSetFailure::new(
        PaneRef::new(
            crate::SessionName::new("unknown").expect("static session name"),
            0,
            0,
        ),
        None,
        RmuxError::transport(
            "join pane-set worker task",
            std::io::Error::other(error.to_string()),
        ),
    )
}
