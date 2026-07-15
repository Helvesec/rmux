//! Broadcast helpers for pane groups.
//!
//! Broadcast uses the daemon-side batch endpoint when every pane belongs to
//! the same resolved SDK endpoint. Delivery still does not claim simultaneous
//! cross-pane execution; callers get a typed partial-failure error when any
//! pane rejects the input.

use std::error::Error;
use std::fmt;

use tokio::task::JoinSet;

use crate::{Pane, PaneId, PaneRef, Result, RmuxError};
use rmux_proto::{PaneBroadcastInputRequest, Request, Response, CAPABILITY_SDK_PANE_BROADCAST};

/// Input that can be broadcast to many panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Input<'a> {
    /// Literal text bytes. No newline is appended.
    Text(&'a str),
    /// One tmux-compatible key token such as `Enter` or `Backspace`.
    Key(&'a str),
}

impl<'a> Input<'a> {
    /// Constructs literal text input.
    #[must_use]
    pub const fn text(value: &'a str) -> Self {
        Self::Text(value)
    }

    /// Constructs key-token input.
    #[must_use]
    pub const fn key(value: &'a str) -> Self {
        Self::Key(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnedInput {
    Text(String),
    Key(String),
}

impl From<Input<'_>> for OwnedInput {
    fn from(value: Input<'_>) -> Self {
        match value {
            Input::Text(value) => Self::Text(value.to_owned()),
            Input::Key(value) => Self::Key(value.to_owned()),
        }
    }
}

/// Successful broadcast delivery for one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastPaneSuccess {
    target: PaneRef,
    pane_id: Option<PaneId>,
}

impl BroadcastPaneSuccess {
    /// Returns the slot target observed for this pane handle.
    #[must_use]
    pub const fn target(&self) -> &PaneRef {
        &self.target
    }

    /// Returns the live pane id observed before delivery, when available.
    #[must_use]
    pub const fn pane_id(&self) -> Option<PaneId> {
        self.pane_id
    }
}

/// Failed broadcast delivery for one pane.
#[derive(Debug)]
pub struct BroadcastPaneFailure {
    target: PaneRef,
    pane_id: Option<PaneId>,
    error: RmuxError,
}

impl BroadcastPaneFailure {
    /// Returns the slot target observed for this pane handle.
    #[must_use]
    pub const fn target(&self) -> &PaneRef {
        &self.target
    }

    /// Returns the live pane id observed before delivery, when available.
    #[must_use]
    pub const fn pane_id(&self) -> Option<PaneId> {
        self.pane_id
    }

    /// Returns the per-pane delivery error.
    #[must_use]
    pub const fn error(&self) -> &RmuxError {
        &self.error
    }

    /// Consumes the failure and returns the per-pane delivery error.
    #[must_use]
    pub fn into_error(self) -> RmuxError {
        self.error
    }
}

/// Result returned when every pane accepted a broadcast input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastResult {
    successes: Vec<BroadcastPaneSuccess>,
}

impl BroadcastResult {
    /// Returns one success entry per targeted pane.
    #[must_use]
    pub fn successes(&self) -> &[BroadcastPaneSuccess] {
        &self.successes
    }

    /// Returns the number of panes that accepted the input.
    #[must_use]
    pub fn len(&self) -> usize {
        self.successes.len()
    }

    /// Returns `true` when the broadcast targeted no panes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.successes.is_empty()
    }
}

/// Error payload for a broadcast where at least one pane failed.
#[derive(Debug)]
pub struct PartialBroadcastFailure {
    successes: Vec<BroadcastPaneSuccess>,
    failures: Vec<BroadcastPaneFailure>,
}

impl PartialBroadcastFailure {
    pub(crate) fn new(
        successes: Vec<BroadcastPaneSuccess>,
        failures: Vec<BroadcastPaneFailure>,
    ) -> Self {
        Self {
            successes,
            failures,
        }
    }

    /// Returns panes that accepted the input before the partial failure was
    /// reported.
    #[must_use]
    pub fn successes(&self) -> &[BroadcastPaneSuccess] {
        &self.successes
    }

    /// Returns panes that rejected the input.
    #[must_use]
    pub fn failures(&self) -> &[BroadcastPaneFailure] {
        &self.failures
    }
}

impl fmt::Display for PartialBroadcastFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "broadcast failed for {} of {} panes",
            self.failures.len(),
            self.successes.len() + self.failures.len()
        )?;
        for (index, failure) in self.failures.iter().enumerate() {
            if index > 0 {
                writeln!(formatter)?;
            }
            write!(
                formatter,
                "{}. {}",
                index + 1,
                RenderBroadcastFailure(failure)
            )?;
        }
        Ok(())
    }
}

impl Error for PartialBroadcastFailure {}

pub(crate) async fn broadcast(panes: &[Pane], input: Input<'_>) -> Result<BroadcastResult> {
    if panes.is_empty() {
        return Ok(BroadcastResult {
            successes: Vec::new(),
        });
    }
    let panes = panes
        .iter()
        .map(Pane::begin_operation_handle)
        .collect::<Vec<_>>();
    if same_endpoint(&panes) {
        match broadcast_daemon_side(&panes, input).await {
            Ok(result) => return Ok(result),
            Err(error) if is_daemon_broadcast_unavailable(&error) => {}
            Err(error) => return Err(error),
        }
    }
    broadcast_client_side(&panes, input).await
}

async fn broadcast_daemon_side(panes: &[Pane], input: Input<'_>) -> Result<BroadcastResult> {
    crate::capabilities::require(panes[0].transport(), &[CAPABILITY_SDK_PANE_BROADCAST]).await?;
    let mut targets = Vec::with_capacity(panes.len());
    let mut request_to_original = Vec::with_capacity(panes.len());
    let mut indexed_failures = Vec::new();
    for (original_index, pane) in panes.iter().enumerate() {
        match pane.required_resolved_proto_target_ref().await {
            Ok(target) => {
                targets.push(target);
                request_to_original.push(original_index);
            }
            Err(error) => indexed_failures.push((
                original_index,
                BroadcastPaneFailure {
                    target: pane.target().clone(),
                    pane_id: None,
                    error,
                },
            )),
        }
    }

    if targets.is_empty() {
        return Err(partial_broadcast(Vec::new(), indexed_failures));
    }

    let requested_target_count = targets.len();
    let response = panes[0]
        .transport()
        .request(Request::PaneBroadcastInput(PaneBroadcastInputRequest {
            targets,
            keys: input_keys(input),
            literal: matches!(input, Input::Text(_)),
        }))
        .await?;

    let response = match response {
        Response::PaneBroadcastInput(response) => response,
        response => {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
                "rmux daemon sent `{}` response for `pane broadcast` request",
                response.command_name()
            ))));
        }
    };

    let mut indexed_successes = Vec::with_capacity(response.successes.len());
    let mut seen = vec![false; requested_target_count];
    for success in response.successes {
        let (request_index, original_index) =
            response_target_index(success.target_index, &request_to_original, "successful")?;
        if std::mem::replace(&mut seen[request_index], true) {
            return Err(duplicate_broadcast_outcome(success.target_index));
        }
        indexed_successes.push((
            original_index,
            BroadcastPaneSuccess {
                target: panes[original_index].target().clone(),
                pane_id: success.pane_id,
            },
        ));
    }
    for failure in response.failures {
        let (request_index, original_index) =
            response_target_index(failure.target_index, &request_to_original, "failed")?;
        if std::mem::replace(&mut seen[request_index], true) {
            return Err(duplicate_broadcast_outcome(failure.target_index));
        }
        indexed_failures.push((
            original_index,
            BroadcastPaneFailure {
                target: panes[original_index].target().clone(),
                pane_id: None,
                error: RmuxError::protocol(failure.error),
            },
        ));
    }

    if let Some(request_index) = seen.iter().position(|seen| !seen) {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon omitted pane broadcast outcome for target index {request_index}"
        ))));
    }

    indexed_successes.sort_by_key(|(index, _)| *index);
    indexed_failures.sort_by_key(|(index, _)| *index);
    let successes = indexed_successes
        .into_iter()
        .map(|(_, success)| success)
        .collect::<Vec<_>>();
    let failures = indexed_failures
        .into_iter()
        .map(|(_, failure)| failure)
        .collect::<Vec<_>>();

    if failures.is_empty() {
        Ok(BroadcastResult { successes })
    } else {
        Err(RmuxError::partial_broadcast(PartialBroadcastFailure::new(
            successes, failures,
        )))
    }
}

fn response_target_index(
    target_index: u32,
    request_to_original: &[usize],
    outcome: &str,
) -> Result<(usize, usize)> {
    let request_index = usize::try_from(target_index).map_err(|_| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon returned out-of-range {outcome} pane broadcast target index {target_index}"
        )))
    })?;
    let original_index = request_to_original
        .get(request_index)
        .copied()
        .ok_or_else(|| {
            RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon returned out-of-range {outcome} pane broadcast target index {target_index}"
        )))
        })?;
    Ok((request_index, original_index))
}

fn duplicate_broadcast_outcome(target_index: u32) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon returned duplicate pane broadcast outcome for target index {target_index}"
    )))
}

fn partial_broadcast(
    mut indexed_successes: Vec<(usize, BroadcastPaneSuccess)>,
    mut indexed_failures: Vec<(usize, BroadcastPaneFailure)>,
) -> RmuxError {
    indexed_successes.sort_by_key(|(index, _)| *index);
    indexed_failures.sort_by_key(|(index, _)| *index);
    RmuxError::partial_broadcast(PartialBroadcastFailure::new(
        indexed_successes
            .into_iter()
            .map(|(_, success)| success)
            .collect(),
        indexed_failures
            .into_iter()
            .map(|(_, failure)| failure)
            .collect(),
    ))
}

async fn broadcast_client_side(panes: &[Pane], input: Input<'_>) -> Result<BroadcastResult> {
    let input = OwnedInput::from(input);
    let mut tasks = JoinSet::new();
    for (index, pane) in panes.iter().cloned().enumerate() {
        let input = input.clone();
        tasks.spawn(async move { (index, send_one(pane, input).await) });
    }

    let mut outcomes = Vec::with_capacity(panes.len());
    while let Some(joined) = tasks.join_next().await {
        let (index, outcome) = joined.map_err(|error| {
            RmuxError::transport(
                "join broadcast worker task",
                std::io::Error::other(error.to_string()),
            )
        })?;
        outcomes.push((index, outcome));
    }
    outcomes.sort_by_key(|(index, _)| *index);

    let mut successes = Vec::new();
    let mut failures = Vec::new();
    for (_, outcome) in outcomes {
        match outcome {
            PaneBroadcastOutcome::Success(success) => successes.push(success),
            PaneBroadcastOutcome::Failure(failure) => failures.push(failure),
        }
    }

    if failures.is_empty() {
        Ok(BroadcastResult { successes })
    } else {
        Err(RmuxError::partial_broadcast(PartialBroadcastFailure::new(
            successes, failures,
        )))
    }
}

fn same_endpoint(panes: &[Pane]) -> bool {
    let Some(first) = panes.first() else {
        return true;
    };
    panes.iter().all(|pane| pane.endpoint() == first.endpoint())
}

fn input_keys(input: Input<'_>) -> Vec<String> {
    match input {
        Input::Text(text) => vec![text.to_owned()],
        Input::Key(key) => vec![key.to_owned()],
    }
}

fn is_daemon_broadcast_unavailable(error: &RmuxError) -> bool {
    if crate::capabilities::is_unavailable(error, CAPABILITY_SDK_PANE_BROADCAST) {
        return true;
    }
    matches!(error, RmuxError::Unsupported { .. })
}

async fn send_one(pane: Pane, input: OwnedInput) -> PaneBroadcastOutcome {
    let target = pane.target().clone();
    let pane_id = pane.id().await.ok().flatten();
    let pane = match pane_id {
        Some(pane_id) => pane.pin_to_id(pane_id),
        None => pane,
    };
    let result = match input {
        OwnedInput::Text(text) => pane.send_text(text).await,
        OwnedInput::Key(key) => pane.send_key(key).await,
    };

    match result {
        Ok(()) => PaneBroadcastOutcome::Success(BroadcastPaneSuccess { target, pane_id }),
        Err(error) => PaneBroadcastOutcome::Failure(BroadcastPaneFailure {
            target,
            pane_id,
            error,
        }),
    }
}

enum PaneBroadcastOutcome {
    Success(BroadcastPaneSuccess),
    Failure(BroadcastPaneFailure),
}

struct RenderBroadcastFailure<'a>(&'a BroadcastPaneFailure);

impl fmt::Display for RenderBroadcastFailure<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?} failed", self.0.target)?;
        if let Some(pane_id) = self.0.pane_id {
            write!(formatter, " ({pane_id})")?;
        }
        write!(formatter, ": {}", self.0.error)
    }
}

#[cfg(test)]
#[path = "broadcast_tests.rs"]
mod tests;
