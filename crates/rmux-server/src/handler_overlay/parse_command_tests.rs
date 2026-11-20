//! A display-popup shell-command as tmux 3.7b takes it: one empty argument is taken as omitted, so
//! the popup starts the target's default-command, or the shell alone when that is empty.

use super::parse::parse_display_popup;

fn popup_command(arguments: &[&str]) -> Option<String> {
    let arguments = arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
    parse_display_popup(arguments)
        .expect("display-popup parses")
        .command
}

#[test]
fn an_empty_shell_command_is_taken_as_omitted() {
    assert_eq!(popup_command(&["-E", ""]), None);
    assert_eq!(popup_command(&["-E"]), None);
}

#[test]
fn a_nonempty_shell_command_is_kept() {
    assert_eq!(popup_command(&["-E", "true"]).as_deref(), Some("true"));
}
