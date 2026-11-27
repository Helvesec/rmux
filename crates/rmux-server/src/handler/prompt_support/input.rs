use rmux_core::{
    key_code_lookup_bits, key_string_lookup_key, KeyCode, KEYC_MASK_KEY, KEYC_MASK_MODIFIERS,
};

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

    /// A control letter as one event, whichever way it arrived. A C0 byte
    /// from the terminal decodes to `Ctrl`, but a key code (send-keys, the
    /// xterm modifyOtherKeys form) decodes to its name, `C-n`. tmux 3.7b reads
    /// both as the one key `'n'|KEYC_CTRL`.
    pub(in super::super) fn with_control_letter_as_ctrl(self) -> Self {
        let Self::KeyName(name) = &self else {
            return self;
        };
        let mut letter = name.strip_prefix("C-").unwrap_or_default().chars();
        match (letter.next(), letter.next()) {
            (Some(letter), None) if letter.is_ascii_lowercase() => Self::Ctrl(letter),
            _ => self,
        }
    }
}

pub(in super::super) fn decode_prompt_key(key: KeyCode) -> PromptInputEvent {
    let key = key_code_lookup_bits(key);
    if key & KEYC_MASK_MODIFIERS == 0 {
        let codepoint = key & KEYC_MASK_KEY;
        if let Ok(codepoint) = u32::try_from(codepoint) {
            if let Some(character) = char::from_u32(codepoint).filter(|ch| !ch.is_control()) {
                return PromptInputEvent::Char(character);
            }
        }
    }

    let name = key_string_lookup_key(key, false).to_owned();
    match name.as_str() {
        "Left" => PromptInputEvent::Left,
        "Right" => PromptInputEvent::Right,
        "Up" => PromptInputEvent::Up,
        "Down" => PromptInputEvent::Down,
        "Home" => PromptInputEvent::Home,
        "End" => PromptInputEvent::End,
        "DC" => PromptInputEvent::Delete,
        "Enter" => PromptInputEvent::Enter,
        "Tab" => PromptInputEvent::Tab,
        "BSpace" => PromptInputEvent::Backspace,
        "Escape" => PromptInputEvent::Escape,
        _ => PromptInputEvent::KeyName(name),
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{key_string_lookup_string, KEYC_CTRL, KEYC_META, KEYC_SHIFT};

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

    #[test]
    fn decode_prompt_key_projects_normalized_modify_other_keys_printables() {
        for character in ['A', '9', ' ', 'é'] {
            assert_eq!(
                decode_prompt_key(character as u64),
                PromptInputEvent::Char(character),
                "normalized printable {character:?}"
            );
        }
    }

    #[test]
    fn decode_prompt_key_preserves_special_keys_and_residual_modifiers() {
        for (name, expected) in [
            ("Enter", PromptInputEvent::Enter),
            ("Escape", PromptInputEvent::Escape),
            ("Tab", PromptInputEvent::Tab),
            ("BSpace", PromptInputEvent::Backspace),
        ] {
            let key = key_string_lookup_string(name).expect("special key parses");
            assert_eq!(decode_prompt_key(key), expected, "special key {name}");
        }
        assert_eq!(
            decode_prompt_key(u64::from(b'a') | KEYC_SHIFT),
            PromptInputEvent::KeyName("S-a".to_owned())
        );
    }
    #[test]
    fn named_input_preserves_key_modifiers() {
        for name in ["C-q", "M-q", "C-M-Left", "S-Up"] {
            let key = key_string_lookup_string(name).expect("valid key");
            assert_eq!(
                decode_prompt_key(key),
                PromptInputEvent::KeyName(name.to_owned())
            );
        }
        let left = key_string_lookup_string("Left").expect("left key");
        assert_eq!(decode_prompt_key(left), PromptInputEvent::Left);
    }
}
