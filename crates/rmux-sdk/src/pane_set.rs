//! Pane group helpers built from ordinary [`Pane`] handles.
//!
//! `PaneSet` is SDK-side composition. It does not add daemon-side batching or
//! atomic cross-pane ordering; it gives callers a small, typed surface for
//! common fan-out and fan-in workflows while preserving per-pane results.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use tokio::task::JoinSet;

use crate::{
    BroadcastResult, Input, Pane, PaneCloseOutcome, PaneId, PaneRef, PaneSnapshot, Result,
    RmuxError,
};

/// Owned group of pane handles.
#[derive(Debug, Clone, Default)]
#[must_use = "pane sets do nothing unless one of their async methods is awaited"]
pub struct PaneSet {
    panes: Vec<Pane>,
}

impl PaneSet {
    /// Creates a pane set from pane handles.
    ///
    /// Preserves caller order exactly. It does not deduplicate repeated pane
    /// handles and may contain panes from different daemon endpoints.
    pub fn new<I>(panes: I) -> Self
    where
        I: IntoIterator<Item = Pane>,
    {
        Self {
            panes: panes.into_iter().collect(),
        }
    }

    /// Returns the panes in their caller-provided order.
    #[must_use]
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// Returns the number of panes in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.panes.len()
    }

    /// Returns true when the set contains no panes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    /// Broadcasts text or one key token to every pane.
    ///
    /// This delegates to the client-side broadcast implementation and returns
    /// the same partial-broadcast error when at least one pane rejects the input.
    pub async fn broadcast(&self, input: Input<'_>) -> Result<BroadcastResult> {
        crate::broadcast::broadcast(&self.panes, input).await
    }

    /// Captures one fresh snapshot per pane.
    ///
    /// The returned batch always contains per-pane successes and failures.
    /// Call [`PaneSetBatch::is_success`] when the caller requires every pane
    /// to succeed.
    pub async fn snapshot_all(&self) -> PaneSetBatch<PaneSnapshot> {
        run_all(
            self.panes.clone(),
            |pane| async move { pane.snapshot().await },
        )
        .await
    }

    /// Closes every pane by consuming this pane set.
    ///
    /// Stale panes use the ordinary [`Pane::close`] idempotent semantics and
    /// return [`PaneCloseOutcome::AlreadyClosed`] as a success.
    pub async fn close_all(self) -> PaneSetBatch<PaneCloseOutcome> {
        close_all_in_order(self.panes).await
    }

    /// Starts an all-panes visible-text expectation builder.
    #[must_use]
    pub fn expect_all(&self) -> PaneSetExpectation<'_> {
        PaneSetExpectation {
            panes: &self.panes,
            mode: ExpectMode::All,
        }
    }

    /// Starts an any-pane visible-text expectation builder.
    #[must_use]
    pub fn expect_any(&self) -> PaneSetExpectation<'_> {
        PaneSetExpectation {
            panes: &self.panes,
            mode: ExpectMode::Any,
        }
    }

    /// Alias for [`Self::expect_all`].
    #[must_use]
    pub fn wait_all(&self) -> PaneSetExpectation<'_> {
        self.expect_all()
    }

    /// Alias for [`Self::expect_any`].
    #[must_use]
    pub fn wait_any(&self) -> PaneSetExpectation<'_> {
        self.expect_any()
    }
}

async fn close_all_in_order(panes: Vec<Pane>) -> PaneSetBatch<PaneCloseOutcome> {
    let mut successes = Vec::new();
    let mut failures = Vec::new();
    for pane in panes {
        let target = pane.target().clone();
        let pane_id = pane.id().await.ok().flatten();
        let pane = match pane_id {
            Some(pane_id) => pane.pin_to_id(pane_id),
            None => pane,
        };
        match pane.close().await {
            Ok(value) => successes.push(PaneSetSuccess::new(target, pane_id, value)),
            Err(error) => failures.push(PaneSetFailure::new(target, pane_id, error)),
        }
    }
    PaneSetBatch::new(successes, failures)
}

impl From<Vec<Pane>> for PaneSet {
    fn from(panes: Vec<Pane>) -> Self {
        Self { panes }
    }
}

impl FromIterator<Pane> for PaneSet {
    fn from_iter<T: IntoIterator<Item = Pane>>(iter: T) -> Self {
        Self::new(iter)
    }
}

impl IntoIterator for PaneSet {
    type Item = Pane;
    type IntoIter = std::vec::IntoIter<Pane>;

    fn into_iter(self) -> Self::IntoIter {
        self.panes.into_iter()
    }
}

/// Successful result for one pane in a [`PaneSet`] batch.
#[derive(Debug)]
pub struct PaneSetSuccess<T> {
    target: PaneRef,
    pane_id: Option<PaneId>,
    value: T,
}

impl<T> PaneSetSuccess<T> {
    fn new(target: PaneRef, pane_id: Option<PaneId>, value: T) -> Self {
        Self {
            target,
            pane_id,
            value,
        }
    }

    /// Returns the slot target observed before the operation.
    #[must_use]
    pub const fn target(&self) -> &PaneRef {
        &self.target
    }

    /// Returns the pane id observed before the operation, when available.
    #[must_use]
    pub const fn pane_id(&self) -> Option<PaneId> {
        self.pane_id
    }

    /// Returns the operation result value.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Consumes the success and returns the operation result value.
    pub fn into_value(self) -> T {
        self.value
    }
}

/// Failed result for one pane in a [`PaneSet`] batch.
#[derive(Debug)]
pub struct PaneSetFailure {
    target: PaneRef,
    pane_id: Option<PaneId>,
    error: RmuxError,
}

impl PaneSetFailure {
    fn new(target: PaneRef, pane_id: Option<PaneId>, error: RmuxError) -> Self {
        Self {
            target,
            pane_id,
            error,
        }
    }

    /// Returns the slot target observed before the operation.
    #[must_use]
    pub const fn target(&self) -> &PaneRef {
        &self.target
    }

    /// Returns the pane id observed before the operation, when available.
    #[must_use]
    pub const fn pane_id(&self) -> Option<PaneId> {
        self.pane_id
    }

    /// Returns the per-pane error.
    #[must_use]
    pub const fn error(&self) -> &RmuxError {
        &self.error
    }

    /// Consumes the failure and returns the per-pane error.
    pub fn into_error(self) -> RmuxError {
        self.error
    }
}

/// Per-pane results for a group operation that targets every pane.
#[derive(Debug)]
pub struct PaneSetBatch<T> {
    successes: Vec<PaneSetSuccess<T>>,
    failures: Vec<PaneSetFailure>,
}

impl<T> PaneSetBatch<T> {
    fn new(successes: Vec<PaneSetSuccess<T>>, failures: Vec<PaneSetFailure>) -> Self {
        Self {
            successes,
            failures,
        }
    }

    /// Returns true when every targeted pane succeeded.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.failures.is_empty()
    }

    /// Returns successful per-pane results.
    #[must_use]
    pub fn successes(&self) -> &[PaneSetSuccess<T>] {
        &self.successes
    }

    /// Returns failed per-pane results.
    #[must_use]
    pub fn failures(&self) -> &[PaneSetFailure] {
        &self.failures
    }

    /// Returns the total number of panes targeted by the batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.successes.len() + self.failures.len()
    }

    /// Returns true when the batch targeted no panes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.successes.is_empty() && self.failures.is_empty()
    }
}

/// Per-pane results for an any-pane wait.
#[derive(Debug)]
pub struct PaneSetAny<T> {
    success: Option<PaneSetSuccess<T>>,
    failures: Vec<PaneSetFailure>,
}

impl<T> PaneSetAny<T> {
    fn from_success(success: PaneSetSuccess<T>, failures: Vec<PaneSetFailure>) -> Self {
        Self {
            success: Some(success),
            failures,
        }
    }

    fn failure(failures: Vec<PaneSetFailure>) -> Self {
        Self {
            success: None,
            failures,
        }
    }

    /// Returns true when at least one pane satisfied the wait.
    #[must_use]
    pub fn matched(&self) -> bool {
        self.success.is_some()
    }

    /// Returns the successful pane result, if any.
    #[must_use]
    pub const fn success(&self) -> Option<&PaneSetSuccess<T>> {
        self.success.as_ref()
    }

    /// Returns failures observed before the first match, or all failures when
    /// no pane matched.
    #[must_use]
    pub fn failures(&self) -> &[PaneSetFailure] {
        &self.failures
    }
}

/// Visible-text expectation builder for a [`PaneSet`].
#[derive(Debug, Clone, Copy)]
pub struct PaneSetExpectation<'a> {
    panes: &'a [Pane],
    mode: ExpectMode,
}

impl<'a> PaneSetExpectation<'a> {
    /// Waits until visible text on the selected pane set contains any literal.
    pub fn visible_text_matches_any<I, S>(self, patterns: I) -> PaneSetVisibleTextWait<'a>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        PaneSetVisibleTextWait::new(
            self.panes,
            self.mode,
            VisibleSetMatcher::Any(patterns.into_iter().map(Into::into).collect()),
        )
    }

    /// Waits until visible text on the selected pane set contains all
    /// literals.
    pub fn visible_text_matches_all<I, S>(self, patterns: I) -> PaneSetVisibleTextWait<'a>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        PaneSetVisibleTextWait::new(
            self.panes,
            self.mode,
            VisibleSetMatcher::All(patterns.into_iter().map(Into::into).collect()),
        )
    }

    /// Waits until visible text on the selected pane set contains one
    /// literal.
    pub fn visible_text_contains(self, pattern: impl Into<String>) -> PaneSetVisibleTextWait<'a> {
        PaneSetVisibleTextWait::new(
            self.panes,
            self.mode,
            VisibleSetMatcher::Contains(pattern.into()),
        )
    }
}

/// Awaitable visible-text wait over a [`PaneSet`].
#[derive(Debug)]
#[must_use = "pane-set visible waits do nothing unless awaited"]
pub struct PaneSetVisibleTextWait<'a> {
    panes: &'a [Pane],
    mode: ExpectMode,
    matcher: VisibleSetMatcher,
    timeout: Option<Duration>,
    poll_interval: Option<Duration>,
}

impl<'a> PaneSetVisibleTextWait<'a> {
    fn new(panes: &'a [Pane], mode: ExpectMode, matcher: VisibleSetMatcher) -> Self {
        Self {
            panes,
            mode,
            matcher,
            timeout: None,
            poll_interval: None,
        }
    }

    /// Overrides the timeout used by each per-pane visible wait.
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Overrides the polling interval used by each per-pane visible wait.
    pub const fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = Some(interval);
        self
    }

    async fn run(self) -> PaneSetVisibleTextOutcome {
        match self.mode {
            ExpectMode::All => {
                let matcher = self.matcher;
                let timeout = self.timeout;
                let poll_interval = self.poll_interval;
                PaneSetVisibleTextOutcome::All(
                    run_all(self.panes.to_vec(), move |pane| {
                        let matcher = matcher.clone();
                        async move { wait_visible_text(pane, matcher, timeout, poll_interval).await }
                    })
                    .await,
                )
            }
            ExpectMode::Any => PaneSetVisibleTextOutcome::Any(self.run_any().await),
        }
    }

    async fn run_any(self) -> PaneSetAny<PaneSnapshot> {
        let mut tasks = JoinSet::new();
        for pane in self.panes.iter().cloned() {
            let matcher = self.matcher.clone();
            let timeout = self.timeout;
            let poll_interval = self.poll_interval;
            tasks.spawn(async move {
                let target = pane.target().clone();
                let pane_id = pane.id().await.ok().flatten();
                let result = wait_visible_text(pane, matcher, timeout, poll_interval).await;
                (target, pane_id, result)
            });
        }

        let mut failures = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (target, pane_id, result) = match joined {
                Ok(outcome) => outcome,
                Err(error) => {
                    failures.push(join_failure(error));
                    continue;
                }
            };
            match result {
                Ok(snapshot) => {
                    tasks.abort_all();
                    return PaneSetAny::from_success(
                        PaneSetSuccess::new(target, pane_id, snapshot),
                        failures,
                    );
                }
                Err(error) => failures.push(PaneSetFailure::new(target, pane_id, error)),
            }
        }
        PaneSetAny::failure(failures)
    }
}

impl<'a> IntoFuture for PaneSetVisibleTextWait<'a> {
    type Output = PaneSetVisibleTextOutcome;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

/// Result of awaiting a [`PaneSetVisibleTextWait`].
#[derive(Debug)]
#[non_exhaustive]
pub enum PaneSetVisibleTextOutcome {
    /// Result for an all-panes wait.
    All(PaneSetBatch<PaneSnapshot>),
    /// Result for an any-pane wait.
    Any(PaneSetAny<PaneSnapshot>),
}

impl PaneSetVisibleTextOutcome {
    /// Returns the all-panes batch when this outcome came from
    /// [`PaneSet::expect_all`] or [`PaneSet::wait_all`].
    #[must_use]
    pub const fn all(&self) -> Option<&PaneSetBatch<PaneSnapshot>> {
        match self {
            Self::All(batch) => Some(batch),
            Self::Any(_) => None,
        }
    }

    /// Returns the any-pane result when this outcome came from
    /// [`PaneSet::expect_any`] or [`PaneSet::wait_any`].
    #[must_use]
    pub const fn any(&self) -> Option<&PaneSetAny<PaneSnapshot>> {
        match self {
            Self::Any(result) => Some(result),
            Self::All(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpectMode {
    All,
    Any,
}

#[derive(Debug, Clone)]
enum VisibleSetMatcher {
    Contains(String),
    Any(Vec<String>),
    All(Vec<String>),
}

async fn wait_visible_text(
    pane: Pane,
    matcher: VisibleSetMatcher,
    timeout: Option<Duration>,
    poll_interval: Option<Duration>,
) -> Result<PaneSnapshot> {
    match matcher {
        VisibleSetMatcher::Contains(pattern) => {
            let wait = pane.expect_visible_text().to_contain(pattern);
            apply_visible_options(wait, timeout, poll_interval).await
        }
        VisibleSetMatcher::Any(patterns) => {
            let wait = pane.expect_visible_text().to_match_any(patterns);
            apply_visible_options(wait, timeout, poll_interval).await
        }
        VisibleSetMatcher::All(patterns) => {
            let wait = pane.expect_visible_text().to_match_all(patterns);
            apply_visible_options(wait, timeout, poll_interval).await
        }
    }
}

async fn apply_visible_options(
    mut wait: crate::VisibleTextWait<'_>,
    timeout: Option<Duration>,
    poll_interval: Option<Duration>,
) -> Result<PaneSnapshot> {
    if let Some(timeout) = timeout {
        wait = wait.timeout(timeout);
    }
    if let Some(poll_interval) = poll_interval {
        wait = wait.poll_interval(poll_interval);
    }
    wait.await
}

async fn run_all<T, Fut>(
    panes: Vec<Pane>,
    operation: impl Fn(Pane) -> Fut + Clone + Send + Sync + 'static,
) -> PaneSetBatch<T>
where
    T: Send + 'static,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    for (index, pane) in panes.into_iter().enumerate() {
        let operation = operation.clone();
        tasks.spawn(async move {
            let target = pane.target().clone();
            let pane_id = pane.id().await.ok().flatten();
            let result = operation(pane).await;
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

fn join_failure(error: tokio::task::JoinError) -> PaneSetFailure {
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
