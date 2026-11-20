use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use rmux_os::identity::UserIdentity;
use rmux_os::process;
use rmux_proto::request::SwitchClientExt3Request;
use rmux_proto::{CommandOutput, OptionName, RmuxError};

use crate::client_format::ClientLoopSnapshot;
use crate::key_table::listed_session_key_table_name;
use crate::outer_terminal::OuterTerminal;
use crate::pane_terminals::HandlerState;
use crate::server_access::current_owner_uid;
use crate::terminal::{base_process_environment, base_process_environment_display_only};

use super::{
    attach_support::{self, ClientFlags},
    control_support, option_value_u32, RequestHandler,
};

mod list_clients;
mod requester_access;

#[cfg(test)]
pub(in crate::handler) use crate::client_names::attached_client_name;
pub(in crate::handler) use crate::client_names::control_client_name;
pub(in crate::handler) use list_clients::{AttachingClient, ListClientSnapshot};
pub(crate) use requester_access::with_authenticated_connection_peer;

pub(in crate::handler) const LIST_CLIENTS_TEMPLATE: &str = "#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}]#{?#{==:#{client_uid},#{uid}},, [user #{?client_user,#{client_user},#{client_uid}}]}#{?client_flags, (#{client_flags}),}";

impl RequestHandler {
    pub(crate) async fn status_job_updates(&self) -> tokio::sync::watch::Receiver<u64> {
        self.state.lock().await.status_jobs().subscribe()
    }

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

    #[allow(dead_code)]
    pub(crate) async fn handle_attached_unlock(&self, attach_pid: u32) {
        let mut active_attach = self.active_attach.lock().await;
        if let Some(active) = active_attach.by_pid.get_mut(&attach_pid) {
            active.suspended = false;
        }
        drop(active_attach);
        self.refresh_theme_sources().await;
    }

    pub(crate) async fn handle_attached_unlock_for_identity(
        &self,
        identity: super::attach_support::ActiveAttachIdentity,
    ) -> bool {
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        else {
            return false;
        };
        active.suspended = false;
        drop(active_attach);
        self.reset_attached_lock_timer(identity).await;
        self.refresh_theme_sources().await;
        true
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
        self.update_client_viewports().await;
        // Resolve attached clients under the state lock (same order as viewport
        // reconciliation: state, then active_attach) instead of cloning the whole
        // option store; command queues call this once per queued command.
        let attach_clients = {
            let state = self.state.lock().await;
            let default_key_tables = state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(session_name, session)| {
                    (
                        session_name.clone(),
                        listed_session_key_table_name(&state, session),
                    )
                })
                .collect::<HashMap<_, _>>();
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .map(|(&pid, active)| {
                    let outer_terminal =
                        OuterTerminal::resolve(&state.options, active.terminal_context.clone());
                    let key_table = active
                        .key_table_name
                        .clone()
                        .or_else(|| default_key_tables.get(&active.session_name).cloned());
                    let snapshot = ListClientSnapshot::from_attached_client(pid, active)
                        .resolved_for_render(
                            outer_terminal.features_string(),
                            key_table.as_deref().unwrap_or("root"),
                        );
                    ListClientSnapshot {
                        key_table,
                        ..snapshot
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
                    pan: Default::default(),
                    viewport: Default::default(),
                    name: control_client_name(pid),
                    pid,
                    tty: String::new(),
                    control: true,
                    session_id: active.session_id,
                    session_name: active.session_name.clone(),
                    last_session_name: active.last_session.clone(),
                    order: active.id,
                    activity_at: active.activity_at,
                    width: active.client_width,
                    height: None,
                    sort_height: active.client_height,
                    termname: active.terminal_context.term_name().to_owned(),
                    termtype: String::new(),
                    termfeatures: active.terminal_context.explicit_features_string(),
                    utf8: active.terminal_context.utf8(),
                    key_table: None,
                    uid: active.uid,
                    user: active.user.clone(),
                    read_only: active.flags.read_only,
                    flags: format_control_client_flags(active),
                })
                .collect::<Vec<_>>()
        };
        let mut clients = attach_clients;
        clients.extend(control_clients);
        sort_client_snapshots_by_order(&mut clients);
        clients
    }

    pub(in crate::handler) async fn client_loop_snapshots(&self) -> Vec<ClientLoopSnapshot> {
        self.list_clients_snapshot()
            .await
            .into_iter()
            .map(|client| client.client_loop_snapshot())
            .collect()
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
            SessionSortOrder::Size => left.sort_size().cmp(&right.sort_size()),
            SessionSortOrder::Creation | SessionSortOrder::Index => left.order.cmp(&right.order),
            SessionSortOrder::Activity => right.activity_at.cmp(&left.activity_at),
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

fn sort_client_snapshots_by_order(clients: &mut [ListClientSnapshot]) {
    clients.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.name.cmp(&right.name))
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
    attached_client_flags(
        active.flags,
        active.reported_focused,
        active.suspended,
        active.terminal_context.utf8(),
    )
}

/// `#{client_flags}` for an attached client, from the independent facts it depends
/// on. A client attaching for the first time is not registered yet, so it has
/// no `ActiveAttach` to read them from.
pub(in crate::handler) fn attached_client_flags(
    flags: ClientFlags,
    focused: bool,
    suspended: bool,
    utf8: bool,
) -> String {
    format_client_flags([
        Some("attached".to_owned()),
        client_flag(focused, "focused"),
        None,
        client_flag(flags.contains(ClientFlags::IGNORESIZE), "ignore-size"),
        client_flag(
            flags.contains(ClientFlags::NO_DETACH_ON_DESTROY),
            "no-detach-on-destroy",
        ),
        None,
        None,
        None,
        client_flag(flags.contains(ClientFlags::READONLY), "read-only"),
        client_flag(flags.contains(ClientFlags::ACTIVEPANE), "active-pane"),
        client_flag(suspended, "suspended"),
        client_flag(utf8, "UTF-8"),
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
        client_flag(active.flags.ignore_size, "ignore-size"),
        None,
        client_flag(active.flags.no_output, "no-output"),
        client_flag(active.flags.wait_exit, "wait-exit"),
        active
            .flags
            .pause_after_millis
            .map(|pause_after_millis| format!("pause-after={}", pause_after_millis / 1000)),
        client_flag(active.flags.read_only, "read-only"),
        None,
        None,
        client_flag(attached && active.terminal_context.utf8(), "UTF-8"),
    ])
}

pub(in crate::handler) fn attached_client_matches_target(
    client_name: &str,
    target_client: &str,
) -> bool {
    let client_path = Path::new(client_name);
    if client_path == Path::new(target_client) {
        return true;
    }

    client_path
        .strip_prefix("/dev")
        .ok()
        .is_some_and(|stripped| stripped == Path::new(target_client))
}

pub(in crate::handler) fn control_client_target_pid(target: &str) -> Option<u32> {
    target
        .strip_prefix("client-")
        .unwrap_or(target)
        .parse()
        .ok()
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
    // Twin of src/client_terminal.rs: on Windows the daemon and its client are
    // the same machine and rmux always drives the outer as VT, so advertise the
    // base VT feature set for every attach — a VT outer reached without
    // WT_SESSION otherwise never gets mouse reporting or bracketed paste
    // enabled (issue #93). This is a server-side fallback for clients that do
    // not self-advertise; a modern client already sends these.
    #[cfg(windows)]
    {
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "sync");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "bpaste");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "mouse");
        // Clipboard (OSC 52): advertise it for every Windows attach so an Ms
        // template exists and the daemon can emit pane writes under
        // `set-clipboard on` (issue #91). System clipboard delivery remains
        // host-dependent because older ConPTY paths may consume the sequence and
        // an outer may ignore it. The on-only gate keeps the `external` default
        // from letting untrusted output drive the clipboard.
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "clipboard");
        // Title (TSL/FSL) on the same VT reasoning: Windows sets no TERM, so no
        // terminal family supplies a title template and `set-titles on` would
        // have nothing to write into (issue #182). Still emitted only when
        // `set-titles` is on, which is off by default.
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "title");
    }
    if client_environment_is_windows_terminal(client_environment) {
        client_terminal.utf8 = true;
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "sync");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "bpaste");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "mouse");
        push_unique_terminal_feature(&mut client_terminal.terminal_features, "clipboard");
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
        .resolve_array_values(Some(session_name), OptionName::UpdateEnvironment);
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
        format_requester_uid, sort_client_snapshots_by_order, sort_list_clients,
        ListClientSnapshot,
    };

    fn client_snapshot(name: &str, order: u64) -> ListClientSnapshot {
        ListClientSnapshot {
            pan: Default::default(),
            viewport: Default::default(),
            name: name.to_owned(),
            pid: u32::try_from(order).unwrap_or(0),
            tty: String::new(),
            control: false,
            session_id: None,
            session_name: None,
            last_session_name: None,
            order,
            activity_at: crate::client_activity::ClientActivityTimestamp::from_parts(0, 0),
            width: 80,
            height: Some(24),
            sort_height: 24,
            termname: String::new(),
            termtype: String::new(),
            termfeatures: String::new(),
            utf8: true,
            key_table: None,
            uid: 0,
            user: UserIdentity::Uid(0),
            read_only: false,
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
    fn client_loop_order_fuses_attached_and_control_registry_ties() {
        let attached_later = client_snapshot("/dev/ttys019", 1);
        let mut control_tie = client_snapshot("client-42", 0);
        control_tie.control = true;
        let mut attached_tie = client_snapshot("/dev/ttys018", 0);
        attached_tie.control = false;
        let mut clients = vec![attached_later, control_tie, attached_tie];

        sort_client_snapshots_by_order(&mut clients);

        assert_eq!(
            clients
                .iter()
                .map(|client| client.name.as_str())
                .collect::<Vec<_>>(),
            vec!["/dev/ttys018", "client-42", "/dev/ttys019"]
        );
    }

    #[test]
    fn list_clients_size_sort_uses_the_hidden_control_height() {
        let mut tall = client_snapshot("tall", 1);
        tall.control = true;
        tall.width = 100;
        tall.height = None;
        tall.sort_height = 30;
        let mut short = client_snapshot("short", 2);
        short.control = true;
        short.width = 100;
        short.height = None;
        short.sort_height = 20;
        let mut clients = vec![tall, short];

        sort_list_clients(&mut clients, Some("size"), false);

        assert_eq!(
            clients
                .iter()
                .map(|client| client.name.as_str())
                .collect::<Vec<_>>(),
            vec!["short", "tall"]
        );
        assert!(
            clients
                .iter()
                .all(|client| client.height_value().is_empty()),
            "the control height remains hidden from formatting"
        );
    }

    #[test]
    fn list_clients_activity_sort_uses_latest_client_input_first() {
        let mut older = client_snapshot("older", 1);
        older.activity_at = crate::client_activity::ClientActivityTimestamp::from_parts(10, 0);
        let mut newer = client_snapshot("newer", 2);
        newer.activity_at = crate::client_activity::ClientActivityTimestamp::from_parts(20, 0);
        let mut clients = vec![older, newer];

        sort_list_clients(&mut clients, Some("activity"), false);
        assert_eq!(
            clients
                .iter()
                .map(|client| client.name.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );

        sort_list_clients(&mut clients, Some("activity"), true);
        assert_eq!(
            clients
                .iter()
                .map(|client| client.name.as_str())
                .collect::<Vec<_>>(),
            vec!["older", "newer"]
        );
    }

    #[test]
    fn client_loop_activity_snapshot_retains_subseconds_and_complete_reverse() {
        use crate::client_activity::ClientActivityTimestamp;
        use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
        use rmux_core::formats::FormatContext;

        // Frozen native oracle: a later hi input in the same displayed second
        // sorts ahead of lo; /tr reverses the entire result, including ties.
        for (lo_time, hi_time, expected) in [
            (
                (42, 100_000_000),
                (42, 500_000_000),
                "hi,0;lo,1;|lo,0;hi,1;",
            ),
            (
                (42, 500_000_000),
                (42, 100_000_000),
                "lo,0;hi,1;|hi,0;lo,1;",
            ),
            (
                (42, 900_000_000),
                (43, 100_000_000),
                "hi,0;lo,1;|lo,0;hi,1;",
            ),
            (
                (42, 100_000_000),
                (42, 100_000_000),
                "lo,0;hi,1;|hi,0;lo,1;",
            ),
        ] {
            // Names deliberately mirror the native lexical roles: a < z.
            let mut lo = client_snapshot("a", 1);
            let mut hi = client_snapshot("z", 2);
            lo.activity_at = ClientActivityTimestamp::from_parts(lo_time.0, lo_time.1);
            hi.activity_at = ClientActivityTimestamp::from_parts(hi_time.0, hi_time.1);
            lo.session_name = Some(rmux_proto::SessionName::new("lo").unwrap());
            hi.session_name = Some(rmux_proto::SessionName::new("hi").unwrap());
            assert_eq!(lo.format_bindings().activity, lo_time.0.to_string());
            assert_eq!(hi.format_bindings().activity, hi_time.0.to_string());
            let clients = [lo.client_loop_snapshot(), hi.client_loop_snapshot()];
            let runtime = RuntimeFormatContext::new(FormatContext::new())
                .with_client_loop_snapshots(&clients);
            let actual = render_runtime_template(
                "#{L/t:#{client_session},#{loop_last_flag};}|#{L/tr:#{client_session},#{loop_last_flag};}",
                &runtime, false);
            assert_eq!(actual, expected, "{lo_time:?} / {hi_time:?}");
        }
    }

    /// The VT feature set a Windows Terminal attach ends up advertising. On
    /// Windows the `cfg(windows)` fallback also supplies `title` (issue #182);
    /// elsewhere only the WT_SESSION arm runs.
    fn windows_terminal_feature_set() -> Vec<&'static str> {
        let mut features = vec!["sync", "bpaste", "mouse", "clipboard"];
        if cfg!(windows) {
            features.push("title");
        }
        features
    }

    /// Same set when the client already advertised sync/bpaste/mouse itself.
    fn preadvertised_windows_terminal_feature_set() -> Vec<&'static str> {
        let mut features = vec!["SYNC", "BPASTE", "MOUSE", "clipboard"];
        if cfg!(windows) {
            features.push("title");
        }
        features
    }

    #[test]
    fn windows_terminal_environment_enables_synchronized_rendering() {
        let environment = HashMap::from([("WT_SESSION".to_owned(), "session-id".to_owned())]);
        let context = effective_client_terminal_context(
            Some(&environment),
            &ClientTerminalContext::default(),
        );

        assert!(context.utf8);
        assert_eq!(context.terminal_features, windows_terminal_feature_set());
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
        assert_eq!(
            context.terminal_features,
            preadvertised_windows_terminal_feature_set()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_vt_outer_without_windows_terminal_still_advertises_mouse_and_bpaste() {
        // Issue #93 server twin: a Windows client on a VT outer that is not
        // Windows Terminal (no WT_SESSION) must still have mouse + bracketed
        // paste advertised so the daemon enables them on the outer. Before the
        // fix an empty (non-WT) client environment added no features.
        let environment = HashMap::from([("SYSTEMROOT".to_owned(), "C:\\Windows".to_owned())]);
        let context = effective_client_terminal_context(
            Some(&environment),
            &ClientTerminalContext::default(),
        );

        for feature in ["sync", "bpaste", "mouse", "clipboard", "title"] {
            assert!(
                context.terminal_features.iter().any(|f| f == feature),
                "missing {feature} for non-WT Windows outer: {:?}",
                context.terminal_features
            );
        }
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

#[cfg(test)]
#[path = "../handler_client_runtime/update_environment_tests.rs"]
mod update_environment_tests;
