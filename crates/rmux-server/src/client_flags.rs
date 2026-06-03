#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ClientFlags(u8);

impl ClientFlags {
    pub(crate) const READONLY: Self = Self(1 << 0);
    pub(crate) const IGNORESIZE: Self = Self(1 << 1);
    pub(crate) const ACTIVEPANE: Self = Self(1 << 2);
    pub(crate) const NO_DETACH_ON_DESTROY: Self = Self(1 << 3);

    #[must_use]
    pub(crate) const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub(crate) fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub(crate) fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    #[must_use]
    pub(crate) fn with_read_only(mut self) -> Self {
        self.insert(Self::READONLY);
        self.insert(Self::IGNORESIZE);
        self
    }

    pub(crate) fn toggle_read_only(&mut self) {
        if self.contains(Self::READONLY) {
            self.remove(Self::READONLY);
            self.remove(Self::IGNORESIZE);
        } else {
            self.insert(Self::READONLY);
            self.insert(Self::IGNORESIZE);
        }
    }

    pub(crate) fn insert_named(&mut self, name: &str) -> Result<(), rmux_proto::RmuxError> {
        match name {
            "read-only" | "readonly" => self.insert(Self::READONLY),
            "ignore-size" | "ignoresize" => self.insert(Self::IGNORESIZE),
            "active-pane" | "activepane" => self.insert(Self::ACTIVEPANE),
            "no-detach-on-destroy" | "nodetachondestroy" => {
                self.insert(Self::NO_DETACH_ON_DESTROY);
            }
            other => {
                return Err(rmux_proto::RmuxError::Server(format!(
                    "unknown client flag: {other}"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn apply_named(&mut self, name: &str) -> Result<(), rmux_proto::RmuxError> {
        if let Some(name) = name.strip_prefix('!') {
            match name {
                "read-only" | "readonly" => self.remove(Self::READONLY),
                "ignore-size" | "ignoresize" => self.remove(Self::IGNORESIZE),
                "active-pane" | "activepane" => self.remove(Self::ACTIVEPANE),
                "no-detach-on-destroy" | "nodetachondestroy" => {
                    self.remove(Self::NO_DETACH_ON_DESTROY);
                }
                other => {
                    return Err(rmux_proto::RmuxError::Server(format!(
                        "unknown client flag: {other}"
                    )));
                }
            }
            return Ok(());
        }

        self.insert_named(name)
    }

    pub(crate) fn from_flag_names(values: &[String]) -> Result<Self, rmux_proto::RmuxError> {
        let mut flags = Self::default();
        for raw in values {
            for value in raw.split(',').filter(|value| !value.is_empty()) {
                flags.apply_named(value)?;
            }
        }
        Ok(flags)
    }
}
