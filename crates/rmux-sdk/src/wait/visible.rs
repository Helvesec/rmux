//! Playwright-style visible text waits built on pane snapshots.

use std::error::Error;
use std::fmt;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use rmux_proto::RmuxError as ProtoError;
use tokio::time::Instant;

use crate::{Pane, PaneSnapshot, Result, RmuxError};

use super::{
    resolved_wait_timeout_override, snapshot_with_wait_deadline, wait_deadline, TEXT_POLL_INTERVAL,
    WAIT_FOR_TEXT_OPERATION,
};

#[cfg(feature = "regex")]
const REGEX_SIZE_LIMIT: usize = 1_000_000;

/// Entry point for visible text assertions on one pane.
#[derive(Debug, Clone, Copy)]
pub struct VisibleTextExpectation<'a> {
    pane: &'a Pane,
}

impl<'a> VisibleTextExpectation<'a> {
    pub(crate) const fn new(pane: &'a Pane) -> Self {
        Self { pane }
    }

    /// Waits until the visible screen text contains `needle`.
    pub fn to_contain(self, needle: impl Into<String>) -> VisibleTextWait<'a> {
        VisibleTextWait::new(self.pane, VisibleTextMatcherSpec::Contains(needle.into()))
    }

    /// Waits until the visible screen text does not contain `needle`.
    ///
    /// Negative waits can pass before a process has printed anything. Prefer
    /// anchoring the workflow with a positive wait first when testing a TUI
    /// transition.
    pub fn not_to_contain(self, needle: impl Into<String>) -> VisibleTextWait<'a> {
        VisibleTextWait::new(
            self.pane,
            VisibleTextMatcherSpec::NotContains(needle.into()),
        )
    }

    /// Waits until any supplied literal is present in the visible screen text.
    pub fn to_match_any<I, S>(self, needles: I) -> VisibleTextWait<'a>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        VisibleTextWait::new(
            self.pane,
            VisibleTextMatcherSpec::Any(needles.into_iter().map(Into::into).collect()),
        )
    }

    /// Waits until all supplied literals are present in the visible screen
    /// text.
    pub fn to_match_all<I, S>(self, needles: I) -> VisibleTextWait<'a>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        VisibleTextWait::new(
            self.pane,
            VisibleTextMatcherSpec::All(needles.into_iter().map(Into::into).collect()),
        )
    }

    /// Waits until the visible screen text matches a regular expression.
    ///
    /// Available with `rmux-sdk` feature `regex`.
    #[cfg(feature = "regex")]
    pub fn to_match(self, pattern: impl Into<String>) -> VisibleTextWait<'a> {
        self.to_match_regex(pattern)
    }

    /// Waits until the visible screen text matches a regular expression.
    ///
    /// Available with `rmux-sdk` feature `regex`.
    #[cfg(feature = "regex")]
    pub fn to_match_regex(self, pattern: impl Into<String>) -> VisibleTextWait<'a> {
        VisibleTextWait::new(self.pane, VisibleTextMatcherSpec::Regex(pattern.into()))
    }

    /// Waits until the visible screen text does not match a regular
    /// expression.
    ///
    /// Available with `rmux-sdk` feature `regex`. Like other negative waits,
    /// this can pass before the process has printed anything; use a preceding
    /// positive wait when absence must be checked after a known state change.
    #[cfg(feature = "regex")]
    pub fn not_to_match_regex(self, pattern: impl Into<String>) -> VisibleTextWait<'a> {
        VisibleTextWait::new(self.pane, VisibleTextMatcherSpec::NotRegex(pattern.into()))
    }

    /// Waits until any supplied regular expression matches the visible screen.
    ///
    /// Available with `rmux-sdk` feature `regex`.
    #[cfg(feature = "regex")]
    pub fn to_match_any_regex<I, S>(self, patterns: I) -> VisibleTextWait<'a>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        VisibleTextWait::new(
            self.pane,
            VisibleTextMatcherSpec::RegexAny(patterns.into_iter().map(Into::into).collect()),
        )
    }

    /// Waits until all supplied regular expressions match the visible screen.
    ///
    /// Available with `rmux-sdk` feature `regex`.
    #[cfg(feature = "regex")]
    pub fn to_match_all_regex<I, S>(self, patterns: I) -> VisibleTextWait<'a>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        VisibleTextWait::new(
            self.pane,
            VisibleTextMatcherSpec::RegexAll(patterns.into_iter().map(Into::into).collect()),
        )
    }
}

/// Awaitable visible text wait builder.
#[derive(Debug)]
#[must_use = "visible text waits do nothing unless awaited"]
pub struct VisibleTextWait<'a> {
    pane: &'a Pane,
    matcher: VisibleTextMatcherSpec,
    timeout: Option<Duration>,
    poll_interval: Duration,
}

impl<'a> VisibleTextWait<'a> {
    fn new(pane: &'a Pane, matcher: VisibleTextMatcherSpec) -> Self {
        Self {
            pane,
            matcher,
            timeout: None,
            poll_interval: TEXT_POLL_INTERVAL,
        }
    }

    /// Overrides the timeout for this wait.
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Overrides the snapshot polling interval for this wait.
    pub const fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    async fn run(self) -> Result<PaneSnapshot> {
        let timeout =
            resolved_wait_timeout_override(self.timeout, self.pane.configured_default_timeout());
        let deadline = wait_deadline(timeout);
        self.run_with_deadline(timeout, deadline).await
    }

    pub(crate) async fn run_with_deadline(
        self,
        timeout: Option<Duration>,
        deadline: Option<Instant>,
    ) -> Result<PaneSnapshot> {
        let matcher = self.matcher.compile()?;
        let pane = self.pane.begin_operation_handle_with_timeout(self.timeout);
        let pane = if pane.is_stable_id() {
            pane
        } else {
            super::with_wait_deadline(
                WAIT_FOR_TEXT_OPERATION,
                timeout,
                deadline,
                pane.pin_to_current_identity(),
            )
            .await?
            .0
        };
        let mut last_snapshot = None;
        loop {
            let snapshot = snapshot_with_wait_deadline(
                &pane,
                WAIT_FOR_TEXT_OPERATION,
                timeout,
                deadline,
                last_snapshot.as_ref(),
                || matcher.describe(),
            )
            .await?;
            if matcher.matches(&snapshot.visible_text()) {
                return Ok(snapshot);
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                    matcher.describe(),
                    timeout.expect("deadline implies timeout"),
                    snapshot,
                )));
            }

            sleep_until_next_poll(deadline, self.poll_interval).await;

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                    matcher.describe(),
                    timeout.expect("deadline implies timeout"),
                    snapshot,
                )));
            }
            last_snapshot = Some(snapshot);
        }
    }
}

impl<'a> IntoFuture for VisibleTextWait<'a> {
    type Output = Result<PaneSnapshot>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

#[derive(Debug)]
enum VisibleTextMatcherSpec {
    Contains(String),
    NotContains(String),
    Any(Vec<String>),
    All(Vec<String>),
    #[cfg(feature = "regex")]
    Regex(String),
    #[cfg(feature = "regex")]
    NotRegex(String),
    #[cfg(feature = "regex")]
    RegexAny(Vec<String>),
    #[cfg(feature = "regex")]
    RegexAll(Vec<String>),
}

impl VisibleTextMatcherSpec {
    fn compile(self) -> Result<VisibleTextMatcher> {
        let invalid = match self {
            Self::Contains(ref value) | Self::NotContains(ref value) => value.is_empty(),
            Self::Any(ref values) | Self::All(ref values) => {
                values.is_empty() || values.iter().any(String::is_empty)
            }
            #[cfg(feature = "regex")]
            Self::Regex(ref value) | Self::NotRegex(ref value) => value.is_empty(),
            #[cfg(feature = "regex")]
            Self::RegexAny(ref values) | Self::RegexAll(ref values) => {
                values.is_empty() || values.iter().any(String::is_empty)
            }
        };
        if invalid {
            return Err(RmuxError::protocol(ProtoError::Server(
                "visible text wait patterns must not be empty".to_owned(),
            )));
        }

        match self {
            Self::Contains(value) => Ok(VisibleTextMatcher::Contains(value)),
            Self::NotContains(value) => Ok(VisibleTextMatcher::NotContains(value)),
            Self::Any(values) => Ok(VisibleTextMatcher::Any(values)),
            Self::All(values) => Ok(VisibleTextMatcher::All(values)),
            #[cfg(feature = "regex")]
            Self::Regex(pattern) => Ok(VisibleTextMatcher::Regex(compile_regex(pattern)?)),
            #[cfg(feature = "regex")]
            Self::NotRegex(pattern) => Ok(VisibleTextMatcher::NotRegex(compile_regex(pattern)?)),
            #[cfg(feature = "regex")]
            Self::RegexAny(patterns) => compile_regexes(patterns).map(VisibleTextMatcher::RegexAny),
            #[cfg(feature = "regex")]
            Self::RegexAll(patterns) => compile_regexes(patterns).map(VisibleTextMatcher::RegexAll),
        }
    }
}

#[derive(Debug)]
enum VisibleTextMatcher {
    Contains(String),
    NotContains(String),
    Any(Vec<String>),
    All(Vec<String>),
    #[cfg(feature = "regex")]
    Regex(regex::Regex),
    #[cfg(feature = "regex")]
    NotRegex(regex::Regex),
    #[cfg(feature = "regex")]
    RegexAny(Vec<regex::Regex>),
    #[cfg(feature = "regex")]
    RegexAll(Vec<regex::Regex>),
}

impl VisibleTextMatcher {
    fn matches(&self, visible_text: &str) -> bool {
        match self {
            Self::Contains(value) => visible_text.contains(value),
            Self::NotContains(value) => !visible_text.contains(value),
            Self::Any(values) => values.iter().any(|value| visible_text.contains(value)),
            Self::All(values) => values.iter().all(|value| visible_text.contains(value)),
            #[cfg(feature = "regex")]
            Self::Regex(pattern) => pattern.is_match(visible_text),
            #[cfg(feature = "regex")]
            Self::NotRegex(pattern) => !pattern.is_match(visible_text),
            #[cfg(feature = "regex")]
            Self::RegexAny(patterns) => patterns
                .iter()
                .any(|pattern| pattern.is_match(visible_text)),
            #[cfg(feature = "regex")]
            Self::RegexAll(patterns) => patterns
                .iter()
                .all(|pattern| pattern.is_match(visible_text)),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Contains(value) => format!("contain `{value}`"),
            Self::NotContains(value) => format!("not contain `{value}`"),
            Self::Any(values) => format!("match any of {}", render_patterns(values)),
            Self::All(values) => format!("match all of {}", render_patterns(values)),
            #[cfg(feature = "regex")]
            Self::Regex(pattern) => format!("match regex `{}`", pattern.as_str()),
            #[cfg(feature = "regex")]
            Self::NotRegex(pattern) => format!("not match regex `{}`", pattern.as_str()),
            #[cfg(feature = "regex")]
            Self::RegexAny(patterns) => {
                format!("match any regex of {}", render_regex_patterns(patterns))
            }
            #[cfg(feature = "regex")]
            Self::RegexAll(patterns) => {
                format!("match all regex of {}", render_regex_patterns(patterns))
            }
        }
    }
}

/// Timeout details for a visible text wait.
#[derive(Debug)]
pub struct WaitTimeoutError {
    matcher: String,
    timeout: Duration,
    last_snapshot: PaneSnapshot,
}

impl WaitTimeoutError {
    pub(crate) fn new(matcher: String, timeout: Duration, last_snapshot: PaneSnapshot) -> Self {
        Self {
            matcher,
            timeout,
            last_snapshot,
        }
    }

    /// Returns the matcher description that did not become true.
    #[must_use]
    pub fn matcher(&self) -> &str {
        &self.matcher
    }

    /// Returns the timeout duration that elapsed.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns the last visible snapshot captured before timeout.
    #[must_use]
    pub const fn last_snapshot(&self) -> &PaneSnapshot {
        &self.last_snapshot
    }

    /// Returns the last visible screen text captured before timeout.
    #[must_use]
    pub fn last_visible_text(&self) -> String {
        self.last_snapshot.visible_text()
    }
}

impl fmt::Display for WaitTimeoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "timed out after {}s waiting for visible text to {}; last visible screen:\n{}",
            self.timeout.as_secs_f32(),
            self.matcher,
            self.last_snapshot.visible_text()
        )
    }
}

impl Error for WaitTimeoutError {}

async fn sleep_until_next_poll(deadline: Option<Instant>, poll_interval: Duration) {
    let Some(deadline) = deadline else {
        tokio::time::sleep(poll_interval).await;
        return;
    };

    let now = Instant::now();
    if now >= deadline {
        return;
    }
    tokio::time::sleep(poll_interval.min(deadline - now)).await;
}

fn render_patterns(patterns: &[String]) -> String {
    patterns
        .iter()
        .map(|pattern| format!("`{pattern}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(feature = "regex")]
fn compile_regex(pattern: String) -> Result<regex::Regex> {
    regex::RegexBuilder::new(&pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|error| RmuxError::invalid_regex(pattern, error.to_string()))
}

#[cfg(feature = "regex")]
fn compile_regexes(patterns: Vec<String>) -> Result<Vec<regex::Regex>> {
    patterns.into_iter().map(compile_regex).collect()
}

#[cfg(feature = "regex")]
fn render_regex_patterns(patterns: &[regex::Regex]) -> String {
    patterns
        .iter()
        .map(|pattern| format!("`{}`", pattern.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}
