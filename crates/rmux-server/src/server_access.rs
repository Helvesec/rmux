use std::collections::BTreeMap;
use std::fs;

use rmux_proto::{AttachSessionExtRequest, CommandOutput, Request, RmuxError, ServerAccessRequest};

use crate::daemon::real_user_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessMode {
    ReadOnly,
    ReadWrite,
}

impl AccessMode {
    #[must_use]
    pub(crate) const fn can_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    #[must_use]
    pub(crate) const fn display_suffix(self) -> &'static str {
        match self {
            Self::ReadOnly => "R",
            Self::ReadWrite => "W",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedUser {
    pub(crate) uid: u32,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerAccessStore {
    owner_uid: u32,
    entries: BTreeMap<u32, AccessMode>,
}

impl ServerAccessStore {
    #[must_use]
    pub(crate) fn new(owner_uid: u32) -> Self {
        let mut entries = BTreeMap::new();
        entries.insert(0, AccessMode::ReadWrite);
        entries.insert(owner_uid, AccessMode::ReadWrite);
        Self { owner_uid, entries }
    }

    #[must_use]
    pub(crate) fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    #[must_use]
    pub(crate) fn mode_for_uid(&self, uid: u32) -> Option<AccessMode> {
        self.entries.get(&uid).copied()
    }

    pub(crate) fn set_mode(&mut self, uid: u32, mode: AccessMode) -> Result<(), RmuxError> {
        self.ensure_mutable_uid(uid)?;
        self.entries.insert(uid, mode);
        Ok(())
    }

    pub(crate) fn remove_uid(&mut self, uid: u32) -> Result<(), RmuxError> {
        self.ensure_mutable_uid(uid)?;
        self.entries.remove(&uid);
        Ok(())
    }

    #[must_use]
    pub(crate) fn contains_uid(&self, uid: u32) -> bool {
        self.entries.contains_key(&uid)
    }

    pub(crate) fn render_list(&self) -> CommandOutput {
        let mut stdout = Vec::new();
        for (&uid, mode) in &self.entries {
            if uid == 0 {
                continue;
            }
            let line = format!("{} ({})\n", user_name_for_uid(uid), mode.display_suffix());
            stdout.extend_from_slice(line.as_bytes());
        }
        CommandOutput::from_stdout(stdout)
    }

    fn ensure_mutable_uid(&self, uid: u32) -> Result<(), RmuxError> {
        if uid == 0 || uid == self.owner_uid {
            return Err(RmuxError::Server(
                "root and the server owner cannot be modified".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn current_owner_uid() -> u32 {
    real_user_id().unwrap_or(0)
}

pub(crate) fn resolve_user(value: &str) -> Result<ResolvedUser, RmuxError> {
    if let Some(user) = passwd_entries()
        .into_iter()
        .find(|entry| entry.name == value)
    {
        return Ok(ResolvedUser {
            uid: user.uid,
            name: user.name,
        });
    }

    let uid = value
        .parse::<u32>()
        .map_err(|_| RmuxError::Server(format!("unknown user: {value}")))?;
    let Some(user) = passwd_entries().into_iter().find(|entry| entry.uid == uid) else {
        return Err(RmuxError::Server(format!("unknown user: {value}")));
    };

    Ok(ResolvedUser {
        uid,
        name: user.name,
    })
}

#[must_use]
pub(crate) fn user_name_for_uid(uid: u32) -> String {
    passwd_entries()
        .into_iter()
        .find(|entry| entry.uid == uid)
        .map(|entry| entry.name)
        .unwrap_or_else(|| uid.to_string())
}

pub(crate) fn apply_access_policy(request: Request, can_write: bool) -> Result<Request, RmuxError> {
    if can_write {
        return Ok(request);
    }

    match request {
        Request::AttachSession(request) => Ok(Request::AttachSessionExt(AttachSessionExtRequest {
            target: Some(request.target),
            detach_other_clients: false,
            kill_other_clients: false,
            read_only: true,
            skip_environment_update: false,
            flags: None,
        })),
        Request::AttachSessionExt(mut request) => {
            request.read_only = true;
            Ok(Request::AttachSessionExt(request))
        }
        Request::AttachSessionExt2(mut request) => {
            request.read_only = true;
            Ok(Request::AttachSessionExt2(request))
        }
        request if read_only_request_allowed(&request) => Ok(request),
        _ => Err(RmuxError::Server("client is read-only".to_owned())),
    }
}

fn read_only_request_allowed(request: &Request) -> bool {
    matches!(
        request,
        Request::HasSession(_)
            | Request::NextWindow(_)
            | Request::PreviousWindow(_)
            | Request::LastWindow(_)
            | Request::ListWindows(_)
            | Request::LastPane(_)
            | Request::NextLayout(_)
            | Request::PreviousLayout(_)
            | Request::DisplayPanes(_)
            | Request::ListPanes(_)
            | Request::SelectPane(_)
            | Request::SelectPaneAdjacent(_)
            | Request::AttachSession(_)
            | Request::AttachSessionExt(_)
            | Request::AttachSessionExt2(_)
            | Request::SwitchClient(_)
            | Request::SwitchClientExt(_)
            | Request::SwitchClientExt2(_)
            | Request::SwitchClientExt3(_)
            | Request::DetachClient(_)
            | Request::DetachClientExt(_)
            | Request::RefreshClient(_)
            | Request::ListClients(_)
            | Request::SuspendClient(_)
            | Request::ShowOptions(_)
            | Request::ShowEnvironment(_)
            | Request::ShowHooks(_)
            | Request::ShowBuffer(_)
            | Request::ListBuffers(_)
            | Request::CapturePane(_)
            | Request::DisplayMessage(_)
            | Request::ShowMessages(_)
            | Request::ListSessions(_)
            | Request::ListKeys(_)
            | Request::CopyMode(_)
            | Request::ControlMode(_)
            | Request::ClockMode(_)
            | Request::ServerAccess(ServerAccessRequest { list: true, .. })
    )
}

pub(crate) fn validate_server_access_request(
    request: &ServerAccessRequest,
) -> Result<(), RmuxError> {
    if request.add && request.deny {
        return Err(RmuxError::Server(
            "-a and -d cannot be used together".to_owned(),
        ));
    }
    if request.read_only && request.write {
        return Err(RmuxError::Server(
            "-r and -w cannot be used together".to_owned(),
        ));
    }
    if request.list {
        if request.user.is_some() {
            return Err(RmuxError::Server(
                "server-access -l does not accept a user argument".to_owned(),
            ));
        }
        return Ok(());
    }
    if request.user.is_none() {
        return Err(RmuxError::Server("missing user argument".to_owned()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PasswdEntry {
    uid: u32,
    name: String,
}

fn passwd_entries() -> Vec<PasswdEntry> {
    fs::read_to_string("/etc/passwd")
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter_map(parse_passwd_entry)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn parse_passwd_entry(line: &str) -> Option<PasswdEntry> {
    let mut fields = line.split(':');
    let name = fields.next()?.to_owned();
    let _password = fields.next()?;
    let uid = fields.next()?.parse::<u32>().ok()?;
    Some(PasswdEntry { uid, name })
}
