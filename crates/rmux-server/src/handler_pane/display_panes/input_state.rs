use super::super::super::{
    attach_support::{DisplayPanesClientState, DisplayPanesLabel},
    prompt_support::PromptInputEvent,
};

pub(super) enum DisplayPanesOutcome {
    Stay,
    Close,
    Select(DisplayPanesLabel),
}

pub(super) fn update_display_panes_state(
    state: &mut DisplayPanesClientState,
    event: PromptInputEvent,
) -> DisplayPanesOutcome {
    match event {
        PromptInputEvent::Char(ch) if ch.is_ascii_alphanumeric() => {
            state.input.push(ch.to_ascii_lowercase());
            match match_display_panes_label(state) {
                DisplayPanesMatch::Exact(_label, true) => DisplayPanesOutcome::Stay,
                DisplayPanesMatch::Exact(label, false) => DisplayPanesOutcome::Select(label),
                DisplayPanesMatch::Prefix => DisplayPanesOutcome::Stay,
                DisplayPanesMatch::None => DisplayPanesOutcome::Close,
            }
        }
        PromptInputEvent::Backspace => {
            state.input.pop();
            DisplayPanesOutcome::Stay
        }
        PromptInputEvent::Enter => match match_display_panes_label(state) {
            DisplayPanesMatch::Exact(label, _) => DisplayPanesOutcome::Select(label),
            DisplayPanesMatch::Prefix | DisplayPanesMatch::None => DisplayPanesOutcome::Close,
        },
        PromptInputEvent::Escape | PromptInputEvent::Ctrl('c') => DisplayPanesOutcome::Close,
        _ => DisplayPanesOutcome::Close,
    }
}

enum DisplayPanesMatch {
    Exact(DisplayPanesLabel, bool),
    Prefix,
    None,
}

fn match_display_panes_label(state: &DisplayPanesClientState) -> DisplayPanesMatch {
    if state.input.is_empty() {
        return DisplayPanesMatch::Prefix;
    }

    let exact = state
        .labels
        .iter()
        .find(|label| label.label == state.input)
        .cloned();
    let has_longer_prefix = state.labels.iter().any(|label| {
        label.label.starts_with(&state.input) && label.label.len() > state.input.len()
    });
    if let Some(label) = exact {
        return DisplayPanesMatch::Exact(label, has_longer_prefix);
    }
    if state
        .labels
        .iter()
        .any(|label| label.label.starts_with(&state.input))
    {
        DisplayPanesMatch::Prefix
    } else {
        DisplayPanesMatch::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{PaneTarget, SessionName, WindowTarget};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session")
    }

    #[test]
    fn accepts_letter_alias_labels() {
        let alpha = session_name("alpha");
        let mut state = DisplayPanesClientState {
            id: 1,
            window: WindowTarget::with_window(alpha.clone(), 0),
            labels: vec![DisplayPanesLabel {
                label: "a".to_owned(),
                target: PaneTarget::with_window(alpha, 0, 10),
                target_string: "=alpha:0.%10".to_owned(),
            }],
            input: String::new(),
            template: None,
            clear_frame: Vec::new(),
        };

        match update_display_panes_state(&mut state, PromptInputEvent::Char('A')) {
            DisplayPanesOutcome::Select(label) => assert_eq!(label.label, "a"),
            DisplayPanesOutcome::Stay => panic!("letter alias should select"),
            DisplayPanesOutcome::Close => panic!("letter alias should not close"),
        }
    }
}
