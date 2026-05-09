use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs;

use rmux_os::identity::{IdentityResolver, UserIdentity};
use rmux_proto::{AttachSessionExtRequest, CommandOutput, Request, RmuxError, ServerAccessRequest};

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
    owner_identity: UserIdentity,
    entries: BTreeMap<UserIdentity, AccessMode>,
}

impl ServerAccessStore {
    #[must_use]
    pub(crate) fn new(owner_uid: u32) -> Self {
        let owner_identity = current_user_identity().unwrap_or(UserIdentity::Uid(owner_uid));
        Self::new_for_identity(owner_uid, owner_identity)
    }

    #[must_use]
    pub(crate) fn new_for_identity(owner_uid: u32, owner_identity: UserIdentity) -> Self {
        let mut entries = BTreeMap::new();
        insert_platform_superuser_access(&mut entries);
        entries.insert(owner_identity.clone(), AccessMode::ReadWrite);
        Self {
            owner_uid,
            owner_identity,
            entries,
        }
    }

    #[must_use]
    pub(crate) fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    #[must_use]
    pub(crate) fn mode_for_identity(&self, identity: &UserIdentity) -> Option<AccessMode> {
        self.entries.get(identity).copied()
    }

    pub(crate) fn set_mode(&mut self, uid: u32, mode: AccessMode) -> Result<(), RmuxError> {
        let identity = UserIdentity::Uid(uid);
        self.ensure_mutable_identity(&identity)?;
        self.entries.insert(identity, mode);
        Ok(())
    }

    pub(crate) fn remove_uid(&mut self, uid: u32) -> Result<(), RmuxError> {
        let identity = UserIdentity::Uid(uid);
        self.ensure_mutable_identity(&identity)?;
        self.entries.remove(&identity);
        Ok(())
    }

    #[must_use]
    pub(crate) fn contains_uid(&self, uid: u32) -> bool {
        self.entries.contains_key(&UserIdentity::Uid(uid))
    }

    pub(crate) fn render_list(&self) -> CommandOutput {
        let mut stdout = Vec::new();
        for (identity, mode) in &self.entries {
            if is_reserved_superuser_identity(identity) {
                continue;
            }
            let line = format!(
                "{} ({})\n",
                user_name_for_identity(identity),
                mode.display_suffix()
            );
            stdout.extend_from_slice(line.as_bytes());
        }
        CommandOutput::from_stdout(stdout)
    }

    fn ensure_mutable_identity(&self, identity: &UserIdentity) -> Result<(), RmuxError> {
        if is_reserved_superuser_identity(identity) || *identity == self.owner_identity {
            return Err(RmuxError::Server(
                "root and the server owner cannot be modified".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn insert_platform_superuser_access(entries: &mut BTreeMap<UserIdentity, AccessMode>) {
    entries.insert(UserIdentity::Uid(0), AccessMode::ReadWrite);
}

#[cfg(windows)]
fn insert_platform_superuser_access(_entries: &mut BTreeMap<UserIdentity, AccessMode>) {}

#[cfg(unix)]
fn is_reserved_superuser_identity(identity: &UserIdentity) -> bool {
    *identity == UserIdentity::Uid(0)
}

#[cfg(windows)]
fn is_reserved_superuser_identity(_identity: &UserIdentity) -> bool {
    false
}

pub(crate) fn current_owner_uid() -> u32 {
    current_user_identity()
        .ok()
        .and_then(|identity| match identity {
            UserIdentity::Uid(uid) => Some(uid),
            UserIdentity::Sid(_) => None,
        })
        .unwrap_or(0)
}

fn current_user_identity() -> std::io::Result<UserIdentity> {
    IdentityResolver::current()
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

#[must_use]
fn user_name_for_identity(identity: &UserIdentity) -> String {
    match identity {
        UserIdentity::Uid(uid) => user_name_for_uid(*uid),
        UserIdentity::Sid(sid) => sid.to_string(),
    }
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
            | Request::SubscribePaneOutput(_)
            | Request::UnsubscribePaneOutput(_)
            | Request::PaneOutputCursor(_)
            | Request::DisplayMessage(_)
            | Request::ShowMessages(_)
            | Request::ListSessions(_)
            | Request::ListKeys(_)
            | Request::CopyMode(_)
            | Request::ControlMode(_)
            | Request::ClockMode(_)
            | Request::Handshake(_)
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
    #[cfg(windows)]
    {
        Err(RmuxError::Server(
            "server-access user mutations are unsupported on Windows; named-pipe access is scoped to the current Windows SID".to_owned(),
        ))
    }
    #[cfg(not(windows))]
    {
        if request.user.is_none() {
            return Err(RmuxError::Server("missing user argument".to_owned()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PasswdEntry {
    uid: u32,
    name: String,
}

fn passwd_entries() -> Vec<PasswdEntry> {
    #[cfg(windows)]
    {
        Vec::new()
    }

    #[cfg(unix)]
    {
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
}

fn parse_passwd_entry(line: &str) -> Option<PasswdEntry> {
    let mut fields = line.split(':');
    let name = fields.next()?.to_owned();
    let _password = fields.next()?;
    let uid = fields.next()?.parse::<u32>().ok()?;
    Some(PasswdEntry { uid, name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_store_can_key_owner_by_windows_sid() {
        let owner = UserIdentity::Sid("S-1-5-21-1000".into());
        let store = ServerAccessStore::new_for_identity(0, owner.clone());

        assert_eq!(store.mode_for_identity(&owner), Some(AccessMode::ReadWrite));
        assert_eq!(
            store.mode_for_identity(&UserIdentity::Sid("S-1-5-21-2000".into())),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn access_store_does_not_trust_uid_zero_on_windows() {
        let owner = UserIdentity::Sid("S-1-5-21-1000".into());
        let store = ServerAccessStore::new_for_identity(0, owner.clone());

        assert_eq!(store.mode_for_identity(&owner), Some(AccessMode::ReadWrite));
        assert_eq!(store.mode_for_identity(&UserIdentity::Uid(0)), None);
    }

    #[cfg(unix)]
    #[test]
    fn access_store_trusts_uid_zero_only_on_unix() {
        let owner = UserIdentity::Uid(1000);
        let store = ServerAccessStore::new_for_identity(1000, owner);

        assert_eq!(
            store.mode_for_identity(&UserIdentity::Uid(0)),
            Some(AccessMode::ReadWrite)
        );
    }

    #[test]
    fn access_store_tracks_current_platform_identity_for_owner() {
        let owner = current_user_identity().expect("current identity");
        let store = ServerAccessStore::new(current_owner_uid());

        assert_eq!(store.mode_for_identity(&owner), Some(AccessMode::ReadWrite));
    }

    #[cfg(windows)]
    #[test]
    fn server_access_user_mutations_are_explicitly_unsupported_on_windows() {
        let error = validate_server_access_request(&ServerAccessRequest {
            add: true,
            deny: false,
            list: false,
            read_only: false,
            write: false,
            user: Some("someone".to_owned()),
        })
        .expect_err("Windows cannot safely map server-access users to Unix UIDs");

        assert!(error
            .to_string()
            .contains("unsupported on Windows; named-pipe access"));
    }

    #[cfg(windows)]
    #[test]
    fn server_access_list_still_validates_on_windows() {
        validate_server_access_request(&ServerAccessRequest {
            add: false,
            deny: false,
            list: true,
            read_only: false,
            write: false,
            user: None,
        })
        .expect("server-access -l remains read-only and portable");
    }
}
