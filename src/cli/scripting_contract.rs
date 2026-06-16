pub(super) const BINARY_CONTRACT_VERSION: u32 = 1;

pub(super) const JSON_COMMANDS: &[&str] = &[
    "capabilities",
    "display-message",
    "list-clients",
    "list-panes",
    "list-sessions",
    "list-windows",
];

pub(super) const CONTROL_NOTIFICATIONS: &[&str] = &[
    "%begin",
    "%end",
    "%error",
    "%output",
    "%extended-output",
    "%pause",
    "%continue",
    "%exit",
    "%message",
    "%config-error",
    "%window-add",
    "%window-close",
    "%window-renamed",
    "%unlinked-window-add",
    "%unlinked-window-close",
    "%unlinked-window-renamed",
    "%window-pane-changed",
    "%pane-mode-changed",
    "%layout-change",
    "%session-changed",
    "%session-renamed",
    "%session-window-changed",
    "%sessions-changed",
    "%client-session-changed",
    "%client-detached",
    "%paste-buffer-changed",
    "%paste-buffer-deleted",
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{CONTROL_NOTIFICATIONS, JSON_COMMANDS};

    #[test]
    fn json_commands_are_sorted_unique_and_stable() {
        assert_eq!(
            JSON_COMMANDS,
            [
                "capabilities",
                "display-message",
                "list-clients",
                "list-panes",
                "list-sessions",
                "list-windows",
            ]
        );
        assert_sorted_unique(JSON_COMMANDS);
    }

    #[test]
    fn control_notifications_are_unique_and_stable() {
        assert_eq!(
            CONTROL_NOTIFICATIONS,
            [
                "%begin",
                "%end",
                "%error",
                "%output",
                "%extended-output",
                "%pause",
                "%continue",
                "%exit",
                "%message",
                "%config-error",
                "%window-add",
                "%window-close",
                "%window-renamed",
                "%unlinked-window-add",
                "%unlinked-window-close",
                "%unlinked-window-renamed",
                "%window-pane-changed",
                "%pane-mode-changed",
                "%layout-change",
                "%session-changed",
                "%session-renamed",
                "%session-window-changed",
                "%sessions-changed",
                "%client-session-changed",
                "%client-detached",
                "%paste-buffer-changed",
                "%paste-buffer-deleted",
            ]
        );
        assert_unique(CONTROL_NOTIFICATIONS);
    }

    #[test]
    fn advertised_control_notifications_cover_control_emitters() {
        let emitted = emitted_control_prefixes();
        let advertised = CONTROL_NOTIFICATIONS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        for prefix in &emitted {
            assert!(
                advertised.contains(prefix),
                "control-mode emitter uses {prefix:?} but capabilities do not advertise it"
            );
        }
        for prefix in &advertised {
            assert!(
                emitted.contains(prefix),
                "capabilities advertise control-mode prefix {prefix:?} but no control emitter uses it"
            );
        }
    }

    fn emitted_control_prefixes() -> BTreeSet<&'static str> {
        [
            include_str!("../../crates/rmux-proto/src/control.rs"),
            include_str!("../../crates/rmux-server/src/control.rs"),
            include_str!("../../crates/rmux-server/src/control/subscriptions.rs"),
            include_str!("../../crates/rmux-server/src/control/output_queue.rs"),
            include_str!("../../crates/rmux-server/src/control_notifications.rs"),
            include_str!("../../crates/rmux-server/src/handler_control.rs"),
            include_str!("../../crates/rmux-server/src/handler_pane/inspection.rs"),
        ]
        .into_iter()
        .flat_map(control_prefixes_in_source)
        .collect()
    }

    fn control_prefixes_in_source(source: &'static str) -> BTreeSet<&'static str> {
        let mut prefixes = BTreeSet::new();
        let mut rest = source;
        while let Some(offset) = rest.find('%') {
            rest = &rest[offset..];
            let mut end = 1;
            for byte in rest.as_bytes().iter().copied().skip(1) {
                if byte.is_ascii_lowercase() || byte == b'-' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > 1 {
                prefixes.insert(&rest[..end]);
            }
            rest = &rest[end..];
        }
        prefixes
    }

    fn assert_sorted_unique(values: &[&str]) {
        for pair in values.windows(2) {
            assert!(pair[0] < pair[1], "{values:?} is not sorted");
        }
        assert_unique(values);
    }

    fn assert_unique(values: &[&str]) {
        let unique = values.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), values.len(), "{values:?} contains duplicates");
    }
}
