use std::borrow::Cow;

use super::{write_lines_output, ExitFailure};
use crate::cli_args::{implemented_command_surface, ListCommandsArgs};

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
        "(refresh) [-cDlLRSU] [-C XxY] [-f flags] [-t target-client] [adjustment]",
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
    ("capabilities", "[--human|--json]"),
    ("claude", "[install-skill|claude-args...]"),
    ("doctor", "tmux-dropin"),
    ("setup", "tmux-shim"),
    (
        "wait-pane",
        "[-t target-pane] [--text text|--next-text text|--visible-text text|--quiet|--pane-exit|--get-by-text text] [--stable-for duration] [--timeout duration] [--json]",
    ),
    (
        "pane-snapshot",
        "[-t target-pane] [--json] [--style] [--region row,col,rows,cols]",
    ),
    ("stream-pane", "[-t target-pane] [--raw|--lines]"),
    (
        "collect-pane-output",
        "[-t target-pane] --until-pane-exit --max-bytes bytes [--json]",
    ),
    ("locator", "[-t target-pane] --get-by-text text [--json]"),
    (
        "expect-pane",
        "[-t target-pane] --get-by-text text [--visible|--hidden|--count count] [--json]",
    ),
    (
        "find-panes",
        "[--title title] [--title-prefix prefix] [--current-command command] [--cwd path] [--json]",
    ),
    (
        "find-sessions",
        "[--name name] [--name-prefix prefix] [--json]",
    ),
    ("broadcast-keys", "-t target-pane... [-l] -- key ..."),
    (
        "with-session",
        "session-name [--kill-on-owner-exit] [--ttl duration] -- command ...",
    ),
    (
        "web-share",
        "[-lX] [-K share-id] [disconnect share-id] [--config] [--lookup share-id] [--operator-only|--spectator-only] [--ttl seconds|--expires-at RFC3339] [--kill-session-on-expire] [--max-operators count] [--max-spectators count] [--frontend-url url] [--tunnel-url url|--tunnel-provider provider] [--no-navbar] [--no-disclaimer] [--hide-viewers] [--theme user|light|dark] [--no-pin] [-t pane|session]",
    ),
];

/// Commands that are RMUX extensions rather than part of the tmux command
/// surface. They stay in `implemented_command_surface()` (so `--help`, the man
/// page and explicit lookup keep them) but are hidden from the bare
/// `list-commands` listing, which is byte-compared against tmux via
/// `#{command_list_name}`. They remain reachable by explicit name
/// (`list-commands web-share`).
const RMUX_EXTENSION_COMMANDS: &[&str] = &[
    "broadcast-keys",
    "capabilities",
    "claude",
    "collect-pane-output",
    "doctor",
    "expect-pane",
    "find-panes",
    "find-sessions",
    "locator",
    "pane-snapshot",
    "setup",
    "stream-pane",
    "wait-pane",
    "web-share",
    "with-session",
];

fn is_rmux_extension(name: &str) -> bool {
    RMUX_EXTENSION_COMMANDS.contains(&name)
}

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
        .filter(|entry| {
            // Explicit lookup shows exactly the requested command (extensions
            // included); the bare listing hides RMUX extensions for tmux parity.
            match requested {
                None => !is_rmux_extension(entry.name),
                Some(name) => entry.name == name,
            }
        })
        .filter_map(|entry| {
            let line = render_list_commands_line(format, entry.name, entry.alias);
            if format.is_some() && line.is_empty() {
                None
            } else {
                Some(line)
            }
        })
        .collect::<Vec<_>>();

    write_lines_output(&lines)
}

fn resolve_list_commands_target(name: &str) -> Result<&'static str, ExitFailure> {
    let name = list_commands_parser_alias(name);
    if let Some(entry) = implemented_command_surface()
        .iter()
        .find(|entry| entry.name == name || entry.alias == Some(name))
    {
        return Ok(entry.name);
    }

    let matches = implemented_command_surface()
        .iter()
        .filter(|entry| !is_rmux_extension(entry.name))
        .filter(|entry| {
            entry.name.starts_with(name) || entry.alias.is_some_and(|alias| alias.starts_with(name))
        })
        .map(|entry| entry.name)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [command] => Ok(command),
        [] => Err(ExitFailure::new(1, format!("unknown command: {name}"))),
        _ => Err(ExitFailure::new(1, format!("ambiguous command: {name}"))),
    }
}

fn list_commands_parser_alias(name: &str) -> &str {
    match name {
        "choose-session" | "choose-window" => "choose-tree",
        _ => name,
    }
}

pub(super) fn render_list_commands_line(
    format: Option<&str>,
    name: &str,
    alias: Option<&str>,
) -> String {
    let alias = alias.unwrap_or("");
    match format {
        Some(template) => {
            let usage = list_command_usage_without_alias(name);
            render_list_commands_template(template, name, alias, usage.as_ref())
        }
        None => format!("{name} {}", list_command_usage(name)),
    }
}

fn render_list_commands_template(template: &str, name: &str, alias: &str, usage: &str) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut index = 0;
    while index < template.len() {
        let rest = &template[index..];
        if rest.starts_with("##") {
            rendered.push('#');
            index += 2;
            continue;
        }
        if rest.starts_with("#{") {
            let value_start = index + 2;
            let value_rest = &template[value_start..];
            let Some(value_len) = value_rest.find('}') else {
                return rendered;
            };
            let variable = &template[value_start..value_start + value_len];
            if variable.contains("#{") && !variable.starts_with("?#{") {
                return rendered;
            }
            rendered.push_str(list_commands_format_value(variable, name, alias, usage));
            index = value_start + value_len + 1;
            continue;
        }

        let next = rest
            .chars()
            .next()
            .expect("index remains inside template while scanning");
        rendered.push(next);
        index += next.len_utf8();
    }
    rendered
}

fn list_commands_format_value<'a>(
    variable: &str,
    name: &'a str,
    alias: &'a str,
    usage: &'a str,
) -> &'a str {
    match variable {
        "command_list_name" => name,
        "command_list_alias" => alias,
        "command_list_usage" => usage,
        "command_name" | "command_alias" | "command_usage" => "",
        _ => "",
    }
}

fn list_command_usage(name: &str) -> &'static str {
    LIST_COMMAND_SIGNATURES
        .iter()
        .find_map(|(command_name, usage)| (*command_name == name).then_some(*usage))
        .unwrap_or("")
}

fn list_command_usage_without_alias(name: &str) -> Cow<'static, str> {
    let usage = list_command_usage(name);
    if let Some(rest) = usage
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(") "))
    {
        Cow::Owned(rest.1.to_owned())
    } else {
        Cow::Borrowed(usage)
    }
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
    fn bare_list_commands_hides_rmux_extensions_but_surface_keeps_them() {
        let surface: Vec<&str> = implemented_command_surface()
            .iter()
            .map(|entry| entry.name)
            .collect();
        assert!(
            surface.contains(&"capabilities"),
            "RMUX extensions stay in the help/dispatch surface"
        );
        assert!(
            surface.contains(&"web-share"),
            "RMUX extensions stay in the help/dispatch surface"
        );
        assert!(
            surface.contains(&"doctor"),
            "RMUX extensions stay in the help/dispatch surface"
        );
        assert!(
            surface.contains(&"setup"),
            "RMUX extensions stay in the help/dispatch surface"
        );

        // The bare listing (no explicit command requested) drops extensions only.
        let listed: Vec<&str> = implemented_command_surface()
            .iter()
            .filter(|entry| !is_rmux_extension(entry.name))
            .map(|entry| entry.name)
            .collect();
        assert!(
            !listed.contains(&"capabilities"),
            "bare list-commands must omit RMUX extensions for tmux byte-parity"
        );
        assert!(
            !listed.contains(&"web-share"),
            "bare list-commands must omit RMUX extensions for tmux byte-parity"
        );
        assert!(
            !listed.contains(&"doctor"),
            "bare list-commands must omit RMUX extensions for tmux byte-parity"
        );
        assert!(
            !listed.contains(&"setup"),
            "bare list-commands must omit RMUX extensions for tmux byte-parity"
        );
        assert_eq!(listed.len(), surface.len() - RMUX_EXTENSION_COMMANDS.len());
    }

    #[test]
    fn formatted_list_commands_uses_command_list_fields_like_tmux() {
        let rendered = render_list_commands_line(
            Some(
                "#{command_name}|#{command_alias}|#{command_list_name}|#{command_list_alias}|#{command_list_usage}",
            ),
            "swap-window",
            Some("swapw"),
        );

        assert_eq!(
            rendered,
            "||swap-window|swapw|[-d] [-s src-window] [-t dst-window]"
        );
    }

    #[test]
    fn formatted_list_commands_expands_unknown_and_non_list_fields_to_empty() {
        let rendered = render_list_commands_line(
            Some(
                "x#{bogus}y|#{command_name}|#{command_alias}|#{command_usage}|#{command_list_name}",
            ),
            "link-window",
            Some("linkw"),
        );

        assert_eq!(rendered, "xy||||link-window");
    }

    #[test]
    fn formatted_list_commands_continues_after_nested_unknown_format() {
        let rendered = render_list_commands_line(
            Some("#{?#{unknown},yes,no}|#{command_list_name}"),
            "link-window",
            Some("linkw"),
        );

        assert_eq!(rendered, ",yes,no}|link-window");
    }

    #[test]
    fn formatted_list_commands_matches_tmux_escapes_and_incomplete_formats() {
        assert_eq!(
            render_list_commands_line(
                Some("##{command_list_name}|abc#{|#{command_list_name}"),
                "link-window",
                Some("linkw"),
            ),
            "#{command_list_name}|abc"
        );
        assert_eq!(
            render_list_commands_line(
                Some("abc#{|#{command_list_name}|tail"),
                "link-window",
                Some("linkw"),
            ),
            "abc"
        );
    }

    #[test]
    fn explicit_list_commands_still_resolves_rmux_extensions() {
        // The explicit-name path stays usable for RMUX users.
        assert_eq!(
            resolve_list_commands_target("capabilities").expect("capabilities resolves"),
            "capabilities"
        );
        assert_eq!(
            resolve_list_commands_target("web-share").expect("web-share resolves"),
            "web-share"
        );
        assert_eq!(
            resolve_list_commands_target("doctor").expect("doctor resolves"),
            "doctor"
        );
        assert_eq!(
            resolve_list_commands_target("setup").expect("setup resolves"),
            "setup"
        );
        assert_eq!(
            resolve_list_commands_target("wait-pane").expect("wait-pane resolves"),
            "wait-pane"
        );
    }

    #[test]
    fn list_commands_target_resolution_errors_for_unknown_and_ambiguous_names() {
        assert_eq!(
            resolve_list_commands_target("neww").expect("neww prefix resolves"),
            "new-window"
        );
        assert_eq!(
            resolve_list_commands_target("choose-session").expect("choose-session alias resolves"),
            "choose-tree"
        );
        assert!(resolve_list_commands_target("nosuch").is_err());
        assert!(resolve_list_commands_target("list").is_err());
        assert!(resolve_list_commands_target("wait-p").is_err());
        assert!(resolve_list_commands_target("pane-s").is_err());
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
