//! Eligibility for transports with a deliberately smaller request surface.
//! Values and precedence are interpreted only by the canonical typed parser.

use crate::cli_args::{self, schema::CommandSpec};
use std::ffi::OsString;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tail {
    None,
    Single,
    Command,
    Values,
    Resize,
}

pub(super) fn canonical<S: CommandSpec>(
    args: &[OsString],
    supported: &str,
    tail: Tail,
) -> Option<S::Arguments> {
    if super::has_queue_separator(args) {
        return None;
    }
    let schema = S::help();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        if arg == "--" {
            if tail == Tail::None || tail == Tail::Resize {
                return None;
            }
            if tail != Tail::Values && index + 1 == args.len() {
                return None;
            }
            break;
        }
        if !arg.starts_with('-') {
            if tail == Tail::None || tail == Tail::Resize {
                return None;
            }
            if tail == Tail::Single && index + 1 != args.len() {
                return None;
            }
            break;
        }
        let mut chars = arg.chars();
        chars.next();
        let flag = chars.next()?;
        if chars.next().is_some() || !supported.contains(flag) {
            return None;
        }
        let option = schema
            .get_arguments()
            .find(|option| option.get_short() == Some(flag))?;
        if option.get_action().takes_values() {
            let optional = option
                .get_num_args()
                .is_some_and(|range| range.min_values() == 0);
            let take = !optional
                || args
                    .get(index + 1)
                    .and_then(|arg| arg.to_str())
                    .is_some_and(|value| !value.starts_with('-'));
            if take {
                index += 1;
                args.get(index)?;
                if optional && index + 1 != args.len() {
                    return None;
                }
            }
        }
        index += 1;
    }
    parse_single::<S>(args)
}

/// Parse one canonical command using its schema projection without building the full CLI.
fn parse_single<S: CommandSpec>(arguments: &[OsString]) -> Option<S::Arguments> {
    let argv = std::iter::once(OsString::from(S::NAME))
        .chain(arguments.iter().cloned())
        .collect::<Vec<_>>();
    let commands = cli_args::parse_command_queue(&argv).ok()?;
    if !commands.assignments().is_empty() {
        return None;
    }
    let [command] = commands.commands() else {
        return None;
    };
    // Tiny transports use only typed fields; they never execute the stored queue.
    S::parse(
        cli_args::command_arguments_for_clap(command.arguments()),
        String::new(),
    )
    .ok()
}
