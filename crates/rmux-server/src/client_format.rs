//! The client-scoped format bindings tmux installs before expanding a
//! client-scoped template.
//!
//! tmux builds one format tree per client through `format_defaults()` and then
//! expands `list-clients`' format, `set-titles-string`, and the rest against
//! it, so `#{client_width}` means the same thing everywhere. Producing them in
//! one place is what keeps those surfaces agreeing: a title expanded against a
//! session-only context silently resolved every `#{client_*}` to the empty
//! string (issue #182).

use crate::client_activity::ClientActivityTimestamp;
use crate::format_runtime::RuntimeFormatContext;
use rmux_proto::TerminalSize;

/// One attached or control client's `#{client_*}` values, already stringified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClientFormatBindings {
    pub(crate) job_identity: Option<(bool, u64)>,
    pub(crate) viewport: crate::client_viewport::ClientViewport,
    pub(crate) name: String,
    pub(crate) pid: String,
    pub(crate) tty: String,
    pub(crate) activity: String,
    pub(crate) session: String,
    pub(crate) last_session: String,
    pub(crate) width: String,
    pub(crate) height: String,
    pub(crate) termfeatures: String,
    pub(crate) termname: String,
    pub(crate) termtype: String,
    pub(crate) key_table: String,
    pub(crate) prefix: String,
    pub(crate) uid: String,
    pub(crate) user: String,
    pub(crate) utf8: String,
    pub(crate) control_mode: String,
    pub(crate) readonly: String,
    pub(crate) flags: String,
}

impl ClientFormatBindings {
    /// Installs these values on a format runtime, overriding any session-scoped
    /// stand-in already bound under the same names.
    pub(crate) fn apply<'a>(&self, runtime: RuntimeFormatContext<'a>) -> RuntimeFormatContext<'a> {
        let runtime = runtime.with_client_bindings(self.clone());
        let runtime = match self.terminal_size() {
            Some(size) => runtime.with_client_size(size),
            None => runtime.without_client_size(),
        };
        runtime
            .with_named_value("client_name", self.name.clone())
            .with_named_value("client_pid", self.pid.clone())
            .with_named_value("client_tty", self.tty.clone())
            .with_named_value("client_activity", self.activity.clone())
            .with_named_value("client_session", self.session.clone())
            .with_named_value("client_last_session", self.last_session.clone())
            .with_named_value("client_width", self.width.clone())
            .with_named_value("client_height", self.height.clone())
            .with_named_value("client_termfeatures", self.termfeatures.clone())
            .with_named_value("client_termname", self.termname.clone())
            .with_named_value("client_termtype", self.termtype.clone())
            .with_named_value("client_key_table", self.key_table.clone())
            .with_named_value("client_prefix", self.prefix.clone())
            .with_named_value("client_uid", self.uid.clone())
            .with_named_value("client_user", self.user.clone())
            .with_named_value("client_utf8", self.utf8.clone())
            .with_named_value("client_control_mode", self.control_mode.clone())
            .with_named_value("client_readonly", self.readonly.clone())
            .with_named_value("client_flags", self.flags.clone())
    }

    fn terminal_size(&self) -> Option<TerminalSize> {
        Some(TerminalSize {
            cols: self.width.parse().ok()?,
            rows: self.height.parse().ok()?,
        })
    }
}

pub(crate) fn sort_client_loop_snapshots(snapshots: &mut [ClientLoopSnapshot]) {
    snapshots.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.name().cmp(right.name()))
    });
}

/// One client record captured before synchronous format expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientLoopSnapshot {
    pub(crate) bindings: ClientFormatBindings,
    pub(crate) order: u64,
    pub(crate) activity_at: ClientActivityTimestamp,
}

impl ClientLoopSnapshot {
    pub(crate) fn name(&self) -> &str {
        &self.bindings.name
    }

    pub(crate) fn is_attached(&self) -> bool {
        !self.bindings.session.is_empty()
    }
}
