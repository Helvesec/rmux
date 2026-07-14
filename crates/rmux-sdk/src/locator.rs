//! Terminal-native locators over visible pane snapshots.
//!
//! A locator is a retryable query against rendered terminal text. It does not
//! model a DOM tree and does not infer hidden input fields; every match comes
//! from the latest [`PaneSnapshot`] visible grid.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

use crate::{Pane, PaneSnapshot, PaneTextMatch, Result, RmuxError, WaitTimeoutError};

mod query;

/// State awaited by [`Locator::wait_for_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LocatorState {
    /// At least one visible text match exists.
    Visible,
    /// No visible text match exists.
    Hidden,
}

/// Text query accepted by [`Pane::get_by_text`](crate::Pane::get_by_text).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LocatorText {
    /// Literal text searched line-by-line in the rendered snapshot.
    Literal(String),
    /// Regular expression searched line-by-line in the rendered snapshot.
    #[cfg(feature = "regex")]
    Regex(String),
}

impl From<&str> for LocatorText {
    fn from(value: &str) -> Self {
        Self::Literal(value.to_owned())
    }
}

impl From<String> for LocatorText {
    fn from(value: String) -> Self {
        Self::Literal(value)
    }
}

#[cfg(feature = "regex")]
impl From<regex::Regex> for LocatorText {
    fn from(value: regex::Regex) -> Self {
        Self::Regex(value.as_str().to_owned())
    }
}

/// Additional constraints applied after the base locator query.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct LocatorFilter {
    /// Keep matches whose matched text contains this literal.
    pub has_text: Option<String>,
    /// Drop matches whose matched text contains this literal.
    pub has_not_text: Option<String>,
    /// `Some(true)` keeps visible matches.
    ///
    /// `Some(false)` is rejected because terminal snapshots cannot prove that
    /// a matched string is hidden; use [`LocatorState::Hidden`] or
    /// [`LocatorExpectation::to_be_hidden`] to wait for absence instead.
    pub visible: Option<bool>,
}

/// One resolved terminal locator match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocatorMatch {
    /// Text coordinates reported by the snapshot search.
    pub text_match: PaneTextMatch,
}

/// Terminal text locator bound to one pane.
#[derive(Debug, Clone)]
#[must_use = "locators do nothing unless an action, assertion, or wait is awaited"]
pub struct Locator {
    pane: Pane,
    query: LocatorQuery,
    selection: LocatorSelection,
    filters: LocatorFilter,
    timeout: Option<Duration>,
    poll_interval: Duration,
    invalid_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LocatorQuery {
    Text(LocatorText),
    Or(Box<LocatorQuery>, Box<LocatorQuery>),
    And(Box<LocatorQuery>, Box<LocatorQuery>),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
enum LocatorSelection {
    #[default]
    Strict,
    First,
    Last,
    Nth(usize),
}

impl Locator {
    pub(crate) fn get_by_text(pane: Pane, text: impl Into<LocatorText>) -> Self {
        Self::new(pane, LocatorQuery::Text(text.into()))
    }

    pub(crate) fn parse(pane: Pane, selector: impl AsRef<str>) -> Self {
        let selector = selector.as_ref();
        let text = selector.strip_prefix("text=").unwrap_or(selector);
        Self::get_by_text(pane, text)
    }

    fn new(pane: Pane, query: LocatorQuery) -> Self {
        Self {
            pane,
            query,
            selection: LocatorSelection::Strict,
            filters: LocatorFilter::default(),
            timeout: None,
            poll_interval: crate::wait::TEXT_POLL_INTERVAL,
            invalid_reason: None,
        }
    }

    /// Selects the first current match before applying strict actions.
    pub const fn first(mut self) -> Self {
        self.selection = LocatorSelection::First;
        self
    }

    /// Selects the last current match before applying strict actions.
    pub const fn last(mut self) -> Self {
        self.selection = LocatorSelection::Last;
        self
    }

    /// Selects the zero-based `index` match before applying strict actions.
    pub const fn nth(mut self, index: usize) -> Self {
        self.selection = LocatorSelection::Nth(index);
        self
    }

    /// Adds terminal-native text filters to this locator.
    pub fn filter(mut self, filter: LocatorFilter) -> Self {
        self.filters = filter;
        self
    }

    /// Creates a locator that matches either locator's text query.
    ///
    /// Both locators must target the same pane. If they do not, the mismatch
    /// is reported when the resulting locator is awaited. Composition accepts
    /// plain locators only; apply filters, selections, and timeout overrides to
    /// the combined locator.
    pub fn or(self, other: Self) -> Self {
        self.combine(other, LocatorCombiner::Or)
    }

    /// Creates a locator that keeps matches present in both text queries.
    ///
    /// Intersections are based on exact visible coordinates. Composition
    /// accepts plain locators only; apply filters, selections, and timeout
    /// overrides to the combined locator.
    pub fn and(self, other: Self) -> Self {
        self.combine(other, LocatorCombiner::And)
    }

    /// Overrides the timeout for waits and assertions derived from this locator.
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Overrides the snapshot polling interval for this locator.
    pub const fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Waits until this locator is visible.
    pub fn wait_for(self) -> LocatorWait {
        self.wait_for_state(LocatorState::Visible)
    }

    /// Waits until this locator reaches `state`.
    pub fn wait_for_state(self, state: LocatorState) -> LocatorWait {
        LocatorWait {
            locator: self,
            state,
        }
    }

    /// Starts locator assertions.
    pub fn expect(self) -> LocatorExpectation {
        LocatorExpectation { locator: self }
    }

    pub(crate) async fn resolve(&self, snapshot: &PaneSnapshot) -> Result<Vec<LocatorMatch>> {
        if let Some(reason) = &self.invalid_reason {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                reason.clone(),
            )));
        }
        let mut matches = query::evaluate_query(&self.query, snapshot)?;
        query::apply_filter(&mut matches, &self.filters)?;
        Ok(query::apply_selection(matches, self.selection))
    }

    pub(crate) async fn resolve_strict_with_wait(&self) -> Result<(PaneSnapshot, LocatorMatch)> {
        let timeout = self
            .timeout
            .or_else(|| crate::wait::resolved_wait_timeout(self.pane.configured_default_timeout()));
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut last_snapshot = None;
        loop {
            let snapshot = crate::wait::snapshot_with_wait_deadline(
                &self.pane,
                "wait for locator snapshot",
                timeout,
                deadline,
                last_snapshot.as_ref(),
                || format!("strict locator {}", self.describe()),
            )
            .await?;
            let matches = self.resolve(&snapshot).await?;
            match matches.len() {
                1 => {
                    let item = matches
                        .into_iter()
                        .next()
                        .expect("single match length guarantees one entry");
                    return Ok((snapshot, item));
                }
                0 => {}
                count => return Err(strict_locator_error(count, self.describe(), &snapshot)),
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                    format!("strict locator {}", self.describe()),
                    timeout.expect("deadline implies timeout"),
                    snapshot,
                )));
            }
            sleep_until_next_poll(deadline, self.poll_interval).await;
            last_snapshot = Some(snapshot);
        }
    }

    pub(crate) fn pane(&self) -> &Pane {
        &self.pane
    }

    fn combine(self, other: Self, combiner: LocatorCombiner) -> Self {
        let invalid_reason = if self.pane.target() != other.pane.target()
            || self.pane.endpoint() != other.pane.endpoint()
        {
            Some(format!(
                "locator combination requires the same pane endpoint and target, got {} and {}",
                self.pane.target().to_proto(),
                other.pane.target().to_proto()
            ))
        } else if let Some(reason) = self.invalid_reason.clone() {
            Some(reason)
        } else if let Some(reason) = other.invalid_reason.clone() {
            Some(reason)
        } else if !self.is_plain_combinable() || !other.is_plain_combinable() {
            Some(format!(
                "locator.{} only supports plain locators; apply first/last/nth, filters, timeout, or poll_interval after combining",
                combiner.name()
            ))
        } else {
            None
        };
        let query = match combiner {
            LocatorCombiner::Or => LocatorQuery::Or(Box::new(self.query), Box::new(other.query)),
            LocatorCombiner::And => LocatorQuery::And(Box::new(self.query), Box::new(other.query)),
        };
        Self {
            pane: self.pane,
            query,
            selection: LocatorSelection::Strict,
            filters: LocatorFilter::default(),
            timeout: None,
            poll_interval: crate::wait::TEXT_POLL_INTERVAL,
            invalid_reason,
        }
    }

    fn describe(&self) -> String {
        query::describe_query(&self.query)
    }

    fn is_plain_combinable(&self) -> bool {
        self.selection == LocatorSelection::Strict
            && self.filters == LocatorFilter::default()
            && self.timeout.is_none()
            && self.poll_interval == crate::wait::TEXT_POLL_INTERVAL
            && self.invalid_reason.is_none()
    }
}

#[derive(Debug, Clone, Copy)]
enum LocatorCombiner {
    Or,
    And,
}

impl LocatorCombiner {
    const fn name(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
        }
    }
}

impl Pane {
    /// Creates a terminal-native locator for visible literal or regex text.
    ///
    /// The locator evaluates against rendered pane snapshots; it does not
    /// model hidden controls or a DOM.
    pub fn get_by_text(&self, text: impl Into<LocatorText>) -> Locator {
        Locator::get_by_text(self.clone(), text)
    }

    /// Parses a small terminal locator selector.
    ///
    /// P3 supports `text=...`; other selectors are treated as literal text so
    /// callers do not accidentally opt into a fake CSS/DOM language.
    pub fn locator(&self, selector: impl AsRef<str>) -> Locator {
        Locator::parse(self.clone(), selector)
    }
}

/// Awaitable locator wait.
#[derive(Debug)]
#[must_use = "locator waits do nothing unless awaited"]
pub struct LocatorWait {
    locator: Locator,
    state: LocatorState,
}

impl LocatorWait {
    /// Overrides the timeout for this wait.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.locator.timeout = Some(timeout);
        self
    }

    async fn run(self) -> Result<PaneSnapshot> {
        wait_for_locator_state(self.locator, self.state).await
    }
}

impl IntoFuture for LocatorWait {
    type Output = Result<PaneSnapshot>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

/// Assertion builder for one locator.
#[derive(Debug)]
#[must_use = "locator assertions do nothing unless awaited"]
pub struct LocatorExpectation {
    locator: Locator,
}

impl LocatorExpectation {
    /// Asserts that exactly one match is visible.
    pub fn to_be_visible(self) -> LocatorAssertion {
        LocatorAssertion::new(self.locator, LocatorAssertionKind::Visible)
    }

    /// Asserts that no match is visible.
    pub fn to_be_hidden(self) -> LocatorAssertion {
        LocatorAssertion::new(self.locator, LocatorAssertionKind::Hidden)
    }

    /// Asserts that one strict match contains `text`.
    pub fn to_contain_text(self, text: impl Into<String>) -> LocatorAssertion {
        LocatorAssertion::new(
            self.locator,
            LocatorAssertionKind::ContainsText(text.into()),
        )
    }

    /// Asserts that one strict match has exactly `text`.
    pub fn to_have_text(self, text: impl Into<String>) -> LocatorAssertion {
        LocatorAssertion::new(self.locator, LocatorAssertionKind::HasText(text.into()))
    }

    /// Asserts the current match count.
    pub fn to_have_count(self, count: usize) -> LocatorAssertion {
        LocatorAssertion::new(self.locator, LocatorAssertionKind::Count(count))
    }
}

/// Awaitable locator assertion.
#[derive(Debug)]
#[must_use = "locator assertions do nothing unless awaited"]
pub struct LocatorAssertion {
    locator: Locator,
    kind: LocatorAssertionKind,
}

#[derive(Debug)]
enum LocatorAssertionKind {
    Visible,
    Hidden,
    ContainsText(String),
    HasText(String),
    Count(usize),
}

impl LocatorAssertion {
    fn new(locator: Locator, kind: LocatorAssertionKind) -> Self {
        Self { locator, kind }
    }

    /// Overrides the timeout for this assertion.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.locator.timeout = Some(timeout);
        self
    }

    async fn run(self) -> Result<PaneSnapshot> {
        wait_for_assertion(self.locator, self.kind).await
    }
}

impl IntoFuture for LocatorAssertion {
    type Output = Result<PaneSnapshot>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

async fn wait_for_locator_state(locator: Locator, state: LocatorState) -> Result<PaneSnapshot> {
    wait_until(
        locator,
        move |matches, _snapshot| match state {
            LocatorState::Visible => !matches.is_empty(),
            LocatorState::Hidden => matches.is_empty(),
        },
        format!("locator to be {state:?}"),
    )
    .await
}

async fn wait_for_assertion(locator: Locator, kind: LocatorAssertionKind) -> Result<PaneSnapshot> {
    let description = assertion_description(&kind);
    let timeout = locator
        .timeout
        .or_else(|| crate::wait::resolved_wait_timeout(locator.pane.configured_default_timeout()));
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
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
    predicate: impl Fn(&[LocatorMatch], &PaneSnapshot) -> bool,
    description: String,
) -> Result<PaneSnapshot> {
    let timeout = locator
        .timeout
        .or_else(|| crate::wait::resolved_wait_timeout(locator.pane.configured_default_timeout()));
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
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

fn strict_locator_error(count: usize, query: String, snapshot: &PaneSnapshot) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "strict locator violation: expected 1 match, found {count}; locator: {query}; last visible screen:\n{}",
        snapshot.visible_text()
    )))
}

async fn sleep_until_next_poll(deadline: Option<Instant>, poll_interval: Duration) {
    let Some(deadline) = deadline else {
        tokio::time::sleep(poll_interval).await;
        return;
    };
    let now = Instant::now();
    if now < deadline {
        tokio::time::sleep(poll_interval.min(deadline - now)).await;
    }
}
