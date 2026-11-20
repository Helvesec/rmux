use rmux_core::{OptionStore, Window};
use rmux_proto::{OptionName, SessionName};

pub(crate) fn automatic_rename_enabled(
    options: &OptionStore,
    session_name: &SessionName,
    window_index: u32,
) -> bool {
    options.resolve_for_window(session_name, window_index, OptionName::AutomaticRename)
        == Some("on")
}

pub(crate) fn automatic_rename_enabled_for_new_window(
    options: &OptionStore,
    session_name: &SessionName,
) -> bool {
    // A not-yet-created window has no local option node. Resolve from the
    // session context so an occupied insertion slot cannot lend its override
    // to the new window that will shift it away.
    options.resolve(Some(session_name), OptionName::AutomaticRename) == Some("on")
}

pub(crate) fn window_allows_automatic_rename(
    options: &OptionStore,
    session_name: &SessionName,
    window_index: u32,
    window: &Window,
    tracked: bool,
) -> bool {
    automatic_rename_enabled(options, session_name, window_index)
        && (tracked || window.automatic_rename() || window.name().is_none())
}

#[cfg(test)]
pub(crate) fn session_window_allows_automatic_rename(
    options: &OptionStore,
    session: rmux_core::ManagedSessionView<'_>,
    window_index: u32,
    tracked: bool,
) -> bool {
    session.window_at(window_index).is_some_and(|window| {
        window_allows_automatic_rename(options, session.name(), window_index, window, tracked)
    })
}

#[cfg(test)]
mod tests {
    use rmux_core::OptionStore;
    use rmux_proto::{
        OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
    };

    use super::{automatic_rename_enabled_for_new_window, session_window_allows_automatic_rename};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    #[test]
    fn automatic_rename_option_disables_automatic_name_updates() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 80, rows: 24 },
        );
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(session.name().clone(), 0)),
                OptionName::AutomaticRename,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("automatic-rename option set succeeds");

        assert!(!session_window_allows_automatic_rename(
            &options,
            session.view(),
            0,
            true
        ));
    }

    #[test]
    fn automatic_rename_option_allows_default_automatic_names() {
        let session = crate::session_fixture::SessionFixture::new(
            session_name("alpha"),
            TerminalSize { cols: 80, rows: 24 },
        );

        assert!(session_window_allows_automatic_rename(
            &OptionStore::new(),
            session.view(),
            0,
            false
        ));
    }

    #[test]
    fn new_window_policy_does_not_inherit_an_occupied_slot_override() {
        let alpha = session_name("alpha");
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Global,
                OptionName::AutomaticRename,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("global automatic-rename option set succeeds");
        options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 1)),
                OptionName::AutomaticRename,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("occupied window override set succeeds");

        assert_eq!(
            options.resolve_for_window(&alpha, 1, OptionName::AutomaticRename),
            Some("on")
        );
        assert!(!automatic_rename_enabled_for_new_window(&options, &alpha));
    }
}
