use rmux_core::OptionStore;
use rmux_proto::{OptionName, ProcessCommand, SessionName};

/// Resolves the command for a newly created window or pane.
///
/// An explicit command, including an empty shell command, always wins. Without
/// one, `default-command` follows the addressed session's option inheritance.
/// The empty effective value selects the normal `default-shell` path.
pub(crate) fn resolve_new_pane_process_command(
    options: &OptionStore,
    session_name: &SessionName,
    explicit: Option<ProcessCommand>,
) -> Option<ProcessCommand> {
    explicit.or_else(|| {
        options
            .resolve(Some(session_name), OptionName::DefaultCommand)
            .filter(|command| !command.is_empty())
            .map(|command| ProcessCommand::Shell(command.to_owned()))
    })
}

#[cfg(test)]
mod tests {
    use rmux_proto::{ScopeSelector, SetOptionMode};

    use super::*;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn set_option(options: &mut OptionStore, scope: ScopeSelector, value: &str) {
        options
            .set(
                scope,
                OptionName::DefaultCommand,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("default-command mutation succeeds");
    }

    #[test]
    fn explicit_command_wins_even_when_empty() {
        let mut options = OptionStore::new();
        let alpha = session_name("alpha");
        set_option(&mut options, ScopeSelector::Global, "printf inherited");

        let explicit = ProcessCommand::Shell(String::new());

        assert_eq!(
            resolve_new_pane_process_command(&options, &alpha, Some(explicit.clone())),
            Some(explicit)
        );
    }

    #[test]
    fn session_value_overrides_global_and_empty_masks_it() {
        let mut options = OptionStore::new();
        let alpha = session_name("alpha");
        let beta = session_name("beta");
        set_option(&mut options, ScopeSelector::Global, "printf global");
        set_option(
            &mut options,
            ScopeSelector::Session(alpha.clone()),
            "printf local",
        );
        set_option(&mut options, ScopeSelector::Session(beta.clone()), "");

        assert_eq!(
            resolve_new_pane_process_command(&options, &alpha, None),
            Some(ProcessCommand::Shell("printf local".to_owned()))
        );
        assert_eq!(
            resolve_new_pane_process_command(&options, &beta, None),
            None
        );
    }

    #[test]
    fn missing_session_value_falls_back_to_global() {
        let mut options = OptionStore::new();
        let alpha = session_name("alpha");
        set_option(&mut options, ScopeSelector::Global, "printf global");

        assert_eq!(
            resolve_new_pane_process_command(&options, &alpha, None),
            Some(ProcessCommand::Shell("printf global".to_owned()))
        );
    }
}
