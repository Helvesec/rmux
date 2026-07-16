use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmux_os::identity::UserIdentity;
use rmux_os::process;
use rmux_proto::request::SwitchClientExt3Request;
use rmux_proto::{CommandOutput, OptionName, RmuxError};

use crate::handler_support::attached_client_required;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::server_access::current_owner_uid;
use crate::terminal::{base_process_environment, base_process_environment_display_only};

use super::{
    attach_support::{self, ClientFlags},
    control_support, option_value_u32, prompt_support, RequestHandler,
};

#[path = "handler_client_runtime/list_clients.rs"]
mod list_clients;

pub(in crate::handler) use list_clients::ListClientSnapshot;

pub(in crate::handler) const LIST_CLIENTS_TEMPLATE: &str = "#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}]#{?#{==:#{client_uid},#{uid}},, [user #{?client_user,#{client_user},#{client_uid}}]}#{?client_flags, (#{client_flags}),}";

impl RequestHandler {
    pub(crate) async fn attached_status_interval(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> Option<Duration> {
        let state = self.state.lock().await;
        let seconds = option_value_u32(
            &state.options,
            Some(session_name),
            OptionName::StatusInterval,
        );
        (seconds > 0).then(|| Duration::from_secs(u64::from(seconds)))
    }

    pub(crate) async fn attached_escape_time(&self) -> Duration {
        let state = self.state.lock().await;
        let millis = option_value_u32(&state.options, None, OptionName::EscapeTime);
        Duration::from_millis(u64::from(millis))
    }

    pub(in crate::handler) async fn requester_can_write(&self, requester_pid: u32) -> bool {
        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                return active.can_write && !active.flags.contains(ClientFlags::READONLY);
            }
        }

        let active_control = self.active_control.lock().await;
        if let Some(active) = active_control.by_pid.get(&requester_pid) {
            return active.can_write;
        }
        drop(active_control);

        {
            let detached_access = self
                .active_detached_requester_access
                .lock()
                .expect("active detached requester access mutex must not be poisoned");
            if let Some(active) = detached_access.get(&requester_pid) {
                return active.can_write();
            }
        }

        requester_pid == std::process::id()
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
                    let tty = attached_client_tty_path(pid)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ListClientSnapshot {
                        name: attached_client_name(pid),
                        pid,
                        tty,
                        control: false,
                        session_name: Some(active.session_name.clone()),
                        order: active.id,
                        width: active.client_size.cols,
                        height: active.client_size.rows,
                        termname: active.terminal_context.term_name().to_owned(),
                        termtype: String::new(),
                        termfeatures: outer_terminal.features_string(),
                        utf8: active.terminal_context.utf8(),
                        key_table: active.key_table_name.clone(),
                        uid: active.uid,
                        user: active.user.clone(),
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
                    key_table: None,
                    uid: active.uid,
                    user: active.user.clone(),
                    flags: format_control_client_flags(active),
                })
                .collect::<Vec<_>>()
        };
        attach_clients.into_iter().chain(control_clients).collect()
    }

    pub(crate) async fn refresh_attached_client_status(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        let attached_count = self.attached_count(session_name).await;
        let (prompt, terminal_context, client_size, key_table) = {
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
                active.client_size,
                active.key_table_name.clone(),
            )
        };
        let socket_path = self.socket_path();
        let bytes = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let session = attach_support::sized_session(session, Some(client_size));
            let outer_terminal = OuterTerminal::resolve(&state.options, terminal_context);
            let frame = crate::renderer::render_status_only_with_attached_count_and_prompt(
                session.as_ref(),
                &state.options,
                attached_count,
                crate::renderer::StatusRenderContext {
                    prompt: prompt.as_ref(),
                    state: Some(&state),
                    key_table: key_table.as_deref(),
                    socket_path: Some(&socket_path),
                    ..crate::renderer::StatusRenderContext::default()
                },
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

#[cfg(windows)]
pub(in crate::handler) fn format_client_uid(_uid: u32) -> String {
    String::new()
}

#[cfg(not(windows))]
pub(in crate::handler) fn format_client_uid(uid: u32) -> String {
    uid.to_string()
}

#[cfg(windows)]
pub(in crate::handler) fn format_requester_uid(_uid: u32) -> String {
    String::new()
}

#[cfg(not(windows))]
pub(in crate::handler) fn format_requester_uid(uid: u32) -> String {
    uid.to_string()
}

#[cfg(windows)]
pub(in crate::handler) fn format_client_user(_uid: u32, user: &UserIdentity) -> String {
    match user {
        UserIdentity::Sid(sid) => sid.to_string(),
        UserIdentity::Uid(uid) => uid.to_string(),
    }
}

#[cfg(not(windows))]
pub(in crate::handler) fn format_client_user(uid: u32, _user: &UserIdentity) -> String {
    crate::server_access::user_name_for_uid(uid)
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

pub(in crate::handler) fn switch_target_selector_count(request: &SwitchClientExt3Request) -> usize {
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
    let reversed = reversed && sort_order.is_some();
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
    rmux_os::process::fd_path(attach_pid, 0)
}

pub(in crate::handler) fn attached_client_name(attach_pid: u32) -> String {
    attached_client_tty_path(attach_pid)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| attach_pid.to_string())
}

pub(in crate::handler) fn session_selection_prefers_live_process(pid: u32) -> bool {
    process::is_live(pid)
}

pub(in crate::handler) fn client_environment_snapshot(
    requester_pid: u32,
) -> Option<HashMap<String, String>> {
    if requester_pid == std::process::id() {
        return launched_as_hidden_daemon().then(current_process_environment_snapshot);
    }

    process::environment(requester_pid)
}

pub(in crate::handler) fn effective_client_terminal_context(
    client_environment: Option<&HashMap<String, String>>,
    client_terminal: &rmux_proto::ClientTerminalContext,
) -> rmux_proto::ClientTerminalContext {
    let mut client_terminal = client_terminal.clone();
    client_terminal.utf8 |= client_environment_infers_utf8(client_environment);
    if client_environment_is_windows_terminal(client_environment) {
        client_terminal.utf8 = true;
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "sync");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "bpaste");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "mouse");
    }
    client_terminal
}

fn client_environment_is_windows_terminal(
    client_environment: Option<&HashMap<String, String>>,
) -> bool {
    client_environment.is_some_and(|client_environment| {
        client_environment
            .get("WT_SESSION")
            .is_some_and(|value| !value.is_empty())
    })
}

fn push_unique_terminal_feature(features: &mut Vec<String>, feature: &str) {
    if !features
        .iter()
        .any(|value| value.eq_ignore_ascii_case(feature))
    {
        features.push(feature.to_owned());
    }
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
    base_process_environment()
}

pub(in crate::handler) fn current_process_environment_display_snapshot() -> HashMap<String, String>
{
    base_process_environment_display_only()
}

pub(in crate::handler) fn seed_global_environment(
    state: &mut HandlerState,
    environment: HashMap<String, String>,
) {
    for (name, value) in environment {
        state.environment.set_implicit_global(name, value);
    }
}

pub(in crate::handler) fn seed_global_display_environment(
    state: &mut HandlerState,
    environment: HashMap<String, String>,
) {
    for (name, value) in environment {
        state.environment.set_implicit_global_display(name, value);
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rmux_os::identity::UserIdentity;
    use rmux_proto::ClientTerminalContext;

    use super::{
        effective_client_terminal_context, format_client_uid, format_client_user,
        format_requester_uid, sort_list_clients, ListClientSnapshot,
    };

    fn client_snapshot(name: &str, order: u64) -> ListClientSnapshot {
        ListClientSnapshot {
            name: name.to_owned(),
            pid: u32::try_from(order).unwrap_or(0),
            tty: String::new(),
            control: false,
            session_name: None,
            order,
            width: 80,
            height: 24,
            termname: String::new(),
            termtype: String::new(),
            termfeatures: String::new(),
            utf8: true,
            key_table: None,
            uid: 0,
            user: UserIdentity::Uid(0),
            flags: String::new(),
        }
    }

    #[test]
    fn list_clients_bare_reverse_does_not_reverse_default_order() {
        let mut clients = vec![client_snapshot("beta", 2), client_snapshot("alpha", 1)];

        sort_list_clients(&mut clients, None, true);
        let names = clients
            .iter()
            .map(|client| client.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha", "beta"]);

        sort_list_clients(&mut clients, Some("name"), true);
        let names = clients
            .iter()
            .map(|client| client.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["beta", "alpha"]);
    }

    #[test]
    fn windows_terminal_environment_enables_synchronized_rendering() {
        let environment = HashMap::from([("WT_SESSION".to_owned(), "session-id".to_owned())]);
        let context = effective_client_terminal_context(
            Some(&environment),
            &ClientTerminalContext::default(),
        );

        assert!(context.utf8);
        assert_eq!(context.terminal_features, vec!["sync", "bpaste", "mouse"]);
    }

    #[test]
    fn windows_terminal_features_are_not_duplicated() {
        let environment = HashMap::from([("WT_SESSION".to_owned(), "session-id".to_owned())]);
        let context = effective_client_terminal_context(
            Some(&environment),
            &ClientTerminalContext {
                terminal_features: vec!["SYNC".to_owned(), "BPASTE".to_owned(), "MOUSE".to_owned()],
                utf8: false,
            },
        );

        assert!(context.utf8);
        assert_eq!(context.terminal_features, vec!["SYNC", "BPASTE", "MOUSE"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_client_formats_do_not_expose_synthetic_uid_zero() {
        let sid = UserIdentity::Sid("S-1-5-21-1000".into());

        assert_eq!(format_client_uid(0), "");
        assert_eq!(format_requester_uid(0), "");
        assert_eq!(format_client_user(0, &sid), "S-1-5-21-1000");
    }

    #[cfg(unix)]
    #[test]
    fn unix_client_formats_preserve_uid_values() {
        let identity = UserIdentity::Uid(1234);

        assert_eq!(format_client_uid(1234), "1234");
        assert_eq!(format_requester_uid(1234), "1234");
        assert!(!format_client_user(1234, &identity).is_empty());
    }
}
