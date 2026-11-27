//! Initialize option metadata only for the requested command.

use std::sync::OnceLock;

use super::{add_internal_short_options, CommandShortOptionSpec, LIST_COMMAND_SIGNATURES};

type OptionSpecs = [OnceLock<CommandShortOptionSpec>; LIST_COMMAND_SIGNATURES.len()];

/// Returns typed short-option metadata for a public command.
///
/// Metadata is derived from the same frozen signature rendered by
/// `list-commands`, including adjacent option groups and tmux's `-L|-S|-U`
/// spelling for mutually exclusive boolean flags.
#[must_use]
pub fn command_short_option_spec(command_name: &str) -> Option<&'static CommandShortOptionSpec> {
    static SPECS: OptionSpecs = [const { OnceLock::new() }; LIST_COMMAND_SIGNATURES.len()];
    option_spec(&SPECS, command_name)
}

fn option_spec<'a>(
    specs: &'a OptionSpecs,
    command_name: &str,
) -> Option<&'a CommandShortOptionSpec> {
    LIST_COMMAND_SIGNATURES
        .iter()
        .zip(specs)
        .find(|((name, _), _)| *name == command_name)
        .map(|((name, usage), slot)| {
            slot.get_or_init(|| {
                let mut spec = CommandShortOptionSpec::from_usage(usage);
                add_internal_short_options(name, &mut spec);
                spec
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_lookup_builds_only_the_requested_command() {
        let specs: OptionSpecs = [const { OnceLock::new() }; LIST_COMMAND_SIGNATURES.len()];
        assert!(option_spec(&specs, "not-a-command").is_none());
        assert!(specs.iter().all(|slot| slot.get().is_none()));
        let capture = option_spec(&specs, "capture-pane").unwrap();
        assert!(capture.takes_value('S'));
        assert!(std::ptr::eq(
            capture,
            option_spec(&specs, "capture-pane").unwrap()
        ));
        for ((name, _), slot) in LIST_COMMAND_SIGNATURES.iter().zip(&specs) {
            assert_eq!(slot.get().is_some(), *name == "capture-pane", "{name}");
        }
    }

    #[test]
    fn every_lookup_keeps_the_full_inventory_parser_and_internal_flags() {
        let specs: OptionSpecs = [const { OnceLock::new() }; LIST_COMMAND_SIGNATURES.len()];
        for (name, usage) in LIST_COMMAND_SIGNATURES {
            let mut expected = CommandShortOptionSpec::from_usage(usage);
            add_internal_short_options(name, &mut expected);
            assert_eq!(option_spec(&specs, name), Some(&expected), "{name}");
        }
    }

    #[test]
    fn simultaneous_first_lookups_share_one_stable_spec() {
        let specs: OptionSpecs = [const { OnceLock::new() }; LIST_COMMAND_SIGNATURES.len()];
        let starts = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let starts = &starts;
            let specs = &specs;
            let workers = (0..4)
                .map(|_| {
                    scope.spawn(move || {
                        starts.wait();
                        option_spec(specs, "display-message").unwrap() as *const _ as usize
                    })
                })
                .collect::<Vec<_>>();
            let pointers = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>();
            assert!(pointers.iter().all(|pointer| *pointer == pointers[0]));
        });
        assert_eq!(specs.iter().filter(|slot| slot.get().is_some()).count(), 1);
    }
}
