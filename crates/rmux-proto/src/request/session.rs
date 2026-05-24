use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ProcessCommand, SessionName, TerminalSize};

/// Request payload for `new-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSessionRequest {
    /// The exact session name to create.
    pub session_name: SessionName,
    /// Whether the session should remain detached after creation.
    pub detached: bool,
    /// The initial pane geometry, when explicitly requested.
    pub size: Option<TerminalSize>,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
}

/// Extended request payload for `new-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewSessionExtRequest {
    /// The optional exact session name to create.
    pub session_name: Option<SessionName>,
    /// Optional tmux format-expanded start directory for the new session.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Whether the session should remain detached after creation.
    pub detached: bool,
    /// The initial pane geometry, when explicitly requested.
    pub size: Option<TerminalSize>,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// The optional target session or group name for grouped-session creation.
    #[serde(default)]
    pub group_target: Option<SessionName>,
    /// Whether an existing target session should be attached instead of erroring.
    #[serde(default)]
    pub attach_if_exists: bool,
    /// Whether other attached clients should be detached before attaching.
    #[serde(default)]
    pub detach_other_clients: bool,
    /// Whether other attached clients should be detached and terminated.
    #[serde(default)]
    pub kill_other_clients: bool,
    /// Optional tmux client-flag names such as `read-only` or `active-pane`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
    /// The optional initial active-window name for standalone session creation.
    #[serde(default)]
    pub window_name: Option<String>,
    /// Whether the created session should print formatted session information.
    #[serde(default)]
    pub print_session_info: bool,
    /// The optional format template used when printing session information.
    #[serde(default)]
    pub print_format: Option<String>,
    /// Legacy optional shell command argv. A single argument is executed via
    /// `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Explicit process launch mode for the initial pane.
    #[serde(default)]
    pub process_command: Option<ProcessCommand>,
    /// Whether the session should be created in passthrough mode.
    ///
    /// When `true`, the server sets the session-scope `passthrough`
    /// option BEFORE spawning the initial pane. This is the only
    /// way to guarantee the initial pane's reader is wired up with
    /// a replay log: passthrough's per-pane log is allocated lazily
    /// at reader spawn time, and a later `set-option` flip would
    /// leave the already-spawned reader holding `None`.
    #[serde(default)]
    pub passthrough: bool,
}

impl<'de> Deserialize<'de> for NewSessionExtRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "NewSessionExtRequest",
            &[
                "session_name",
                "working_directory",
                "detached",
                "size",
                "environment",
                "group_target",
                "attach_if_exists",
                "detach_other_clients",
                "kill_other_clients",
                "flags",
                "window_name",
                "print_session_info",
                "print_format",
                "command",
                "process_command",
                "passthrough",
            ],
            NewSessionExtRequestVisitor,
        )
    }
}

struct NewSessionExtRequestVisitor;

impl<'de> Visitor<'de> for NewSessionExtRequestVisitor {
    type Value = NewSessionExtRequest;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a new-session extended request")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let session_name = required_next(&mut seq, 0, &self)?;
        let working_directory = required_next(&mut seq, 1, &self)?;
        let detached = required_next(&mut seq, 2, &self)?;
        let size = required_next(&mut seq, 3, &self)?;
        let environment = required_next(&mut seq, 4, &self)?;
        let group_target = required_next(&mut seq, 5, &self)?;
        let attach_if_exists = required_next(&mut seq, 6, &self)?;
        let detach_other_clients = required_next(&mut seq, 7, &self)?;
        let kill_other_clients = required_next(&mut seq, 8, &self)?;
        let flags = required_next(&mut seq, 9, &self)?;
        let window_name = required_next(&mut seq, 10, &self)?;
        let print_session_info = required_next(&mut seq, 11, &self)?;
        let print_format = required_next(&mut seq, 12, &self)?;
        let command = required_next(&mut seq, 13, &self)?;
        let process_command = compat_next_element(&mut seq)?;
        let passthrough = compat_next_element(&mut seq)?;

        Ok(NewSessionExtRequest {
            session_name,
            working_directory,
            detached,
            size,
            environment,
            group_target,
            attach_if_exists,
            detach_other_clients,
            kill_other_clients,
            flags,
            window_name,
            print_session_info,
            print_format,
            command,
            process_command,
            passthrough,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut session_name = None;
        let mut working_directory = None;
        let mut detached = None;
        let mut size = None;
        let mut environment = None;
        let mut group_target = None;
        let mut attach_if_exists = None;
        let mut detach_other_clients = None;
        let mut kill_other_clients = None;
        let mut flags = None;
        let mut window_name = None;
        let mut print_session_info = None;
        let mut print_format = None;
        let mut command = None;
        let mut process_command = None;
        let mut passthrough = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "session_name" => session_name = Some(map.next_value()?),
                "working_directory" => working_directory = Some(map.next_value()?),
                "detached" => detached = Some(map.next_value()?),
                "size" => size = Some(map.next_value()?),
                "environment" => environment = Some(map.next_value()?),
                "group_target" => group_target = Some(map.next_value()?),
                "attach_if_exists" => attach_if_exists = Some(map.next_value()?),
                "detach_other_clients" => detach_other_clients = Some(map.next_value()?),
                "kill_other_clients" => kill_other_clients = Some(map.next_value()?),
                "flags" => flags = Some(map.next_value()?),
                "window_name" => window_name = Some(map.next_value()?),
                "print_session_info" => print_session_info = Some(map.next_value()?),
                "print_format" => print_format = Some(map.next_value()?),
                "command" => command = Some(map.next_value()?),
                "process_command" => process_command = Some(map.next_value()?),
                "passthrough" => passthrough = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(NewSessionExtRequest {
            session_name: session_name.ok_or_else(|| de::Error::missing_field("session_name"))?,
            working_directory: working_directory
                .ok_or_else(|| de::Error::missing_field("working_directory"))?,
            detached: detached.ok_or_else(|| de::Error::missing_field("detached"))?,
            size: size.ok_or_else(|| de::Error::missing_field("size"))?,
            environment: environment.ok_or_else(|| de::Error::missing_field("environment"))?,
            group_target: group_target.ok_or_else(|| de::Error::missing_field("group_target"))?,
            attach_if_exists: attach_if_exists
                .ok_or_else(|| de::Error::missing_field("attach_if_exists"))?,
            detach_other_clients: detach_other_clients
                .ok_or_else(|| de::Error::missing_field("detach_other_clients"))?,
            kill_other_clients: kill_other_clients
                .ok_or_else(|| de::Error::missing_field("kill_other_clients"))?,
            flags: flags.ok_or_else(|| de::Error::missing_field("flags"))?,
            window_name: window_name.ok_or_else(|| de::Error::missing_field("window_name"))?,
            print_session_info: print_session_info
                .ok_or_else(|| de::Error::missing_field("print_session_info"))?,
            print_format: print_format.ok_or_else(|| de::Error::missing_field("print_format"))?,
            command: command.ok_or_else(|| de::Error::missing_field("command"))?,
            process_command: process_command.unwrap_or_default(),
            passthrough: passthrough.unwrap_or_default(),
        })
    }
}

fn required_next<'de, A, T, V>(seq: &mut A, index: usize, visitor: &V) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de>,
    V: Visitor<'de>,
{
    seq.next_element()?
        .ok_or_else(|| de::Error::invalid_length(index, visitor))
}

fn compat_next_element<'de, A, T>(seq: &mut A) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de> + Default,
{
    match seq.next_element::<T>() {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Ok(T::default()),
        Err(error) if is_truncated_compat_sequence(&error) => Ok(T::default()),
        Err(error) => Err(error),
    }
}

fn is_truncated_compat_sequence(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string();
    message.contains("UnexpectedEof") || message.contains("unexpected end of file")
}

/// Request payload for `has-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HasSessionRequest {
    /// The exact target session name.
    pub target: SessionName,
}

/// Request payload for `kill-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillSessionRequest {
    /// The exact target session name.
    pub target: SessionName,
    /// Whether every other session should be destroyed instead of the target session.
    #[serde(default)]
    pub kill_all_except_target: bool,
    /// Whether the target session's window alert flags should be cleared instead of destroying it.
    #[serde(default)]
    pub clear_alerts: bool,
}

/// Request payload for creating an app-owner lease for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionLeaseRequest {
    /// Session kept alive only while the owner renews this lease.
    pub session_name: SessionName,
    /// Requested lease time-to-live in milliseconds.
    pub ttl_millis: u64,
}

/// Request payload for renewing an app-owner session lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewSessionLeaseRequest {
    /// Leased session name.
    pub session_name: SessionName,
    /// Server-issued lease token.
    pub token: u64,
    /// Requested renewed time-to-live in milliseconds.
    pub ttl_millis: u64,
}

/// Request payload for releasing an app-owner session lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseSessionLeaseRequest {
    /// Leased session name.
    pub session_name: SessionName,
    /// Server-issued lease token.
    pub token: u64,
}

/// Request payload for `rename-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSessionRequest {
    /// The exact existing session name.
    pub target: SessionName,
    /// The validated destination session name.
    pub new_name: SessionName,
}

/// Request payload for `list-sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSessionsRequest {
    /// An optional server-side format template.
    pub format: Option<String>,
    /// An optional server-side filter expression.
    #[serde(default)]
    pub filter: Option<String>,
    /// The optional tmux sort order name.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether the selected sort order should be reversed.
    #[serde(default)]
    pub reversed: bool,
}
