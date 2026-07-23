use std::time::Duration;

use tokio::time::Instant;

use crate::{PaneSnapshot, Result, RmuxError, WaitTimeoutError};

use super::{Locator, LocatorAssertionKind, LocatorMatch, LocatorState};

pub(super) async fn wait_for_locator_state(
    locator: Locator,
    state: LocatorState,
) -> Result<PaneSnapshot> {
    let (locator, timeout, deadline) = locator.begin_pinned_operation().await?;
    wait_until(
        locator,
        timeout,
        deadline,
        move |matches, _snapshot| match state {
            LocatorState::Visible => !matches.is_empty(),
            LocatorState::Hidden => matches.is_empty(),
        },
        format!("locator to be {state:?}"),
    )
    .await
}

pub(super) async fn wait_for_assertion(
    locator: Locator,
    kind: LocatorAssertionKind,
) -> Result<PaneSnapshot> {
    let (locator, timeout, deadline) = locator.begin_pinned_operation().await?;
    let description = assertion_description(&kind);
    let mut last_snapshot = None;
    loop {
        let snapshot = crate::wait::snapshot_with_wait_deadline(
            &locator.pane,
            "wait for locator snapshot",
            timeout,
            deadline,
            last_snapshot.as_ref(),
            || description.clone(),
        )
        .await?;
        let matches = locator.resolve(&snapshot).await?;
        match assertion_outcome(&matches, &kind) {
            AssertionOutcome::Matched => return Ok(snapshot),
            AssertionOutcome::Continue => {}
            AssertionOutcome::StrictViolation => {
                return Err(strict_locator_error(
                    matches.len(),
                    locator.describe(),
                    &snapshot,
                ));
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                description,
                timeout.expect("deadline implies timeout"),
                snapshot,
            )));
        }
        sleep_until_next_poll(deadline, locator.poll_interval).await;
        last_snapshot = Some(snapshot);
    }
}

async fn wait_until(
    locator: Locator,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
    predicate: impl Fn(&[LocatorMatch], &PaneSnapshot) -> bool,
    description: String,
) -> Result<PaneSnapshot> {
    let mut last_snapshot = None;
    loop {
        let snapshot = crate::wait::snapshot_with_wait_deadline(
            &locator.pane,
            "wait for locator snapshot",
            timeout,
            deadline,
            last_snapshot.as_ref(),
            || description.clone(),
        )
        .await?;
        let matches = locator.resolve(&snapshot).await?;
        if predicate(&matches, &snapshot) {
            return Ok(snapshot);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                description,
                timeout.expect("deadline implies timeout"),
                snapshot,
            )));
        }
        sleep_until_next_poll(deadline, locator.poll_interval).await;
        last_snapshot = Some(snapshot);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssertionOutcome {
    Matched,
    Continue,
    StrictViolation,
}

fn assertion_outcome(matches: &[LocatorMatch], kind: &LocatorAssertionKind) -> AssertionOutcome {
    match kind {
        LocatorAssertionKind::Visible => strict_unary_outcome(matches, |_| true),
        LocatorAssertionKind::Hidden => {
            if matches.is_empty() {
                AssertionOutcome::Matched
            } else {
                AssertionOutcome::Continue
            }
        }
        LocatorAssertionKind::ContainsText(text) => {
            strict_unary_outcome(matches, |item| item.text_match.text.contains(text))
        }
        LocatorAssertionKind::HasText(text) => {
            strict_unary_outcome(matches, |item| item.text_match.text == *text)
        }
        LocatorAssertionKind::Count(count) => {
            if matches.len() == *count {
                AssertionOutcome::Matched
            } else {
                AssertionOutcome::Continue
            }
        }
    }
}

fn strict_unary_outcome(
    matches: &[LocatorMatch],
    predicate: impl FnOnce(&LocatorMatch) -> bool,
) -> AssertionOutcome {
    match matches {
        [] => AssertionOutcome::Continue,
        [item] if predicate(item) => AssertionOutcome::Matched,
        [_] => AssertionOutcome::Continue,
        _ => AssertionOutcome::StrictViolation,
    }
}

fn assertion_description(kind: &LocatorAssertionKind) -> String {
    match kind {
        LocatorAssertionKind::Visible => "locator to be visible".to_owned(),
        LocatorAssertionKind::Hidden => "locator to be hidden".to_owned(),
        LocatorAssertionKind::ContainsText(text) => format!("locator to contain text `{text}`"),
        LocatorAssertionKind::HasText(text) => format!("locator to have text `{text}`"),
        LocatorAssertionKind::Count(count) => format!("locator to have count {count}"),
    }
}

pub(super) fn strict_locator_error(
    count: usize,
    query: String,
    snapshot: &PaneSnapshot,
) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "strict locator violation: expected 1 match, found {count}; locator: {query}; last visible screen:\n{}",
        snapshot.visible_text()
    )))
}

pub(super) async fn sleep_until_next_poll(deadline: Option<Instant>, poll_interval: Duration) {
    let Some(deadline) = deadline else {
        tokio::time::sleep(poll_interval).await;
        return;
    };
    let now = Instant::now();
    if now < deadline {
        tokio::time::sleep(poll_interval.min(deadline - now)).await;
    }
}
