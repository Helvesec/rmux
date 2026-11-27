use std::path::Path;
use std::time::{Duration, Instant};

use rmux_bootstrap::deadline::StartupDeadline;
use rmux_client::Connection;
use rmux_proto::{PaneOutputSubscriptionId, PaneOutputSubscriptionStart};
use serde_json::{json, Value};

use crate::cli_args::{SendKeysArgs, SendKeysWaitMode, WaitPaneArgs};
use crate::cli_response::expect_command_success;

use super::super::ExitFailure;
use super::common::{
    check_disabled, connect_wait, duration_millis, elapsed_millis, find_visible_text,
    timeout_deadline, visible_text, write_json, DEFAULT_STABLE_FOR,
};
use super::pane_exit::PaneExitStatus;
use super::stream;
use super::wait_target::{self, StableWaitProcessState, StableWaitTarget};

mod send_keys;
use send_keys::{
    send_keys_target_plan, send_keys_through_command_path, unresolved_wait_target_error,
};

const PANE_EXIT_TOMBSTONE_HANDOFF_GRACE: Duration = Duration::from_millis(100);

#[cfg(all(test, unix))]
pub(crate) fn run_wait_pane(args: WaitPaneArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    run_wait_pane_with_deadline(args, socket_path, None)
}

pub(crate) fn run_wait_pane_with_deadline(
    args: WaitPaneArgs,
    socket_path: &Path,
    deadline: Option<StartupDeadline>,
) -> Result<i32, ExitFailure> {
    match run_wait_pane_inner(args.clone(), socket_path, deadline) {
        Err(error) if error.is_timeout() => write_timeout_result(
            args.json,
            WaitCondition::from_wait_pane(&args).name(),
            args.target.as_ref().map_or("", |target| target.raw()),
            args.timeout.unwrap_or(super::common::DEFAULT_TIMEOUT),
        ),
        result => result,
    }
}

fn run_wait_pane_inner(
    args: WaitPaneArgs,
    socket_path: &Path,
    deadline: Option<StartupDeadline>,
) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_WAIT_PANE", "wait-pane")?;
    let started_at = Instant::now();
    let deadline = deadline.unwrap_or_else(|| timeout_deadline(args.timeout));
    let condition = WaitCondition::from_wait_pane(&args);
    let mut connection = connect_wait(socket_path, deadline)?;
    let target = wait_target::resolve(&mut connection, args.target.as_ref(), "wait-pane")?;

    match wait_condition(&mut connection, target.clone(), &condition, deadline, None)? {
        WaitCompletion::Matched { pane_exit } => {
            if args.json {
                let pane_exit = pane_exit.map_or(Value::Null, PaneExitStatus::json_value);
                return write_json(&json!({
                    "schema_version": 1,
                    "ok": true,
                    "condition": condition.name(),
                    "target": target_name(&target),
                    "elapsed_ms": elapsed_millis(started_at),
                    "pane_exit": pane_exit,
                }));
            }
            Ok(0)
        }
        WaitCompletion::TimedOut => write_timeout_result(
            args.json,
            condition.name(),
            &target_name(&target),
            args.timeout.unwrap_or(super::common::DEFAULT_TIMEOUT),
        ),
        WaitCompletion::Lag { missed_events } => write_lag_result(
            args.json,
            condition.name(),
            &target_name(&target),
            missed_events,
        ),
    }
}

pub(crate) fn run_send_keys_with_wait(
    args: SendKeysArgs,
    socket_path: &Path,
    deadline: Option<StartupDeadline>,
) -> Result<i32, ExitFailure> {
    let started_at = Instant::now();
    match run_send_keys_with_wait_inner(args.clone(), socket_path, started_at, deadline) {
        Err(error) if error.is_timeout() => Err(timeout_error(
            WaitCondition::from_send_keys(&args)?.name(),
            started_at,
        )),
        result => result,
    }
}

fn run_send_keys_with_wait_inner(
    args: SendKeysArgs,
    socket_path: &Path,
    started_at: Instant,
    deadline: Option<StartupDeadline>,
) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_SEND_KEYS_WAIT", "send-keys --wait")?;
    let deadline = deadline.unwrap_or_else(|| timeout_deadline(args.timeout));
    let condition = WaitCondition::from_send_keys(&args)?;
    let mut send_connection = connect_wait(socket_path, deadline)?;
    let mut target_plan = send_keys_target_plan(&mut send_connection, &args)?;
    if target_plan.wait_target.is_none() {
        return Err(unresolved_wait_target_error(&args));
    }

    if let WaitCondition::NextText(bytes) = &condition {
        let mut wait_target = target_plan
            .wait_target
            .clone()
            .expect("send-keys --wait target was preflighted");
        let mut wait_connection = connect_wait(socket_path, deadline)?;
        let wait_target_ref =
            wait_target.refreshed_target_ref(&mut wait_connection, "send-keys")?;
        let subscription_id = stream::subscribe(
            &mut wait_connection,
            wait_target_ref,
            PaneOutputSubscriptionStart::Now,
        )?;
        let send_response = match send_keys_through_command_path(
            &mut send_connection,
            args,
            target_plan.send_target,
        ) {
            Ok(response) => response,
            Err(error) => {
                let _ = wait_connection.unsubscribe_pane_output(subscription_id);
                return Err(error);
            }
        };
        if let Err(error) = expect_command_success(send_response, "send-keys") {
            let _ = wait_connection.unsubscribe_pane_output(subscription_id);
            return Err(error);
        }
        let result = wait_next_text_subscription(
            &mut wait_connection,
            subscription_id,
            bytes.as_bytes(),
            deadline,
        );
        let _ = wait_connection.unsubscribe_pane_output(subscription_id);
        return match result? {
            WaitCompletion::Matched { .. } => Ok(0),
            WaitCompletion::TimedOut => Err(timeout_error(condition.name(), started_at)),
            WaitCompletion::Lag { missed_events } => Err(next_text_lag_error(missed_events)),
        };
    }

    let baseline_revision = if matches!(condition, WaitCondition::Quiet(_)) {
        target_plan
            .wait_target
            .as_mut()
            .map(|target| {
                wait_target::snapshot(&mut send_connection, target)
                    .map(|snapshot| snapshot.revision)
            })
            .transpose()?
    } else {
        None
    };
    let send_response =
        send_keys_through_command_path(&mut send_connection, args, target_plan.send_target)?;
    expect_command_success(send_response, "send-keys")?;
    let wait_target = target_plan
        .wait_target
        .expect("send-keys --wait target was preflighted");

    match wait_condition(
        &mut send_connection,
        wait_target,
        &condition,
        deadline,
        baseline_revision,
    )? {
        WaitCompletion::Matched { pane_exit } => send_keys_wait_exit_code(&condition, pane_exit),
        WaitCompletion::TimedOut => Err(timeout_error(condition.name(), started_at)),
        WaitCompletion::Lag { missed_events } => Err(next_text_lag_error(missed_events)),
    }
}

fn send_keys_wait_exit_code(
    condition: &WaitCondition,
    pane_exit: Option<PaneExitStatus>,
) -> Result<i32, ExitFailure> {
    if !matches!(condition, WaitCondition::PaneExit) {
        return Ok(0);
    }
    PaneExitStatus::send_keys_exit_code(pane_exit)
}

mod outcome;
#[cfg(test)]
use outcome::lag_json_value;
use outcome::{next_text_lag_error, timeout_error, write_lag_result, write_timeout_result};

pub(super) fn invocation_timeout(
    command: &crate::cli_args::Command,
    started_at: Instant,
) -> Result<i32, ExitFailure> {
    match command {
        crate::cli_args::Command::WaitPane(args) => write_timeout_result(
            args.json,
            WaitCondition::from_wait_pane(args).name(),
            args.target.as_ref().map_or("", |target| target.raw()),
            args.timeout.unwrap_or(super::common::DEFAULT_TIMEOUT),
        ),
        crate::cli_args::Command::SendKeys(args) => Err(timeout_error(
            WaitCondition::from_send_keys(args)?.name(),
            started_at,
        )),
        _ => unreachable!("only wait commands have an invocation budget"),
    }
}

#[derive(Debug, Clone)]
enum WaitCondition {
    Text(String),
    VisibleText(String),
    NextText(String),
    Quiet(Duration),
    PaneExit,
    GetByText(String),
}

impl WaitCondition {
    fn from_wait_pane(args: &WaitPaneArgs) -> Self {
        if let Some(text) = &args.text {
            return Self::Text(text.clone());
        }
        if let Some(text) = &args.visible_text {
            return Self::VisibleText(text.clone());
        }
        if let Some(text) = &args.next_text {
            return Self::NextText(text.clone());
        }
        if let Some(text) = &args.get_by_text {
            return Self::GetByText(text.clone());
        }
        if args.pane_exit {
            return Self::PaneExit;
        }
        Self::Quiet(args.stable_for.unwrap_or(DEFAULT_STABLE_FOR))
    }

    fn from_send_keys(args: &SendKeysArgs) -> Result<Self, ExitFailure> {
        if args.wait == Some(SendKeysWaitMode::Quiet) {
            return Ok(Self::Quiet(args.stable_for.unwrap_or(DEFAULT_STABLE_FOR)));
        }
        if let Some(text) = &args.wait_text {
            return Ok(Self::Text(text.clone()));
        }
        if let Some(text) = &args.wait_visible_text {
            return Ok(Self::VisibleText(text.clone()));
        }
        if let Some(text) = &args.wait_next_text {
            return Ok(Self::NextText(text.clone()));
        }
        if args.wait_pane_exit {
            return Ok(Self::PaneExit);
        }
        Err(ExitFailure::new(
            1,
            "send-keys --wait requires a wait condition",
        ))
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::VisibleText(_) => "visible-text",
            Self::NextText(_) => "next-text",
            Self::Quiet(_) => "quiet",
            Self::PaneExit => "pane-exit",
            Self::GetByText(_) => "get-by-text",
        }
    }
}

enum WaitCompletion {
    Matched { pane_exit: Option<PaneExitStatus> },
    TimedOut,
    Lag { missed_events: u64 },
}

fn wait_condition(
    connection: &mut Connection,
    target: StableWaitTarget,
    condition: &WaitCondition,
    deadline: StartupDeadline,
    require_revision_change_from: Option<u64>,
) -> Result<WaitCompletion, ExitFailure> {
    match condition {
        WaitCondition::Text(text)
        | WaitCondition::VisibleText(text)
        | WaitCondition::GetByText(text) => wait_visible_text(connection, target, text, deadline),
        WaitCondition::NextText(text) => wait_next_text(connection, target, text, deadline),
        WaitCondition::Quiet(stable_for) => wait_quiet(
            connection,
            target,
            deadline,
            *stable_for,
            require_revision_change_from,
        ),
        WaitCondition::PaneExit => wait_pane_exit(connection, target, deadline),
    }
}

fn wait_visible_text(
    connection: &mut Connection,
    mut target: StableWaitTarget,
    text: &str,
    deadline: StartupDeadline,
) -> Result<WaitCompletion, ExitFailure> {
    loop {
        let snapshot = wait_target::snapshot(connection, &mut target)?;
        if visible_text(&snapshot).contains(text) || !find_visible_text(&snapshot, text).is_empty()
        {
            return Ok(WaitCompletion::Matched { pane_exit: None });
        }
        if deadline
            .remaining_timeout()
            .is_some_and(|remaining| remaining.is_zero())
        {
            return Ok(WaitCompletion::TimedOut);
        }
        std::thread::sleep(deadline.sleep_for(super::common::POLL_INTERVAL));
    }
}

fn wait_quiet(
    connection: &mut Connection,
    mut target: StableWaitTarget,
    deadline: StartupDeadline,
    stable_for: Duration,
    require_revision_change_from: Option<u64>,
) -> Result<WaitCompletion, ExitFailure> {
    let mut last_revision = None;
    let mut stable_since = Instant::now();
    let mut observed_required_change = require_revision_change_from.is_none();

    loop {
        if let Some(completion) = quiet_completion_after_process_end(connection, &mut target)? {
            return Ok(completion);
        }
        let snapshot = match wait_target::snapshot(connection, &mut target) {
            Ok(snapshot) => snapshot,
            // The pane may end between the probe and the snapshot: probe again
            // so the wait ends on what happened to the pane, not on the race.
            Err(snapshot_error) => {
                return quiet_completion_after_process_end(connection, &mut target)?
                    .ok_or(snapshot_error);
            }
        };
        if require_revision_change_from.is_some_and(|baseline| snapshot.revision != baseline) {
            observed_required_change = true;
        }
        if Some(snapshot.revision) != last_revision {
            last_revision = Some(snapshot.revision);
            stable_since = Instant::now();
        }
        if observed_required_change && stable_since.elapsed() >= stable_for {
            return Ok(WaitCompletion::Matched { pane_exit: None });
        }
        if deadline
            .remaining_timeout()
            .is_some_and(|remaining| remaining.is_zero())
        {
            return Ok(WaitCompletion::TimedOut);
        }
        std::thread::sleep(deadline.sleep_for(super::common::POLL_INTERVAL));
    }
}

/// Ends a quiet wait once the pane process has ended; `None` while it runs.
fn quiet_completion_after_process_end(
    connection: &mut Connection,
    target: &mut StableWaitTarget,
) -> Result<Option<WaitCompletion>, ExitFailure> {
    match wait_target::process_state(connection, target)? {
        StableWaitProcessState::Alive => Ok(None),
        // A pane removed without exit metadata (kill-pane) never settled:
        // only an observed process exit may end a quiet wait early.
        StableWaitProcessState::Exited {
            status: pane_exit, ..
        } if pane_exit.is_stale() => Err(ExitFailure::new(1, "wait target pane no longer exists")),
        StableWaitProcessState::Exited {
            status: pane_exit, ..
        } => Ok(Some(WaitCompletion::Matched {
            pane_exit: Some(pane_exit),
        })),
        StableWaitProcessState::TargetGone => {
            Err(ExitFailure::new(1, "wait target session no longer exists"))
        }
    }
}

fn wait_pane_exit(
    connection: &mut Connection,
    mut target: StableWaitTarget,
    deadline: StartupDeadline,
) -> Result<WaitCompletion, ExitFailure> {
    let mut target_gone_since = None;
    loop {
        match wait_target::process_state(connection, &mut target)? {
            StableWaitProcessState::Alive => target_gone_since = None,
            StableWaitProcessState::Exited {
                status: value,
                retained,
            } => {
                if !retained {
                    match wait_pane_output_eof(connection, target.clone(), deadline)? {
                        PaneOutputEofWait::Reached | PaneOutputEofWait::Unavailable => {}
                        PaneOutputEofWait::TimedOut => return Ok(WaitCompletion::TimedOut),
                        PaneOutputEofWait::Lag { missed_events } => {
                            return Ok(WaitCompletion::Lag { missed_events });
                        }
                    }
                }
                return Ok(WaitCompletion::Matched {
                    pane_exit: Some(value),
                });
            }
            StableWaitProcessState::TargetGone => {
                let gone_since = target_gone_since.get_or_insert_with(Instant::now);
                if gone_since.elapsed() >= PANE_EXIT_TOMBSTONE_HANDOFF_GRACE {
                    return Ok(WaitCompletion::Matched {
                        pane_exit: Some(PaneExitStatus::stale()),
                    });
                }
            }
        }
        if deadline
            .remaining_timeout()
            .is_some_and(|remaining| remaining.is_zero())
        {
            return Ok(WaitCompletion::TimedOut);
        }
        std::thread::sleep(deadline.sleep_for(super::common::POLL_INTERVAL));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneOutputEofWait {
    Reached,
    TimedOut,
    Lag { missed_events: u64 },
    Unavailable,
}

fn wait_pane_output_eof(
    connection: &mut Connection,
    mut target: StableWaitTarget,
    deadline: StartupDeadline,
) -> Result<PaneOutputEofWait, ExitFailure> {
    let target_ref = target.refreshed_target_ref(connection, "wait-pane")?;
    let start = PaneOutputSubscriptionStart::Oldest;
    let subscription_id = match stream::try_subscribe(connection, target_ref, start)? {
        Ok(subscription_id) => subscription_id,
        Err(error) if target.is_gone_error(&error) => return Ok(PaneOutputEofWait::Unavailable),
        Err(error) => return Err(stream::subscription_refused(&error)),
    };

    loop {
        let batch = stream::poll_output_silent_lag(connection, subscription_id, "wait-pane")?;
        if let Some(lag) = batch.lag {
            let _ = connection.unsubscribe_pane_output(subscription_id);
            return Ok(PaneOutputEofWait::Lag {
                missed_events: lag.missed_events,
            });
        }
        if batch.saw_eof {
            let _ = connection.unsubscribe_pane_output(subscription_id);
            return Ok(PaneOutputEofWait::Reached);
        }
        if deadline
            .remaining_timeout()
            .is_some_and(|remaining| remaining.is_zero())
        {
            let _ = connection.unsubscribe_pane_output(subscription_id);
            return Ok(PaneOutputEofWait::TimedOut);
        }
        std::thread::sleep(deadline.sleep_for(super::common::POLL_INTERVAL));
    }
}

fn wait_next_text(
    connection: &mut Connection,
    mut target: StableWaitTarget,
    text: &str,
    deadline: StartupDeadline,
) -> Result<WaitCompletion, ExitFailure> {
    let target_ref = target.refreshed_target_ref(connection, "wait-pane")?;
    let subscription_id =
        stream::subscribe(connection, target_ref, PaneOutputSubscriptionStart::Now)?;
    let result =
        wait_next_text_subscription(connection, subscription_id, text.as_bytes(), deadline);
    let _ = connection.unsubscribe_pane_output(subscription_id);
    result
}

fn wait_next_text_subscription(
    connection: &mut Connection,
    subscription_id: PaneOutputSubscriptionId,
    needle: &[u8],
    deadline: StartupDeadline,
) -> Result<WaitCompletion, ExitFailure> {
    let mut tail = Vec::new();
    loop {
        let batch = stream::poll_output_silent_lag(connection, subscription_id, "wait-pane")?;
        if let Some(lag) = batch.lag {
            return Ok(WaitCompletion::Lag {
                missed_events: lag.missed_events,
            });
        }
        for bytes in batch.chunks {
            if observe_needle(&mut tail, &bytes, needle) {
                return Ok(WaitCompletion::Matched { pane_exit: None });
            }
        }
        if batch.saw_eof {
            return Ok(WaitCompletion::TimedOut);
        }
        if deadline
            .remaining_timeout()
            .is_some_and(|remaining| remaining.is_zero())
        {
            return Ok(WaitCompletion::TimedOut);
        }
        std::thread::sleep(deadline.sleep_for(super::common::POLL_INTERVAL));
    }
}

fn observe_needle(tail: &mut Vec<u8>, bytes: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut window = Vec::with_capacity(tail.len().saturating_add(bytes.len()));
    window.extend_from_slice(tail);
    window.extend_from_slice(bytes);
    if window
        .windows(needle.len())
        .any(|candidate| candidate == needle)
    {
        return true;
    }
    let keep = needle.len().saturating_sub(1).min(window.len());
    tail.clear();
    tail.extend_from_slice(&window[window.len() - keep..]);
    false
}

fn target_name(target: &StableWaitTarget) -> String {
    target.target_ref().to_string()
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod quiet_race_tests;
