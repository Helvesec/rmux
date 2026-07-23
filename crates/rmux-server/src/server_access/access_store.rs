use std::collections::BTreeMap;

use rmux_os::identity::UserIdentity;
use rmux_proto::{CommandOutput, RmuxError};

use super::{current_user_identity, user_name_for_uid};

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
    const fn display_suffix(self) -> &'static str {
        match self {
            Self::ReadOnly => "R",
            Self::ReadWrite => "W",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AccessEpoch(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServerAccessEntry {
    mode: AccessMode,
    epoch: AccessEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerAccessAdmission {
    identity: UserIdentity,
    mode: AccessMode,
    epoch: AccessEpoch,
}

impl ServerAccessAdmission {
    #[must_use]
    pub(crate) const fn can_write(&self) -> bool {
        self.mode.can_write()
    }

    #[must_use]
    pub(crate) fn with_write_cap(mut self, can_write: bool) -> Self {
        if !can_write {
            self.mode = AccessMode::ReadOnly;
        }
        self
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
    entries: BTreeMap<UserIdentity, ServerAccessEntry>,
    next_epoch: u64,
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
        let mut next_epoch = 0;
        insert_platform_superuser_access(&mut entries, &mut next_epoch);
        insert_initial_access_entry(
            &mut entries,
            &mut next_epoch,
            owner_identity.clone(),
            AccessMode::ReadWrite,
        );
        Self {
            owner_uid,
            owner_identity,
            entries,
            next_epoch,
        }
    }

    #[must_use]
    pub(crate) fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    #[must_use]
    pub(crate) fn owner_identity(&self) -> &UserIdentity {
        &self.owner_identity
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn mode_for_identity(&self, identity: &UserIdentity) -> Option<AccessMode> {
        self.entries.get(identity).map(|entry| entry.mode)
    }

    #[must_use]
    pub(crate) fn admission_for_identity(
        &self,
        identity: &UserIdentity,
    ) -> Option<ServerAccessAdmission> {
        let entry = self.entries.get(identity)?;
        Some(ServerAccessAdmission {
            identity: identity.clone(),
            mode: entry.mode,
            epoch: entry.epoch,
        })
    }

    #[must_use]
    pub(crate) fn admission_for_identity_with_write_cap(
        &self,
        identity: &UserIdentity,
        can_write: bool,
    ) -> Option<ServerAccessAdmission> {
        self.admission_for_identity(identity)
            .map(|admission| admission.with_write_cap(can_write))
    }

    #[must_use]
    pub(crate) fn owner_admission(&self) -> ServerAccessAdmission {
        self.admission_for_identity(&self.owner_identity)
            .expect("the server owner always has an access entry")
    }

    #[must_use]
    pub(crate) fn revalidate_admission(
        &self,
        admission: &ServerAccessAdmission,
        expected_identity: &UserIdentity,
    ) -> Option<AccessMode> {
        if &admission.identity != expected_identity {
            return None;
        }
        let current = self.entries.get(expected_identity)?;
        if admission.epoch != current.epoch {
            return None;
        }
        Some(current.mode)
    }

    #[must_use]
    pub(crate) fn revalidate_detached_admission(
        &self,
        admission: &ServerAccessAdmission,
    ) -> Option<AccessMode> {
        let current_mode = self.revalidate_admission(admission, &admission.identity)?;
        // Revalidation may observe a current ACL upgrade, but a callback must
        // never gain write authority beyond the scope that admitted it.
        Some(if admission.mode.can_write() && current_mode.can_write() {
            AccessMode::ReadWrite
        } else {
            AccessMode::ReadOnly
        })
    }

    pub(crate) fn set_mode(&mut self, uid: u32, mode: AccessMode) -> Result<(), RmuxError> {
        let identity = UserIdentity::Uid(uid);
        self.ensure_mutable_identity(&identity)?;
        if let Some(entry) = self.entries.get_mut(&identity) {
            entry.mode = mode;
            return Ok(());
        }
        let epoch = self.allocate_epoch()?;
        self.entries
            .insert(identity, ServerAccessEntry { mode, epoch });
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

    #[must_use]
    pub(crate) fn has_delegated_users(&self) -> bool {
        self.entries.keys().any(|identity| {
            !is_reserved_superuser_identity(identity) && *identity != self.owner_identity
        })
    }

    pub(crate) fn render_list(&self) -> CommandOutput {
        let mut stdout = Vec::new();
        for (identity, entry) in &self.entries {
            if is_reserved_superuser_identity(identity) {
                continue;
            }
            let line = format!(
                "{} ({})\n",
                user_name_for_identity(identity),
                entry.mode.display_suffix()
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

    fn allocate_epoch(&mut self) -> Result<AccessEpoch, RmuxError> {
        self.next_epoch = self.next_epoch.checked_add(1).ok_or_else(|| {
            RmuxError::Server("server access identity epoch space exhausted".to_owned())
        })?;
        Ok(AccessEpoch(self.next_epoch))
    }
}

#[cfg(unix)]
fn insert_platform_superuser_access(
    entries: &mut BTreeMap<UserIdentity, ServerAccessEntry>,
    next_epoch: &mut u64,
) {
    insert_initial_access_entry(
        entries,
        next_epoch,
        UserIdentity::Uid(0),
        AccessMode::ReadWrite,
    );
}

#[cfg(windows)]
fn insert_platform_superuser_access(
    _entries: &mut BTreeMap<UserIdentity, ServerAccessEntry>,
    _next_epoch: &mut u64,
) {
}

fn insert_initial_access_entry(
    entries: &mut BTreeMap<UserIdentity, ServerAccessEntry>,
    next_epoch: &mut u64,
    identity: UserIdentity,
    mode: AccessMode,
) {
    if entries.contains_key(&identity) {
        return;
    }
    *next_epoch = next_epoch
        .checked_add(1)
        .expect("initial server access entries cannot exhaust identity epochs");
    entries.insert(
        identity,
        ServerAccessEntry {
            mode,
            epoch: AccessEpoch(*next_epoch),
        },
    );
}

#[cfg(unix)]
fn is_reserved_superuser_identity(identity: &UserIdentity) -> bool {
    *identity == UserIdentity::Uid(0)
}

#[cfg(windows)]
fn is_reserved_superuser_identity(_identity: &UserIdentity) -> bool {
    false
}

fn user_name_for_identity(identity: &UserIdentity) -> String {
    match identity {
        UserIdentity::Uid(uid) => user_name_for_uid(*uid),
        UserIdentity::Sid(sid) => sid.to_string(),
    }
}
