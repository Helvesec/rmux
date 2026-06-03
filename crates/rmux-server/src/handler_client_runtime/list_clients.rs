use rmux_os::identity::UserIdentity;

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
    pub(in crate::handler) key_table: Option<String>,
    pub(in crate::handler) uid: u32,
    pub(in crate::handler) user: UserIdentity,
    pub(in crate::handler) flags: String,
}

impl ListClientSnapshot {
    pub(in crate::handler) fn key_table_name(&self) -> &str {
        self.key_table.as_deref().unwrap_or("root")
    }

    pub(in crate::handler) fn prefix_value(&self) -> &'static str {
        if self.key_table.as_deref() == Some("prefix") {
            "1"
        } else {
            "0"
        }
    }
}
