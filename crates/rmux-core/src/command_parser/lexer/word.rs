use super::{LexToken, Lexer};
use crate::command_parser::{is_parse_time_assignment, CommandParseError};

impl<'a> Lexer<'a> {
    pub(super) fn read_token(&mut self, ch: char) -> Result<String, CommandParseError> {
        let mut buffer = String::new();
        let mut state = QuoteState::None;
        let mut last = QuoteState::Start;
        let mut current = Some(ch);
        let mut delimiter = None;

        while let Some(mut ch) = current {
            if state == QuoteState::None && ch == '\r' {
                ch = match self.get_char() {
                    Some('\n') => '\n',
                    Some(next) => {
                        self.unget_char(next);
                        '\r'
                    }
                    None => '\r',
                };
            }

            if ch == '\n' {
                if state == QuoteState::None {
                    delimiter = Some(ch);
                    break;
                }
                self.line += 1;
            }

            if state == QuoteState::None && matches!(ch, ' ' | '\t') {
                delimiter = Some(ch);
                break;
            }
            if state == QuoteState::None && matches!(ch, ';' | '}') {
                delimiter = Some(ch);
                break;
            }

            if ch == '\n' && state != QuoteState::None {
                buffer.push('\n');
                current = self.get_char_after_quoted_newline();
                continue;
            }

            if ch == '\\' && state != QuoteState::SingleQuotes {
                #[cfg(windows)]
                if can_start_windows_path_prefix(&buffer)
                    && self.read_windows_path_prefix(&mut buffer)
                {
                    last = state;
                    current = self.get_char();
                    continue;
                }
                self.read_escape(&mut buffer)?;
                last = state;
                current = self.get_char();
                continue;
            }

            if ch == '~' && last != state && state != QuoteState::SingleQuotes {
                self.read_tilde(&mut buffer)?;
                last = state;
                current = self.get_char();
                continue;
            }

            if ch == '$' && state != QuoteState::SingleQuotes {
                self.read_variable(&mut buffer)?;
                last = state;
                current = self.get_char();
                continue;
            }

            if ch == '}' && state == QuoteState::None {
                return Err(CommandParseError::new(self.line, "unmatched }"));
            }

            if ch == '\'' {
                match state {
                    QuoteState::None => {
                        state = QuoteState::SingleQuotes;
                        current = self.get_char();
                        continue;
                    }
                    QuoteState::SingleQuotes => {
                        state = QuoteState::None;
                        current = self.get_char();
                        continue;
                    }
                    _ => {}
                }
            }

            if ch == '"' {
                match state {
                    QuoteState::None => {
                        state = QuoteState::DoubleQuotes;
                        current = self.get_char();
                        continue;
                    }
                    QuoteState::DoubleQuotes => {
                        state = QuoteState::None;
                        current = self.get_char();
                        continue;
                    }
                    _ => {}
                }
            }

            buffer.push(ch);
            last = state;
            current = self.get_char();
        }

        if let Some(ch) = delimiter {
            self.unget_char(ch);
        }
        Ok(buffer)
    }

    #[cfg(windows)]
    fn read_windows_path_prefix(&mut self, buffer: &mut String) -> bool {
        let Some(second) = self.get_char() else {
            return false;
        };
        let mut consumed = vec![second];
        if second != '\\' {
            self.restore_consumed_chars(&consumed);
            return false;
        }

        let Some(third) = self.get_char() else {
            self.restore_consumed_chars(&consumed);
            return false;
        };
        consumed.push(third);

        // Accept the standard escaped spelling (`\\\\server\\share`) by
        // consuming the four leading source backslashes as one UNC prefix.
        if third == '\\' {
            let Some(fourth) = self.get_char() else {
                self.restore_consumed_chars(&consumed);
                return false;
            };
            consumed.push(fourth);
            if fourth == '\\' {
                buffer.push_str(r"\\");
                return true;
            }
            self.restore_consumed_chars(&consumed);
            return false;
        }

        // Native Windows configs commonly use the literal spelling
        // (`\\server\share` or `\\?\C:\...`). Only classify it as a path
        // when another separator exists before the token boundary, preserving
        // ordinary leading escaped text such as `\\n`.
        let mut found_separator = false;
        while let Some(next) = self.get_char() {
            consumed.push(next);
            if matches!(next, '\\' | '/') {
                found_separator = true;
                break;
            }
            if matches!(next, '"' | '\'' | ' ' | '\t' | '\r' | '\n' | ';' | '}') {
                break;
            }
        }

        if found_separator {
            self.restore_consumed_chars(&consumed[1..]);
            buffer.push_str(r"\\");
            true
        } else {
            self.restore_consumed_chars(&consumed);
            false
        }
    }

    #[cfg(windows)]
    fn restore_consumed_chars(&mut self, consumed: &[char]) {
        for ch in consumed.iter().rev() {
            self.unget_char(*ch);
        }
    }

    fn get_char_after_quoted_newline(&mut self) -> Option<char> {
        let ch = loop {
            match self.get_char() {
                Some(' ' | '\t') => continue,
                Some(ch) => break ch,
                None => return None,
            }
        };

        if ch != '#' {
            return Some(ch);
        }

        let next = self.get_char()?;
        if matches!(next, ',' | '#' | '{' | '}' | ':') {
            self.unget_char(next);
            return Some('#');
        }

        while let Some(current) = self.get_char() {
            if current == '\n' {
                return Some(current);
            }
        }
        None
    }

    fn read_escape(&mut self, buffer: &mut String) -> Result<(), CommandParseError> {
        let Some(ch) = self.get_char() else {
            return Err(CommandParseError::new(self.line, "invalid escape"));
        };

        #[cfg(windows)]
        if is_windows_path_escape_context(buffer) && !matches!(ch, '\\' | '\'' | '"') {
            buffer.push('\\');
            buffer.push(ch);
            return Ok(());
        }

        if matches!(ch, '4'..='7') {
            return Err(CommandParseError::new(self.line, "invalid octal escape"));
        }
        if matches!(ch, '0'..='3') {
            let Some(o2) = self.get_char() else {
                return Err(CommandParseError::new(self.line, "invalid octal escape"));
            };
            let Some(o3) = self.get_char() else {
                return Err(CommandParseError::new(self.line, "invalid octal escape"));
            };
            if !matches!(o2, '0'..='7') || !matches!(o3, '0'..='7') {
                return Err(CommandParseError::new(self.line, "invalid octal escape"));
            }
            let value = 64 * (ch as u32 - '0' as u32)
                + 8 * (o2 as u32 - '0' as u32)
                + (o3 as u32 - '0' as u32);
            buffer.push(char::from_u32(value).expect("three octal digits fit in char"));
            return Ok(());
        }

        match ch {
            'a' => buffer.push('\x07'),
            'b' => buffer.push('\x08'),
            'e' => buffer.push('\x1b'),
            'f' => buffer.push('\x0c'),
            's' => buffer.push(' '),
            'v' => buffer.push('\x0b'),
            'r' => buffer.push('\r'),
            'n' => buffer.push('\n'),
            't' => buffer.push('\t'),
            'u' => self.read_unicode_escape(buffer, 4, 'u')?,
            'U' => self.read_unicode_escape(buffer, 8, 'U')?,
            other => buffer.push(other),
        }

        Ok(())
    }

    fn read_unicode_escape(
        &mut self,
        buffer: &mut String,
        size: usize,
        escape_type: char,
    ) -> Result<(), CommandParseError> {
        let mut digits = String::with_capacity(size);
        for _ in 0..size {
            let Some(ch) = self.get_char() else {
                return Err(CommandParseError::new(
                    self.line,
                    format!("invalid \\{escape_type} argument"),
                ));
            };
            if ch == '\n' || !ch.is_ascii_hexdigit() {
                return Err(CommandParseError::new(
                    self.line,
                    format!("invalid \\{escape_type} argument"),
                ));
            }
            digits.push(ch);
        }

        let value = u32::from_str_radix(&digits, 16).map_err(|_| {
            CommandParseError::new(self.line, format!("invalid \\{escape_type} argument"))
        })?;
        let Some(character) = char::from_u32(value) else {
            return Err(CommandParseError::new(
                self.line,
                format!("invalid \\{escape_type} argument"),
            ));
        };
        buffer.push(character);
        Ok(())
    }

    fn read_variable(&mut self, buffer: &mut String) -> Result<(), CommandParseError> {
        let Some(ch) = self.get_char() else {
            return Err(CommandParseError::new(
                self.line,
                "invalid environment variable",
            ));
        };

        let mut name = String::new();
        let brackets = ch == '{';
        if !brackets {
            if !is_var_char(ch, true) {
                buffer.push('$');
                self.unget_char(ch);
                return Ok(());
            }
            name.push(ch);
        }

        loop {
            let Some(next) = self.get_char() else {
                if !brackets {
                    break;
                }
                return Err(CommandParseError::new(
                    self.line,
                    "invalid environment variable",
                ));
            };
            if brackets && next == '}' {
                break;
            }
            if !is_var_char(next, false) {
                if !brackets {
                    self.unget_char(next);
                    break;
                }
                return Err(CommandParseError::new(
                    self.line,
                    "invalid environment variable",
                ));
            }
            if name.len() >= 1022 {
                return Err(CommandParseError::new(
                    self.line,
                    "environment variable is too long",
                ));
            }
            name.push(next);
        }

        if let Some(value) = self.lookup_environment(&name) {
            buffer.push_str(value);
        }
        Ok(())
    }

    fn read_tilde(&mut self, buffer: &mut String) -> Result<(), CommandParseError> {
        let mut user = String::new();
        let mut delimiter = None;
        while let Some(ch) = self.get_char() {
            if matches!(ch, '/' | ' ' | '\t' | '\n' | '"' | '\'') {
                self.unget_char(ch);
                if user.is_empty() && ch != '/' {
                    buffer.push('~');
                    return Ok(());
                }
                delimiter = Some(ch);
                break;
            }
            if user.len() >= 1022 {
                return Err(CommandParseError::new(self.line, "user name is too long"));
            }
            user.push(ch);
        }

        if user.is_empty() && delimiter.is_none() {
            buffer.push('~');
            return Ok(());
        }

        let Some(home) = self.expand_tilde(&user) else {
            return Err(CommandParseError::new(
                self.line,
                format!("unknown user: ~{user}"),
            ));
        };
        buffer.push_str(home);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    Start,
    None,
    DoubleQuotes,
    SingleQuotes,
}

pub(super) fn classify_word(word: String) -> LexToken {
    if is_parse_time_assignment(&word) {
        return LexToken::Equals(word);
    }

    LexToken::Token(word)
}

fn is_var_char(ch: char, first: bool) -> bool {
    if ch == '=' {
        return false;
    }
    if first && ch.is_ascii_digit() {
        return false;
    }
    ch.is_ascii_alphanumeric() || ch == '_'
}

#[cfg(windows)]
fn is_windows_path_escape_context(buffer: &str) -> bool {
    let value = windows_path_value(buffer);

    if value.starts_with(r"\\") {
        return true;
    }

    if has_windows_drive_prefix(value) {
        return true;
    }

    if matches!(value, "." | "..") || value.starts_with(".\\") || value.starts_with("..\\") {
        return true;
    }

    if value
        .chars()
        .last()
        .is_some_and(|ch| matches!(ch, '\\' | '/'))
    {
        return true;
    }

    let mut chars = value.chars().rev();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(':'), Some(drive), None) if drive.is_ascii_alphabetic()
    )
}

#[cfg(windows)]
fn can_start_windows_path_prefix(buffer: &str) -> bool {
    windows_path_value(buffer).is_empty()
}

#[cfg(windows)]
fn windows_path_value(buffer: &str) -> &str {
    // Keep the path prefix independent from a single-token option name such as
    // `--script=.\scripts\tool.ps1`.
    buffer.split_once('=').map_or(buffer, |(_, value)| value)
}

#[cfg(windows)]
fn has_windows_drive_prefix(buffer: &str) -> bool {
    let mut chars = buffer.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic()
    )
}
