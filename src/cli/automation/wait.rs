use std::path::Path;
use std::time::{Duration, Instant};

use rmux_client::Connection;
use rmux_proto::{
    ErrorResponse, ListClientsRequest, PaneOutputSubscriptionId, PaneOutputSubscriptionStart,
    PaneTarget, PaneTargetRef, ResolveTargetType, Response, SendKeysExt2Request,
    SendKeysExtRequest, Target,
};
use serde_json::{json, Value};

use crate::cli_args::{SendKeysArgs, SendKeysWaitMode, WaitPaneArgs};
use crate::cli_response::{expect_command_success, tmux_cli_error_message};

use super::super::ExitFailure;
use super::common::{
    check_disabled, connect_cli, duration_millis, elapsed_millis, find_visible_text,
    pane_process_state, pane_snapshot, resolve_pane_ref, sleep_poll_interval,
    stable_pane_ref_for_slot, timeout_deadline, visible_text, write_json, PaneProcessState,
    DEFAULT_STABLE_FOR,
};
use super::pane_exit::PaneExitStatus;
use super::stream;

const CLIENT_FIELD_SEPARATOR: char = '\u{1f}';
const CLIENT_ROW_SEPARATOR: char = '\u{1e}';
const CLIENT_TARGET_FORMAT: &str =
    "#{client_name}\u{1f}#{client_tty}\u{1f}#{client_pid}\u{1f}#{client_session}\u{1f}#{client_control_mode}\u{1e}";

pub(crate) fn run_wait_pane(args: WaitPaneArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_WAIT_PANE", "wait-pane")?;
    let started_at = Instant::now();
    let deadline = timeout_deadline(args.timeout);
    let condition = WaitCondition::from_wait_pane(&args);
    let mut connection = connect_cli(socket_path)?;
    let target = resolve_pane_ref(&mut connection, args.target.as_ref(), "wait-pane")?;

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
) -> Result<i32, ExitFailure> {
    check_disabled("RMUX_DISABLE_SEND_KEYS_WAIT", "send-keys --wait")?;
    let started_at = Instant::now();
    let deadline = timeout_deadline(args.timeout);
    let condition = WaitCondition::from_send_keys(&args)?;
    let mut send_connection = connect_cli(socket_path)?;
    let target_plan = send_keys_target_plan(&mut send_connection, &args)?;
    if target_plan.wait_target.is_none() {
        return Err(unresolved_wait_target_error(&args));
    }

    if let WaitCondition::NextText(bytes) = &condition {
        let wait_target = target_plan
            .wait_target
            .clone()
            .expect("send-keys --wait target was preflighted");
        let mut wait_connection = connect_cli(socket_path)?;
        let subscription_id = stream::subscribe(
            &mut wait_connection,
            wait_target,
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
            .clone()
            .map(|target| {
                pane_snapshot(&mut send_connection, target).map(|snapshot| snapshot.revision)
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

fn write_timeout_result(
    json_output: bool,
    condition: &'static str,
    target: &str,
    timeout: Duration,
) -> Result<i32, ExitFailure> {
    if json_output {
        return write_json(&json!({
            "schema_version": 1,
            "ok": false,
            "error": "timeout",
            "condition": condition,
            "target": target,
            "timeout_ms": duration_millis(timeout),
        }))
        .map(|_| 1);
    }
    Err(ExitFailure::new(
        1,
        format!("wait-pane timed out waiting for {condition}"),
    ))
}

fn timeout_error(condition: &'static str, started_at: Instant) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "send-keys timed out waiting for {condition} after {} ms",
            elapsed_millis(started_at)
        ),
    )
}

fn next_text_lag_error(missed_events: u64) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "wait-pane --next-text lost pane output due to lag before matching; missed {missed_events} events"
        ),
    )
}

fn write_lag_result(
    json_output: bool,
    condition: &'static str,
    target: &str,
    missed_events: u64,
) -> Result<i32, ExitFailure> {
    if json_output {
        return write_json(&lag_json_value(condition, target, missed_events)).map(|_| 1);
    }
    Err(next_text_lag_error(missed_events))
}

fn lag_json_value(condition: &'static str, target: &str, missed_events: u64) -> Value {
    json!({
        "schema_version": 1,
        "ok": false,
        "error": "pane-output-lag",
        "condition": condition,
        "target": target,
        "missed_events": missed_events,
    })
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
    target: PaneTargetRef,
    condition: &WaitCondition,
    deadline: Instant,
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
    target: PaneTargetRef,
    text: &str,
    deadline: Instant,
) -> Result<WaitCompletion, ExitFailure> {
    loop {
        let snapshot = pane_snapshot(connection, target.clone())?;
        if visible_text(&snapshot).contains(text) || !find_visible_text(&snapshot, text).is_empty()
        {
            return Ok(WaitCompletion::Matched { pane_exit: None });
        }
        if Instant::now() >= deadline {
            return Ok(WaitCompletion::TimedOut);
        }
        sleep_poll_interval();
    }
}

fn wait_quiet(
    connection: &mut Connection,
    target: PaneTargetRef,
    deadline: Instant,
    stable_for: Duration,
    require_revision_change_from: Option<u64>,
) -> Result<WaitCompletion, ExitFailure> {
    let mut last_revision = None;
    let mut stable_since = Instant::now();
    let mut observed_required_change = require_revision_change_from.is_none();

    loop {
        if let PaneProcessState::Exited(pane_exit) = pane_process_state(connection, &target)? {
            return Ok(WaitCompletion::Matched {
                pane_exit: Some(pane_exit),
            });
        }
        let snapshot = pane_snapshot(connection, target.clone())?;
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
        if Instant::now() >= deadline {
            return Ok(WaitCompletion::TimedOut);
        }
        sleep_poll_interval();
    }
}

fn wait_pane_exit(
    connection: &mut Connection,
    target: PaneTargetRef,
    deadline: Instant,
) -> Result<WaitCompletion, ExitFailure> {
    loop {
        match pane_process_state(connection, &target)? {
            PaneProcessState::Alive => {}
            PaneProcessState::Exited(value) => {
                match wait_pane_output_eof(connection, target.clone(), deadline)? {
                    PaneOutputEofWait::Reached | PaneOutputEofWait::Unavailable => {}
                    PaneOutputEofWait::TimedOut => return Ok(WaitCompletion::TimedOut),
                    PaneOutputEofWait::Lag { missed_events } => {
                        return Ok(WaitCompletion::Lag { missed_events });
                    }
                }
                return Ok(WaitCompletion::Matched {
                    pane_exit: Some(value),
                });
            }
        }
        if Instant::now() >= deadline {
            return Ok(WaitCompletion::TimedOut);
        }
        sleep_poll_interval();
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
    target: PaneTargetRef,
    deadline: Instant,
) -> Result<PaneOutputEofWait, ExitFailure> {
    let subscription_id =
        match stream::subscribe(connection, target, PaneOutputSubscriptionStart::Oldest) {
            Ok(subscription_id) => subscription_id,
            Err(error) if pane_output_subscription_unavailable(error.message()) => {
                return Ok(PaneOutputEofWait::Unavailable);
            }
            Err(error) => return Err(error),
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
        if Instant::now() >= deadline {
            let _ = connection.unsubscribe_pane_output(subscription_id);
            return Ok(PaneOutputEofWait::TimedOut);
        }
        sleep_poll_interval();
    }
}

fn pane_output_subscription_unavailable(message: &str) -> bool {
    message.contains("pane not found")
        || message.contains("session not found")
        || message.contains("window index does not exist")
}

fn wait_next_text(
    connection: &mut Connection,
    target: PaneTargetRef,
    text: &str,
    deadline: Instant,
) -> Result<WaitCompletion, ExitFailure> {
    let subscription_id = stream::subscribe(connection, target, PaneOutputSubscriptionStart::Now)?;
    let result =
        wait_next_text_subscription(connection, subscription_id, text.as_bytes(), deadline);
    let _ = connection.unsubscribe_pane_output(subscription_id);
    result
}

fn wait_next_text_subscription(
    connection: &mut Connection,
    subscription_id: PaneOutputSubscriptionId,
    needle: &[u8],
    deadline: Instant,
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
        if Instant::now() >= deadline {
            return Ok(WaitCompletion::TimedOut);
        }
        sleep_poll_interval();
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

struct SendKeysTargetPlan {
    send_target: Option<PaneTarget>,
    wait_target: Option<PaneTargetRef>,
}

fn send_keys_target_plan(
    connection: &mut Connection,
    args: &SendKeysArgs,
) -> Result<SendKeysTargetPlan, ExitFailure> {
    let send_target = if args.target.is_some() {
        Some(super::common::resolve_pane_slot(
            connection,
            args.target.as_ref(),
            "send-keys",
        )?)
    } else {
        None
    };
    let wait_target = if let Some(target) = send_target.as_ref() {
        Some(stable_pane_ref_for_slot(connection, target, "send-keys")?)
    } else if let Some(target_client) = args.client_target.as_deref() {
        attached_target_client_pane_ref(connection, target_client)?
    } else {
        let current = super::common::resolve_pane_slot(connection, None, "send-keys")?;
        Some(stable_pane_ref_for_slot(connection, &current, "send-keys")?)
    };

    Ok(SendKeysTargetPlan {
        send_target,
        wait_target,
    })
}

fn attached_target_client_pane_ref(
    connection: &mut Connection,
    target_client: &str,
) -> Result<Option<PaneTargetRef>, ExitFailure> {
    let Some(session_name) = attached_target_client_session(connection, target_client)? else {
        return Ok(None);
    };
    let response = connection
        .resolve_target(Some(session_name), ResolveTargetType::Pane, false, false)
        .map_err(ExitFailure::from_client)?;
    let target = match response {
        Response::ResolveTarget(response) => match response.target {
            Target::Pane(target) => target,
            other => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "resolve-target produced {} where a pane target was required",
                        target_kind_name(&other)
                    ),
                ));
            }
        },
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(
                1,
                tmux_cli_error_message("send-keys", &error),
            ));
        }
        other => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "protocol error: unexpected '{}' response for send-keys",
                    other.command_name()
                ),
            ));
        }
    };

    stable_pane_ref_for_slot(connection, &target, "send-keys").map(Some)
}

fn unresolved_wait_target_error(args: &SendKeysArgs) -> ExitFailure {
    if let Some(target_client) = args.client_target.as_deref() {
        return ExitFailure::new(
            1,
            format!("send-keys --wait cannot observe a pane for target client {target_client:?}"),
        );
    }
    ExitFailure::new(1, "send-keys --wait cannot resolve a pane to observe")
}

fn attached_target_client_session(
    connection: &mut Connection,
    target_client: &str,
) -> Result<Option<String>, ExitFailure> {
    let response = connection
        .list_clients(ListClientsRequest {
            format: Some(CLIENT_TARGET_FORMAT.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_session: None,
        })
        .map_err(ExitFailure::from_client)?;
    let output = match response {
        Response::ListClients(response) => response.output,
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(
                1,
                tmux_cli_error_message("send-keys", &error),
            ));
        }
        other => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "protocol error: unexpected '{}' response for send-keys",
                    other.command_name()
                ),
            ));
        }
    };
    let target_client = normalize_target_client(target_client);
    if target_client == "=" {
        return Ok(None);
    }
    let rendered = String::from_utf8_lossy(output.stdout());
    for row in rendered.split(CLIENT_ROW_SEPARATOR) {
        let Some(client) = ParsedClientRow::parse(row) else {
            continue;
        };
        if client.control || !client.matches(target_client) {
            continue;
        }
        if client.session.is_empty() {
            return Ok(None);
        }
        return Ok(Some(client.session.to_owned()));
    }
    Ok(None)
}

struct ParsedClientRow<'a> {
    name: &'a str,
    tty: &'a str,
    pid: &'a str,
    session: &'a str,
    control: bool,
}

impl<'a> ParsedClientRow<'a> {
    fn parse(row: &'a str) -> Option<Self> {
        if row.is_empty() {
            return None;
        }
        let mut fields = row.split(CLIENT_FIELD_SEPARATOR);
        let name = fields.next()?;
        let tty = fields.next()?;
        let pid = fields.next()?;
        let session = fields.next()?;
        let control = fields.next()? == "1";
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            name,
            tty,
            pid,
            session,
            control,
        })
    }

    fn matches(&self, target_client: &str) -> bool {
        self.pid == target_client
            || client_path_matches(self.name, target_client)
            || client_path_matches(self.tty, target_client)
    }
}

fn client_path_matches(path: &str, target_client: &str) -> bool {
    path == target_client
        || path
            .strip_prefix("/dev/")
            .is_some_and(|stripped| stripped == target_client)
}

fn normalize_target_client(target_client: &str) -> &str {
    target_client.strip_suffix(':').unwrap_or(target_client)
}

fn target_kind_name(target: &Target) -> &'static str {
    match target {
        Target::Session(_) => "session",
        Target::Window(_) => "window",
        Target::Pane(_) => "pane",
    }
}

fn send_keys_through_command_path(
    connection: &mut Connection,
    args: SendKeysArgs,
    target: Option<PaneTarget>,
) -> Result<Response, ExitFailure> {
    if let Some(target_client) = args.client_target {
        return connection
            .send_keys_extended_target_client(SendKeysExt2Request {
                target,
                keys: args.keys,
                expand_formats: args.expand_formats,
                hex: args.hex,
                literal: args.literal,
                dispatch_key_table: args.key_table,
                copy_mode_command: args.copy_mode,
                forward_mouse_event: args.mouse,
                reset_terminal: args.reset_terminal,
                repeat_count: args.repeat_count,
                target_client: Some(target_client),
            })
            .map_err(ExitFailure::from_client);
    }

    connection
        .send_keys_extended(SendKeysExtRequest {
            target,
            keys: args.keys,
            expand_formats: args.expand_formats,
            hex: args.hex,
            literal: args.literal,
            dispatch_key_table: args.key_table,
            copy_mode_command: args.copy_mode,
            forward_mouse_event: args.mouse,
            reset_terminal: args.reset_terminal,
            repeat_count: args.repeat_count,
        })
        .map_err(ExitFailure::from_client)
}

fn target_name(target: &PaneTargetRef) -> String {
    target.to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        lag_json_value, observe_needle, send_keys_wait_exit_code, PaneExitStatus, WaitCondition,
    };

    #[test]
    fn next_text_observer_matches_across_output_chunks() {
        let mut tail = Vec::new();

        assert!(!observe_needle(&mut tail, b"abc", b"cde"));
        assert!(observe_needle(&mut tail, b"def", b"cde"));
    }

    #[test]
    fn next_text_observer_keeps_only_required_tail() {
        let mut tail = Vec::new();

        assert!(!observe_needle(&mut tail, b"abcdef", b"defgh"));

        assert_eq!(tail, b"cdef");
    }

    #[test]
    fn next_text_lag_json_is_machine_readable() {
        let value = lag_json_value("next-text", "%1", 7);

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"], "pane-output-lag");
        assert_eq!(value["condition"], "next-text");
        assert_eq!(value["target"], "%1");
        assert_eq!(value["missed_events"], 7);
    }

    #[test]
    fn quiet_completion_does_not_claim_the_observed_process_status() {
        let condition = WaitCondition::Quiet(Duration::from_millis(100));

        assert_eq!(
            send_keys_wait_exit_code(&condition, Some(PaneExitStatus::known(Some(7), None)),)
                .expect("quiet reports completion rather than process success"),
            0
        );
    }
}
