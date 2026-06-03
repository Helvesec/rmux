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
    session: &rmux_core::Session,
    window_index: u32,
    tracked: bool,
) -> bool {
    session.window_at(window_index).is_some_and(|window| {
        window_allows_automatic_rename(options, session.name(), window_index, window, tracked)
    })
}

#[cfg(test)]
mod tests {
    use rmux_core::{OptionStore, Session};
    use rmux_proto::{
        OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
    };

    use super::session_window_allows_automatic_rename;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    #[test]
    fn automatic_rename_option_disables_automatic_name_updates() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });
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
            &options, &session, 0, true
        ));
    }

    #[test]
    fn automatic_rename_option_allows_default_automatic_names() {
        let session = Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 });

        assert!(session_window_allows_automatic_rename(
            &OptionStore::new(),
            &session,
            0,
            false
        ));
    }
}
