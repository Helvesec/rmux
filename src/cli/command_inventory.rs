use rmux_core::command_parser::lookup_command;

use super::{write_lines_output, ExitFailure};
use crate::cli_args::{documented_cli_aliases, implemented_command_surface, ListCommandsArgs};

const LIST_COMMAND_SIGNATURES: &[(&str, &str)] = &[
    (
        "attach-session",
        "(attach) [-dErx] [-c working-directory] [-f flags] [-t target-session]",
    ),
    (
        "bind-key",
        "(bind) [-nr] [-T key-table] [-N note] key [command [arguments]]",
    ),
    (
        "break-pane",
        "(breakp) [-abdP] [-F format] [-n window-name] [-s src-pane] [-t dst-window]",
    ),
    (
        "capture-pane",
        "(capturep) [-aCeJNpPqT] [-b buffer-name] [-E end-line] [-S start-line] [-t target-pane]",
    ),
    (
        "choose-buffer",
        "[-NrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
    ),
    (
        "choose-client",
        "[-NrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
    ),
    (
        "choose-tree",
        "[-GNrswZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
    ),
    ("clear-history", "(clearhist) [-H] [-t target-pane]"),
    ("clear-prompt-history", "(clearphist) [-T type]"),
    ("clock-mode", "[-t target-pane]"),
    (
        "command-prompt",
        "[-1bFkiN] [-I inputs] [-p prompts] [-t target-client] [-T type] [template]",
    ),
    (
        "confirm-before",
        "(confirm) [-by] [-c confirm_key] [-p prompt] [-t target-client] command",
    ),
    ("copy-mode", "[-eHMuq] [-s src-pane] [-t target-pane]"),
    ("customize-mode", "[-NZ] [-F format] [-f filter] [-t target-pane]"),
    ("delete-buffer", "(deleteb) [-b buffer-name]"),
    (
        "detach-client",
        "(detach) [-aP] [-E shell-command] [-s target-session] [-t target-client]",
    ),
    (
        "display-menu",
        "(menu) [-O] [-b border-lines] [-c target-client] [-C starting-choice] [-H selected-style] [-s style] [-S border-style] [-t target-pane][-T title] [-x position] [-y position] name key command ...",
    ),
    (
        "display-message",
        "(display) [-aIlNpv] [-c target-client] [-d delay] [-F format] [-t target-pane] [message]",
    ),
    (
        "display-popup",
        "(popup) [-BCE] [-b border-lines] [-c target-client] [-d start-directory] [-e environment] [-h height] [-s style] [-S border-style] [-t target-pane][-T title] [-w width] [-x position] [-y position] [shell-command]",
    ),
    (
        "display-panes",
        "(displayp) [-bN] [-d duration] [-t target-client] [template]",
    ),
    (
        "find-window",
        "(findw) [-CiNrTZ] [-t target-pane] match-string",
    ),
    ("has-session", "(has) [-t target-session]"),
    (
        "if-shell",
        "(if) [-bF] [-t target-pane] shell-command command [command]",
    ),
    (
        "join-pane",
        "(joinp) [-bdfhv] [-l size] [-s src-pane] [-t dst-pane]",
    ),
    ("kill-pane", "(killp) [-a] [-t target-pane]"),
    ("kill-server", ""),
    ("kill-session", "[-aC] [-t target-session]"),
    ("kill-window", "(killw) [-a] [-t target-window]"),
    ("last-pane", "(lastp) [-deZ] [-t target-window]"),
    ("last-window", "(last) [-t target-session]"),
    (
        "link-window",
        "(linkw) [-abdk] [-s src-window] [-t dst-window]",
    ),
    ("list-buffers", "(lsb) [-F format] [-f filter]"),
    (
        "list-clients",
        "(lsc) [-F format] [-f filter] [-t target-session]",
    ),
    ("list-commands", "(lscm) [-F format] [command]"),
    (
        "list-keys",
        "(lsk) [-1aN] [-P prefix-string] [-T key-table] [key]",
    ),
    (
        "list-panes",
        "(lsp) [-as] [-F format] [-f filter] [-t target-window]",
    ),
    ("list-sessions", "(ls) [-F format] [-f filter]"),
    (
        "list-windows",
        "(lsw) [-a] [-F format] [-f filter] [-t target-session]",
    ),
    (
        "load-buffer",
        "(loadb) [-b buffer-name] [-t target-client] path",
    ),
    ("lock-client", "(lockc) [-t target-client]"),
    ("lock-server", "(lock) "),
    ("lock-session", "(locks) [-t target-session]"),
    (
        "move-pane",
        "(movep) [-bdfhv] [-l size] [-s src-pane] [-t dst-pane]",
    ),
    (
        "move-window",
        "(movew) [-abdkr] [-s src-window] [-t dst-window]",
    ),
    (
        "new-session",
        "(new) [-AdDEPX] [-c start-directory] [-e environment] [-F format] [-f flags] [-n window-name] [-s session-name] [-t target-session] [-x width] [-y height] [shell-command]",
    ),
    (
        "new-window",
        "(neww) [-abdkPS] [-c start-directory] [-e environment] [-F format] [-n window-name] [-t target-window] [shell-command]",
    ),
    ("next-layout", "(nextl) [-t target-window]"),
    ("next-window", "(next) [-a] [-t target-session]"),
    (
        "paste-buffer",
        "(pasteb) [-dpr] [-s separator] [-b buffer-name] [-t target-pane]",
    ),
    ("pipe-pane", "(pipep) [-IOo] [-t target-pane] [shell-command]"),
    ("previous-layout", "(prevl) [-t target-window]"),
    ("previous-window", "(prev) [-a] [-t target-session]"),
    (
        "refresh-client",
        "(refresh) [-cDlLRSU] [-A pane:state] [-B name:what:format] [-C XxY] [-f flags] [-t target-client] [adjustment]",
    ),
    ("rename-session", "(rename) [-t target-session] new-name"),
    ("rename-window", "(renamew) [-t target-window] new-name"),
    (
        "resize-pane",
        "(resizep) [-DLMRTUZ] [-x width] [-y height] [-t target-pane] [adjustment]",
    ),
    (
        "resize-window",
        "(resizew) [-aADLRU] [-x width] [-y height] [-t target-window] [adjustment]",
    ),
    (
        "respawn-pane",
        "(respawnp) [-k] [-c start-directory] [-e environment] [-t target-pane] [shell-command]",
    ),
    (
        "respawn-window",
        "(respawnw) [-k] [-c start-directory] [-e environment] [-t target-window] [shell-command]",
    ),
    ("rotate-window", "(rotatew) [-DUZ] [-t target-window]"),
    (
        "run-shell",
        "(run) [-bC] [-c start-directory] [-d delay] [-t target-pane] [shell-command]",
    ),
    ("save-buffer", "(saveb) [-a] [-b buffer-name] path"),
    ("select-layout", "(selectl) [-Enop] [-t target-pane] [layout-name]"),
    (
        "select-pane",
        "(selectp) [-DdeLlMmRUZ] [-T title] [-t target-pane]",
    ),
    ("select-window", "(selectw) [-lnpT] [-t target-window]"),
    (
        "send-keys",
        "(send) [-FHKlMRX] [-c target-client] [-N repeat-count] [-t target-pane] key ...",
    ),
    ("send-prefix", "[-2] [-t target-pane]"),
    ("server-access", "[-adlrw] [user]"),
    (
        "set-buffer",
        "(setb) [-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] data",
    ),
    (
        "set-environment",
        "(setenv) [-Fhgru] [-t target-session] name [value]",
    ),
    ("set-hook", "[-agpRuw] [-t target-pane] hook [command]"),
    (
        "set-option",
        "(set) [-aFgopqsuUw] [-t target-pane] option [value]",
    ),
    (
        "set-window-option",
        "(setw) [-aFgoqu] [-t target-window] option [value]",
    ),
    ("show-buffer", "(showb) [-b buffer-name]"),
    (
        "show-environment",
        "(showenv) [-hgs] [-t target-session] [name]",
    ),
    ("show-hooks", "[-gpw] [-t target-pane]"),
    ("show-messages", "(showmsgs) [-JT] [-t target-client]"),
    (
        "show-options",
        "(show) [-AgHpqsvw] [-t target-pane] [option]",
    ),
    ("show-prompt-history", "(showphist) [-T type]"),
    (
        "show-window-options",
        "(showw) [-gv] [-t target-window] [option]",
    ),
    ("source-file", "(source) [-Fnqv] [-t target-pane] path ..."),
    (
        "split-window",
        "(splitw) [-bdefhIPvZ] [-c start-directory] [-e environment] [-F format] [-l size] [-t target-pane][shell-command]",
    ),
    ("start-server", "(start) "),
    ("suspend-client", "(suspendc) [-t target-client]"),
    ("swap-pane", "(swapp) [-dDUZ] [-s src-pane] [-t dst-pane]"),
    ("swap-window", "(swapw) [-d] [-s src-window] [-t dst-window]"),
    (
        "switch-client",
        "(switchc) [-ElnprZ] [-c target-client] [-t target-session] [-T key-table]",
    ),
    ("unbind-key", "(unbind) [-anq] [-T key-table] key"),
    ("unlink-window", "(unlinkw) [-k] [-t target-window]"),
    ("wait-for", "(wait) [-L|-S|-U] channel"),
    (
        "web-share",
        "[-lwX] [-K share-id] [--config] [--lookup share-id] [--ttl seconds] [--max-viewers count] [--frontend-url url] [--tunnel-url url] [--no-navbar] [--no-disclaimer] [--theme user|light|dark] [--pin] [-t target-pane]",
    ),
];

pub(super) fn run_list_commands(args: ListCommandsArgs) -> Result<i32, ExitFailure> {
    let entries = implemented_command_surface();
    let requested = args
        .command
        .as_deref()
        .map(resolve_list_commands_target)
        .transpose()?;
    let format = args.format.as_deref();
    let lines = entries
        .iter()
        .copied()
        .filter(|entry| requested.map(|name| entry.name == name).unwrap_or(true))
        .map(|entry| render_list_commands_line(format, entry.name, entry.alias))
        .collect::<Vec<_>>();

    write_lines_output(&lines)
}

fn resolve_list_commands_target(name: &str) -> Result<&'static str, ExitFailure> {
    if let Some(alias) = documented_cli_aliases()
        .iter()
        .find(|alias| alias.alias == name)
    {
        return Ok(alias.expansion.split(' ').next().unwrap_or("choose-tree"));
    }

    let resolved = lookup_command(name).map_err(|error| ExitFailure::new(1, error.to_string()))?;
    if implemented_command_surface()
        .iter()
        .any(|entry| entry.name == resolved.name)
    {
        Ok(resolved.name)
    } else {
        Err(ExitFailure::new(
            1,
            format!("command not implemented: {}", resolved.name),
        ))
    }
}

pub(super) fn render_list_commands_line(
    format: Option<&str>,
    name: &str,
    alias: Option<&str>,
) -> String {
    let alias = alias.unwrap_or("");
    let usage = list_command_usage(name);
    match format {
        Some(template) => template
            .replace("#{command_name}", name)
            .replace("#{command_alias}", alias)
            .replace("#{command_list_name}", name)
            .replace("#{command_list_alias}", alias)
            .replace("#{command_list_usage}", usage),
        None => format!("{name} {usage}"),
    }
}

fn list_command_usage(name: &str) -> &'static str {
    LIST_COMMAND_SIGNATURES
        .iter()
        .find_map(|(command_name, usage)| (*command_name == name).then_some(*usage))
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_command_signatures_match_implemented_inventory_order() {
        let expected = implemented_command_surface()
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        let actual = LIST_COMMAND_SIGNATURES
            .iter()
            .map(|(name, _usage)| *name)
            .collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }

    #[test]
    fn list_command_signature_aliases_match_inventory_aliases() {
        for entry in implemented_command_surface() {
            let usage = list_command_usage(entry.name);
            match entry.alias {
                Some(alias) => assert!(
                    usage.starts_with(&format!("({alias})")),
                    "{} list-commands usage should start with alias ({alias}), got {usage:?}",
                    entry.name
                ),
                None => assert!(
                    !usage.starts_with('('),
                    "{} list-commands usage should not advertise an alias, got {usage:?}",
                    entry.name
                ),
            }
        }
    }
}
