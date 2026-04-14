use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rmux_proto::request::SwitchClientExt3Request;
use rmux_proto::{CommandOutput, OptionName, RmuxError, ScopeSelector};

use crate::handler_support::attached_client_required;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::server_access::current_owner_uid;

use super::{
    attach_support::{self, ClientFlags},
    control_support, prompt_support, RequestHandler,
};

pub(in crate::handler) const LIST_CLIENTS_TEMPLATE: &str = "#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}]#{?#{==:#{client_uid},#{uid}},, [user #{?client_user,#{client_user},#{client_uid}}]}#{?client_flags, (#{client_flags}),}";

impl RequestHandler {
    pub(in crate::handler) async fn requester_can_write(&self, requester_pid: u32) -> bool {
        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                return active.can_write && !active.flags.contains(ClientFlags::READONLY);
            }
        }

        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .map(|active| active.can_write)
            .unwrap_or(true)
    }

    pub(crate) async fn handle_attached_unlock(&self, attach_pid: u32) {
        let mut active_attach = self.active_attach.lock().await;
        if let Some(active) = active_attach.by_pid.get_mut(&attach_pid) {
            active.suspended = false;
        }
    }

    pub(in crate::handler) async fn requester_uid(&self, requester_pid: u32) -> u32 {
        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                return active.uid;
            }
        }
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .map(|active| active.uid)
            .unwrap_or_else(current_owner_uid)
    }

    pub(in crate::handler) async fn list_clients_snapshot(&self) -> Vec<ListClientSnapshot> {
        let options = {
            let state = self.state.lock().await;
            state.options.clone()
        };
        let attach_clients = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .map(|(&pid, active)| {
                    let outer_terminal =
                        OuterTerminal::resolve(&options, active.terminal_context.clone());
                    ListClientSnapshot {
                        name: attached_client_tty_path(pid)
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_else(|| pid.to_string()),
                        pid,
                        tty: attached_client_tty_path(pid)
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        control: false,
                        session_name: Some(active.session_name.clone()),
                        order: active.id,
                        width: active.client_size.cols,
                        height: active.client_size.rows,
                        termname: active.terminal_context.term_name().to_owned(),
                        termtype: String::new(),
                        termfeatures: outer_terminal.features_string(),
                        utf8: active.terminal_context.utf8(),
                        uid: active.uid,
                        flags: format_attached_client_flags(active),
                    }
                })
                .collect::<Vec<_>>()
        };
        let control_clients = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .iter()
                .map(|(&pid, active)| ListClientSnapshot {
                    name: pid.to_string(),
                    pid,
                    tty: String::new(),
                    control: true,
                    session_name: active.session_name.clone(),
                    order: active.id,
                    width: 0,
                    height: 0,
                    termname: active.terminal_context.term_name().to_owned(),
                    termtype: String::new(),
                    termfeatures: active.terminal_context.explicit_features_string(),
                    utf8: active.terminal_context.utf8(),
                    uid: active.uid,
                    flags: format_control_client_flags(active),
                })
                .collect::<Vec<_>>()
        };
        attach_clients.into_iter().chain(control_clients).collect()
    }

    pub(in crate::handler) async fn refresh_attached_client_status(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        let attached_count = self.attached_count(session_name).await;
        let (prompt, terminal_context) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            (
                active
                    .prompt
                    .as_ref()
                    .map(prompt_support::ClientPromptState::rendered_prompt),
                active.terminal_context.clone(),
            )
        };
        let bytes = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let outer_terminal = OuterTerminal::resolve(&state.options, terminal_context);
            let frame = crate::renderer::render_status_only_with_attached_count_and_prompt(
                session,
                &state.options,
                attached_count,
                prompt.as_ref(),
            );
            outer_terminal.wrap_render_frame(&frame)
        };
        self.send_attach_control(
            attach_pid,
            AttachControl::Write(bytes),
            "refresh-client",
            None,
        )
        .await?;
        Ok(())
    }
}

pub(in crate::handler) fn parse_client_flags(
    flags: Option<&Vec<String>>,
    read_only: bool,
) -> Result<ClientFlags, RmuxError> {
    let mut parsed = flags
        .map(|flags| ClientFlags::from_flag_names(flags))
        .transpose()?
        .unwrap_or_default();
    if read_only {
        parsed = parsed.with_read_only();
    }
    Ok(parsed)
}

pub(in crate::handler) fn command_output_from_lines(lines: &[String]) -> CommandOutput {
    if lines.is_empty() {
        return CommandOutput::from_stdout(Vec::new());
    }

    CommandOutput::from_stdout(format!("{}\n", lines.join("\n")).into_bytes())
}

pub(in crate::handler) fn normalize_target_client(target_client: &str) -> &str {
    target_client.strip_suffix(':').unwrap_or(target_client)
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct ListClientSnapshot {
    pub(in crate::handler) name: String,
    pub(in crate::handler) pid: u32,
    pub(in crate::handler) tty: String,
    pub(in crate::handler) control: bool,
    pub(in crate::handler) session_name: Option<rmux_proto::SessionName>,
    pub(in crate::handler) order: u64,
    pub(in crate::handler) width: u16,
    pub(in crate::handler) height: u16,
    pub(in crate::handler) termname: String,
    pub(in crate::handler) termtype: String,
    pub(in crate::handler) termfeatures: String,
    pub(in crate::handler) utf8: bool,
    pub(in crate::handler) uid: u32,
    pub(in crate::handler) flags: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum SessionSortOrder {
    Index,
    Name,
    Activity,
    Creation,
    Modifier,
    Order,
    Size,
}

pub(in crate::handler) fn switch_target_selector_count(
    request: &SwitchClientExt3Request,
) -> usize {
    usize::from(request.target.is_some())
        + usize::from(request.last_session)
        + usize::from(request.next_session)
        + usize::from(request.previous_session)
}

pub(in crate::handler) fn clipboard_query_sequence() -> Vec<u8> {
    b"\x1b]52;c;?\x1b\\".to_vec()
}

pub(in crate::handler) fn parse_session_sort_order(
    sort_order: Option<&str>,
) -> Option<SessionSortOrder> {
    match sort_order?.trim().to_ascii_lowercase().as_str() {
        "index" | "key" => Some(SessionSortOrder::Index),
        "name" | "title" => Some(SessionSortOrder::Name),
        "activity" => Some(SessionSortOrder::Activity),
        "creation" => Some(SessionSortOrder::Creation),
        "modifier" => Some(SessionSortOrder::Modifier),
        "order" => Some(SessionSortOrder::Order),
        "size" => Some(SessionSortOrder::Size),
        _ => None,
    }
}

pub(in crate::handler) fn sort_list_clients(
    clients: &mut [ListClientSnapshot],
    sort_order: Option<&str>,
    reversed: bool,
) {
    clients.sort_by(|left, right| {
        let ordering = match parse_session_sort_order(sort_order)
            .unwrap_or(SessionSortOrder::Creation)
        {
            SessionSortOrder::Name | SessionSortOrder::Modifier | SessionSortOrder::Order => {
                left.name.cmp(&right.name)
            }
            SessionSortOrder::Size => (left.width, left.height).cmp(&(right.width, right.height)),
            SessionSortOrder::Creation | SessionSortOrder::Index => left.order.cmp(&right.order),
            SessionSortOrder::Activity => left
                .session_name
                .as_ref()
                .map(ToString::to_string)
                .cmp(&right.session_name.as_ref().map(ToString::to_string)),
        };
        let ordering = if reversed {
            ordering.reverse()
        } else {
            ordering
        };
        if ordering.is_eq() {
            left.name.cmp(&right.name)
        } else {
            ordering
        }
    });
}

fn client_flag(enabled: bool, value: &'static str) -> Option<String> {
    enabled.then(|| value.to_owned())
}

// Keep this sequence aligned with tmux's server_client_get_flags.
fn format_client_flags(flags: [Option<String>; 12]) -> String {
    flags.into_iter().flatten().collect::<Vec<_>>().join(",")
}

pub(in crate::handler) fn format_attached_client_flags(
    active: &attach_support::ActiveAttach,
) -> String {
    format_client_flags([
        Some("attached".to_owned()),
        client_flag(!active.suspended, "focused"),
        None,
        client_flag(
            active.flags.contains(ClientFlags::IGNORESIZE),
            "ignore-size",
        ),
        client_flag(
            active.flags.contains(ClientFlags::NO_DETACH_ON_DESTROY),
            "no-detach-on-destroy",
        ),
        None,
        None,
        None,
        client_flag(active.flags.contains(ClientFlags::READONLY), "read-only"),
        client_flag(
            active.flags.contains(ClientFlags::ACTIVEPANE),
            "active-pane",
        ),
        client_flag(active.suspended, "suspended"),
        client_flag(active.terminal_context.utf8(), "UTF-8"),
    ])
}

pub(in crate::handler) fn format_control_client_flags(
    active: &control_support::ActiveControl,
) -> String {
    let attached = active.session_name.is_some();
    format_client_flags([
        client_flag(attached, "attached"),
        client_flag(attached, "focused"),
        Some("control-mode".to_owned()),
        None,
        None,
        client_flag(active.flags.no_output, "no-output"),
        client_flag(active.flags.wait_exit, "wait-exit"),
        active
            .flags
            .pause_after_millis
            .map(|pause_after_millis| format!("pause-after={}", pause_after_millis / 1000)),
        None,
        None,
        None,
        client_flag(attached && active.terminal_context.utf8(), "UTF-8"),
    ])
}

pub(in crate::handler) fn attached_client_matches_target(
    attach_pid: u32,
    target_client: &str,
) -> bool {
    let Some(tty_path) = attached_client_tty_path(attach_pid) else {
        return false;
    };
    if tty_path == Path::new(target_client) {
        return true;
    }

    tty_path
        .strip_prefix("/dev")
        .ok()
        .is_some_and(|stripped| stripped == Path::new(target_client))
}

fn attached_client_tty_path(attach_pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{attach_pid}/fd/0")).ok()
}

pub(in crate::handler) fn session_selection_prefers_live_process(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, tail)) = stat.rsplit_once(") ") else {
        return false;
    };
    !matches!(tail.chars().next(), Some('Z' | 'X'))
}

pub(in crate::handler) fn client_environment_snapshot(
    requester_pid: u32,
) -> Option<HashMap<String, String>> {
    if requester_pid == std::process::id() {
        return launched_as_hidden_daemon().then(current_process_environment_snapshot);
    }

    let environ = fs::read(format!("/proc/{requester_pid}/environ")).ok()?;
    let mut values = HashMap::new();
    for entry in environ.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = std::str::from_utf8(entry).ok()?;
        let (name, value) = entry.split_once('=')?;
        values.insert(name.to_owned(), value.to_owned());
    }
    Some(values)
}

pub(in crate::handler) fn effective_client_terminal_context(
    client_environment: Option<&HashMap<String, String>>,
    client_terminal: &rmux_proto::ClientTerminalContext,
) -> rmux_proto::ClientTerminalContext {
    let mut client_terminal = client_terminal.clone();
    client_terminal.utf8 |= client_environment_infers_utf8(client_environment);
    client_terminal
}

fn client_environment_infers_utf8(client_environment: Option<&HashMap<String, String>>) -> bool {
    let Some(client_environment) = client_environment else {
        return false;
    };
    if client_environment
        .get("RMUX")
        .is_some_and(|value| !value.is_empty())
    {
        return true;
    }

    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| {
            client_environment
                .get(name)
                .filter(|value| !value.is_empty())
        })
        .is_some_and(|value| {
            let lower = value.to_ascii_lowercase();
            lower.contains("utf-8") || lower.contains("utf8")
        })
}

fn launched_as_hidden_daemon() -> bool {
    const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";

    std::env::args_os().any(|argument| argument == INTERNAL_DAEMON_FLAG)
}

pub(in crate::handler) fn current_process_environment_snapshot() -> HashMap<String, String> {
    std::env::vars().collect()
}

pub(in crate::handler) fn seed_global_environment(
    state: &mut HandlerState,
    environment: HashMap<String, String>,
) {
    for (name, value) in environment {
        state.environment.set(ScopeSelector::Global, name, value);
    }
}

pub(in crate::handler) fn update_environment_from_client(
    state: &mut HandlerState,
    session_name: &rmux_proto::SessionName,
    client_environment: &HashMap<String, String>,
) {
    let patterns = state
        .options
        .resolve(Some(session_name), OptionName::UpdateEnvironment)
        .map(|value| {
            value
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    state
        .environment
        .update(session_name, &patterns, client_environment);
}
