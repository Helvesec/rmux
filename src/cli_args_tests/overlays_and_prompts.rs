use super::*;

#[test]
fn display_menu_parses_overlay_flags_and_queue_command() {
    let cli = parse_args(&[
        "display-menu",
        "-M",
        "-O",
        "-b",
        "double",
        "-c",
        "%1",
        "-C",
        "2",
        "-H",
        "fg=black",
        "-s",
        "fg=blue",
        "-S",
        "fg=yellow",
        "-t",
        "alpha:0.0",
        "-T",
        "Menu",
        "-x",
        "C",
        "-y",
        "P",
        "Open",
        "o",
        "display-message open",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::DisplayMenu(args) => {
            assert!(args.mouse);
            assert!(args.select_open);
            assert_eq!(args.border_lines.as_deref(), Some("double"));
            assert_eq!(args.target_client.as_deref(), Some("%1"));
            assert_eq!(args.starting_choice.as_deref(), Some("2"));
            assert_eq!(args.selected_style.as_deref(), Some("fg=black"));
            assert_eq!(args.style.as_deref(), Some("fg=blue"));
            assert_eq!(args.border_style.as_deref(), Some("fg=yellow"));
            assert_eq!(args.target.as_deref(), Some("alpha:0.0"));
            assert_eq!(args.title.as_deref(), Some("Menu"));
            assert_eq!(args.x.as_deref(), Some("C"));
            assert_eq!(args.y.as_deref(), Some("P"));
            assert_eq!(args.items, vec!["Open", "o", "display-message open"]);
            assert!(args.queue_command.starts_with("display-menu "));
            assert!(args.queue_command.contains("-T Menu"));
            assert!(args.queue_command.contains("display-message open"));
        }
        other => panic!("expected DisplayMenu command, got {other:?}"),
    }
}

#[test]
fn display_popup_parses_overlay_flags_and_queue_command() {
    let cli = parse_args(&[
        "display-popup",
        "-B",
        "-C",
        "-E",
        "-k",
        "-N",
        "-b",
        "double",
        "-c",
        "%1",
        "-d",
        "/tmp",
        "-e",
        "FOO=bar",
        "-h",
        "12",
        "-s",
        "fg=blue",
        "-S",
        "fg=yellow",
        "-t",
        "alpha:0.0",
        "-T",
        "Popup",
        "-w",
        "40",
        "-x",
        "C",
        "-y",
        "P",
        "printf hi",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::DisplayPopup(args) => {
            assert!(args.no_border);
            assert!(args.close_all);
            assert!(args.close_on_exit);
            assert!(args.close_on_key);
            assert!(args.no_title_border);
            assert_eq!(args.border_lines.as_deref(), Some("double"));
            assert_eq!(args.target_client.as_deref(), Some("%1"));
            assert_eq!(args.start_directory.as_deref(), Some("/tmp"));
            assert_eq!(args.environment, vec!["FOO=bar"]);
            assert_eq!(args.height.as_deref(), Some("12"));
            assert_eq!(args.style.as_deref(), Some("fg=blue"));
            assert_eq!(args.border_style.as_deref(), Some("fg=yellow"));
            assert_eq!(args.target.as_deref(), Some("alpha:0.0"));
            assert_eq!(args.title.as_deref(), Some("Popup"));
            assert_eq!(args.width.as_deref(), Some("40"));
            assert_eq!(args.x.as_deref(), Some("C"));
            assert_eq!(args.y.as_deref(), Some("P"));
            assert_eq!(args.shell_command, vec!["printf hi"]);
            assert!(args.queue_command.starts_with("display-popup "));
            assert!(args.queue_command.contains("-T Popup"));
            assert!(args.queue_command.contains("printf hi"));
        }
        other => panic!("expected DisplayPopup command, got {other:?}"),
    }
}

#[test]
fn prompt_history_commands_parse_optional_type_filters() {
    let clear = parse_args(&["clear-prompt-history", "-T", "window-target"]).unwrap();
    match clear.command.expect("parsed command") {
        super::super::Command::ClearPromptHistory(args) => {
            assert_eq!(args.prompt_type.as_deref(), Some("window-target"));
            assert_eq!(args.queue_command, "clear-prompt-history -T window-target");
        }
        other => panic!("expected ClearPromptHistory command, got {other:?}"),
    }

    let show = parse_args(&["show-prompt-history", "-T", "search"]).unwrap();
    match show.command.expect("parsed command") {
        super::super::Command::ShowPromptHistory(args) => {
            assert_eq!(args.prompt_type.as_deref(), Some("search"));
            assert_eq!(args.queue_command, "show-prompt-history -T search");
        }
        other => panic!("expected ShowPromptHistory command, got {other:?}"),
    }
}

#[test]
fn prompt_commands_accept_target_client_flags() {
    let prompt = parse_args(&[
        "command-prompt",
        "-t",
        "99999",
        "-p",
        "name",
        "display-message hi",
    ])
    .unwrap();
    match prompt.command.expect("parsed command") {
        super::super::Command::Prompt(args) => {
            assert_eq!(args.target_client.as_deref(), Some("99999"));
            assert_eq!(args.prompts.as_deref(), Some("name"));
            assert!(args.queue_command.contains("-t 99999"));
        }
        other => panic!("expected Prompt command, got {other:?}"),
    }

    let confirm = parse_args(&[
        "confirm-before",
        "-t",
        "99999",
        "-p",
        "sure",
        "display-message hi",
    ])
    .unwrap();
    match confirm.command.expect("parsed command") {
        super::super::Command::ConfirmBefore(args) => {
            assert_eq!(args.target_client.as_deref(), Some("99999"));
            assert_eq!(args.prompt.as_deref(), Some("sure"));
            assert!(args.queue_command.contains("-t 99999"));
        }
        other => panic!("expected ConfirmBefore command, got {other:?}"),
    }
}
