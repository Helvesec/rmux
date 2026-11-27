use rmux_core::OptionStore;
use rmux_proto::{OptionName, ProcessCommand, TerminalSize};

use crate::handler::DEFAULT_SESSION_SIZE;

pub(super) struct SessionCreationOptions {
    pub(super) size: TerminalSize,
    pub(super) base_index: u32,
    pub(super) process_command: Option<ProcessCommand>,
}

pub(super) fn resolve_session_creation_options(
    options: &OptionStore,
    requested_size: Option<TerminalSize>,
    requested_process_command: Option<ProcessCommand>,
) -> SessionCreationOptions {
    SessionCreationOptions {
        size: requested_size.unwrap_or_else(|| default_size(options)),
        base_index: global_u32(options, OptionName::BaseIndex),
        process_command: requested_process_command.or_else(|| default_command(options)),
    }
}

fn global_u32(options: &OptionStore, option: OptionName) -> u32 {
    options
        .global_value(option)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0)
}

fn default_size(options: &OptionStore) -> TerminalSize {
    options
        .global_value(OptionName::DefaultSize)
        .and_then(parse_size)
        .unwrap_or(DEFAULT_SESSION_SIZE)
}

fn parse_size(value: &str) -> Option<TerminalSize> {
    let (cols, rows) = value.split_once('x')?;
    Some(TerminalSize {
        cols: cols.parse().ok()?,
        rows: rows.parse().ok()?,
    })
}

fn default_command(options: &OptionStore) -> Option<ProcessCommand> {
    let command = options.global_value(OptionName::DefaultCommand)?;
    (!command.is_empty()).then(|| ProcessCommand::Shell(command.to_owned()))
}

#[cfg(test)]
mod tests {
    use rmux_proto::{ScopeSelector, SetOptionMode};

    use super::*;

    fn set_global(options: &mut OptionStore, option: OptionName, value: &str) {
        options
            .set(
                ScopeSelector::Global,
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("global option mutation must succeed");
    }

    #[test]
    fn session_global_options_drive_default_creation_options() {
        let mut options = OptionStore::new();
        set_global(&mut options, OptionName::BaseIndex, "7");
        set_global(&mut options, OptionName::DefaultSize, "120x32");
        set_global(&mut options, OptionName::DefaultCommand, "printf default");

        let resolved = resolve_session_creation_options(&options, None, None);

        assert_eq!(resolved.base_index, 7);
        assert_eq!(
            resolved.size,
            TerminalSize {
                cols: 120,
                rows: 32
            }
        );
        assert_eq!(
            resolved.process_command,
            Some(ProcessCommand::Shell("printf default".to_owned()))
        );
    }

    #[test]
    fn explicit_request_values_override_default_options() {
        let mut options = OptionStore::new();
        set_global(&mut options, OptionName::DefaultSize, "120x32");
        set_global(&mut options, OptionName::DefaultCommand, "printf default");

        let requested_command = ProcessCommand::Argv(vec!["printf".to_owned(), "argv".to_owned()]);
        let resolved = resolve_session_creation_options(
            &options,
            Some(TerminalSize { cols: 90, rows: 20 }),
            Some(requested_command.clone()),
        );

        assert_eq!(resolved.size, TerminalSize { cols: 90, rows: 20 });
        assert_eq!(resolved.process_command, Some(requested_command));
    }
}
