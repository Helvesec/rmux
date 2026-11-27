//! display-popup -e values as tmux 3.7b takes them: a value without '=' is accepted and sets
//! nothing, an empty value sets an empty variable.

use super::parse::parse_display_popup;
use crate::terminal::parse_environment_assignments;

fn popup_environment(arguments: &[&str]) -> Vec<String> {
    let arguments = arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
    parse_display_popup(arguments)
        .expect("display-popup parses")
        .environment
}

#[test]
fn a_name_without_equals_sign_sets_nothing() {
    let environment = popup_environment(&["-e", "POPUP_A", "-ePOPUP_B", "true"]);

    assert!(environment.is_empty());
    assert!(parse_environment_assignments(&environment)
        .expect("nothing to refuse")
        .is_empty());
}

#[test]
fn empty_and_set_values_are_kept_around_a_bare_name() {
    let environment =
        popup_environment(&["-e", "POPUP_A=", "-e", "POPUP_B", "-e", "POPUP_C=v", "true"]);

    assert_eq!(environment, ["POPUP_A=", "POPUP_C=v"]);
    let assignments = parse_environment_assignments(&environment).expect("assignments");
    assert_eq!(assignments.get("POPUP_A").map(String::as_str), Some(""));
    assert_eq!(assignments.get("POPUP_C").map(String::as_str), Some("v"));
    assert!(!assignments.contains_key("POPUP_B"));
}
