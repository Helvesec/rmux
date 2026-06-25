use std::env;
use std::sync::OnceLock;

use super::version::{current_windows_version, WindowsVersion};

// These ConPTY compatibility bits are intentionally backend-private. Microsoft
// Learn documents only `PSEUDOCONSOLE_INHERIT_CURSOR`; Windows Terminal and
// OpenConsole use these bits to request modern ConPTY behavior.
const PSEUDOCONSOLE_RESIZE_QUIRK: u32 = 0x2;
const PSEUDOCONSOLE_WIN32_INPUT_MODE: u32 = 0x4;
const PSEUDOCONSOLE_PASSTHROUGH_MODE: u32 = 0x8;
const PASSTHROUGH_MIN_BUILD: u32 = 22_621;
const DISABLE_PASSTHROUGH_ENV: &str = "RMUX_CONPTY_NO_PASSTHROUGH";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConptyFlags {
    bits: u32,
    passthrough: bool,
}

impl ConptyFlags {
    pub(super) const fn bits(self) -> u32 {
        self.bits
    }

    pub(super) const fn uses_passthrough(self) -> bool {
        self.passthrough
    }
}

pub(super) fn selected_conpty_flags() -> ConptyFlags {
    static SELECTED: OnceLock<ConptyFlags> = OnceLock::new();
    *SELECTED.get_or_init(compute_selected_conpty_flags)
}

fn compute_selected_conpty_flags() -> ConptyFlags {
    let version = current_windows_version().ok();
    let passthrough_disabled = env_flag(DISABLE_PASSTHROUGH_ENV);
    let flags = select_conpty_flags(version, passthrough_disabled);
    tracing::debug!(
        target: "rmux::conpty",
        bits = flags.bits(),
        passthrough = flags.uses_passthrough(),
        version = ?version,
        "selected ConPTY flags"
    );
    flags
}

pub(super) const fn conpty_flags_without_passthrough() -> ConptyFlags {
    ConptyFlags {
        bits: base_flags(),
        passthrough: false,
    }
}

pub(super) const fn standard_conpty_flags() -> ConptyFlags {
    ConptyFlags {
        bits: 0,
        passthrough: false,
    }
}

fn select_conpty_flags(version: Option<WindowsVersion>, passthrough_disabled: bool) -> ConptyFlags {
    let mut bits = base_flags();
    let passthrough = !passthrough_disabled && version.is_some_and(supports_passthrough);
    if passthrough {
        bits |= PSEUDOCONSOLE_PASSTHROUGH_MODE;
    }
    ConptyFlags { bits, passthrough }
}

const fn base_flags() -> u32 {
    PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE
}

fn supports_passthrough(version: WindowsVersion) -> bool {
    version.build >= PASSTHROUGH_MIN_BUILD
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conpty_flags_enable_passthrough_on_supported_builds() {
        let flags = select_conpty_flags(
            Some(WindowsVersion {
                major: 10,
                minor: 0,
                build: PASSTHROUGH_MIN_BUILD,
            }),
            false,
        );

        assert_eq!(
            flags.bits(),
            PSEUDOCONSOLE_RESIZE_QUIRK
                | PSEUDOCONSOLE_WIN32_INPUT_MODE
                | PSEUDOCONSOLE_PASSTHROUGH_MODE
        );
        assert!(flags.uses_passthrough());
    }

    #[test]
    fn conpty_flags_skip_passthrough_on_older_builds() {
        let flags = select_conpty_flags(
            Some(WindowsVersion {
                major: 10,
                minor: 0,
                build: PASSTHROUGH_MIN_BUILD - 1,
            }),
            false,
        );

        assert_eq!(
            flags.bits(),
            PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE
        );
        assert!(!flags.uses_passthrough());
    }

    #[test]
    fn conpty_flags_honor_passthrough_disable_flag() {
        let flags = select_conpty_flags(
            Some(WindowsVersion {
                major: 10,
                minor: 0,
                build: PASSTHROUGH_MIN_BUILD,
            }),
            true,
        );

        assert_eq!(
            flags.bits(),
            PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE
        );
        assert!(!flags.uses_passthrough());
    }

    #[test]
    fn conpty_flags_skip_passthrough_when_version_probe_fails() {
        let flags = select_conpty_flags(None, false);

        assert_eq!(
            flags.bits(),
            PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE
        );
        assert!(!flags.uses_passthrough());
    }
}
