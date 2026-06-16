use rmux_core::{key_code_lookup_bits, key_string_lookup_key, KeyCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) enum PromptInputEvent {
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Ctrl(char),
    KeyName(String),
}

impl PromptInputEvent {
    pub(super) fn key_string(&self) -> Option<String> {
        match self {
            Self::Char(ch) => Some(ch.to_string()),
            Self::Enter => Some("Enter".to_owned()),
            Self::Escape => Some("Escape".to_owned()),
            Self::Tab => Some("Tab".to_owned()),
            Self::Backspace => Some("BSpace".to_owned()),
            Self::Delete => Some("DC".to_owned()),
            Self::Left => Some("Left".to_owned()),
            Self::Right => Some("Right".to_owned()),
            Self::Up => Some("Up".to_owned()),
            Self::Down => Some("Down".to_owned()),
            Self::Home => Some("Home".to_owned()),
            Self::End => Some("End".to_owned()),
            Self::Ctrl(ch) => Some(format!("C-{ch}")),
            Self::KeyName(name) => Some(name.clone()),
        }
    }
}

pub(in super::super) fn decode_prompt_key(key: KeyCode) -> PromptInputEvent {
    let name = key_string_lookup_key(key_code_lookup_bits(key), false).to_owned();
    match name.as_str() {
        "Left" => PromptInputEvent::Left,
        "Right" => PromptInputEvent::Right,
        "Up" => PromptInputEvent::Up,
        "Down" => PromptInputEvent::Down,
        "Home" => PromptInputEvent::Home,
        "End" => PromptInputEvent::End,
        "DC" => PromptInputEvent::Delete,
        "Enter" => PromptInputEvent::Enter,
        "BSpace" => PromptInputEvent::Backspace,
        "Escape" => PromptInputEvent::Escape,
        _ => PromptInputEvent::KeyName(name),
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{KEYC_CTRL, KEYC_META};

    use super::{decode_prompt_key, PromptInputEvent};

    #[test]
    fn decode_prompt_key_preserves_meta_shortcuts() {
        assert_eq!(
            decode_prompt_key(u64::from(b'a') | KEYC_META),
            PromptInputEvent::KeyName("M-a".to_owned())
        );
    }

    #[test]
    fn decode_prompt_key_keeps_control_events_canonical() {
        assert_eq!(
            decode_prompt_key(u64::from(b't') | KEYC_CTRL),
            PromptInputEvent::KeyName("C-t".to_owned())
        );
    }
}
