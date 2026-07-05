use super::{CommandParseError, CommandParser, EnvironmentAssignment};

#[path = "lexer/word.rs"]
mod word;

use self::word::classify_word;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LexToken {
    Token(String),
    Equals(String),
    Format(String),
    Hidden,
    If,
    Else,
    Elif,
    Endif,
    Semicolon,
    OpenBrace,
    CloseBrace,
    Newline,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpannedToken {
    pub(super) token: LexToken,
    pub(super) line: usize,
}

pub(super) struct Lexer<'a> {
    input: Vec<char>,
    offset: usize,
    ungot: Vec<char>,
    pending_backslashes: usize,
    hash_format_mode: HashFormatMode,
    local_environment: Vec<(String, String)>,
    condition: bool,
    eol: bool,
    eof: bool,
    line: usize,
    pub(super) context: &'a CommandParser,
}

impl<'a> Lexer<'a> {
    pub(super) fn new(input: &str, context: &'a CommandParser) -> Self {
        Self::with_hash_format_mode(input, context, HashFormatMode::TokenOutsideCondition)
    }

    pub(super) fn new_source_file(input: &str, context: &'a CommandParser) -> Self {
        Self::with_hash_format_mode(input, context, HashFormatMode::CommentOutsideCondition)
    }

    fn with_hash_format_mode(
        input: &str,
        context: &'a CommandParser,
        hash_format_mode: HashFormatMode,
    ) -> Self {
        Self {
            input: Self::normalize_newlines(input),
            offset: 0,
            ungot: Vec::new(),
            pending_backslashes: 0,
            hash_format_mode,
            local_environment: Vec::new(),
            condition: false,
            eol: false,
            eof: false,
            line: 1,
            context,
        }
    }

    /// Collapse Windows (`\r\n`) and old-Mac (`\r`) line endings to `\n` so the
    /// rest of the lexer only ever sees LF. Without this, a trailing `\r` from a
    /// CRLF-saved config is swallowed into the preceding word by `read_word`, and
    /// backslash line-continuation (which only checks for `\n`) fails on `\r\n`.
    fn normalize_newlines(input: &str) -> Vec<char> {
        let mut chars = Vec::with_capacity(input.len());
        let mut iter = input.chars().peekable();
        while let Some(ch) = iter.next() {
            if ch == '\r' {
                if iter.peek() == Some(&'\n') {
                    iter.next();
                }
                chars.push('\n');
            } else {
                chars.push(ch);
            }
        }
        chars
    }

    pub(super) fn next_token(&mut self) -> Result<SpannedToken, CommandParseError> {
        if self.eol {
            self.line += 1;
        }
        self.eol = false;

        let condition = self.condition;
        self.condition = false;

        loop {
            let Some(mut ch) = self.get_char() else {
                if self.eof {
                    return Ok(self.spanned(LexToken::Eof));
                }
                self.eof = true;
                return Ok(self.spanned(LexToken::Newline));
            };

            if matches!(ch, ' ' | '\t') {
                continue;
            }

            if ch == '\r' {
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
                self.eol = true;
                return Ok(self.spanned(LexToken::Newline));
            }

            match ch {
                ';' => return Ok(self.spanned(LexToken::Semicolon)),
                '{' => return Ok(self.spanned(LexToken::OpenBrace)),
                '}' => return Ok(self.spanned(LexToken::CloseBrace)),
                '#' => {
                    let next = self.get_char();
                    if condition && next == Some('{') {
                        let token = self.read_format()?;
                        return Ok(self.spanned(token));
                    }
                    if next == Some('{')
                        && self.hash_format_mode == HashFormatMode::TokenOutsideCondition
                    {
                        let token = match self.read_format()? {
                            LexToken::Format(value) => LexToken::Token(value),
                            _ => unreachable!("read_format returns a format token"),
                        };
                        return Ok(self.spanned(token));
                    }
                    if self.consume_comment(next) {
                        self.line += 1;
                    }
                    return Ok(self.spanned(LexToken::Newline));
                }
                '%' => {
                    let token = self.read_percent_word()?;
                    return Ok(self.spanned(token));
                }
                _ => {
                    let token = self.read_token(ch)?;
                    return Ok(self.spanned(classify_word(token)));
                }
            }
        }
    }

    fn spanned(&self, token: LexToken) -> SpannedToken {
        SpannedToken {
            token,
            line: self.line,
        }
    }

    fn raw_get_char(&mut self) -> Option<char> {
        let ch = self.input.get(self.offset).copied()?;
        self.offset += 1;
        Some(ch)
    }

    fn raw_unget_char(&mut self) {
        if self.offset > 0 {
            self.offset -= 1;
        }
    }

    fn get_char(&mut self) -> Option<char> {
        if let Some(ch) = self.ungot.pop() {
            return Some(ch);
        }
        if self.pending_backslashes != 0 {
            self.pending_backslashes -= 1;
            return Some('\\');
        }

        loop {
            let ch = self.raw_get_char()?;
            if ch != '\\' {
                return Some(ch);
            }

            let mut count = 1;
            while self.input.get(self.offset) == Some(&'\\') {
                self.offset += 1;
                count += 1;
            }

            let next = self.raw_get_char();
            if next == Some('\n') && count % 2 == 1 {
                self.line += 1;
                count -= 1;
                if count != 0 {
                    self.pending_backslashes = count - 1;
                    return Some('\\');
                }
                continue;
            }

            if next.is_some() {
                self.raw_unget_char();
            }
            self.pending_backslashes = count - 1;
            return Some('\\');
        }
    }

    fn unget_char(&mut self, ch: char) {
        self.ungot.push(ch);
    }

    pub(super) fn put_assignment(&mut self, assignment: &EnvironmentAssignment) {
        self.local_environment
            .push((assignment.name.clone(), assignment.value.clone()));
    }

    fn lookup_environment(&self, name: &str) -> Option<&str> {
        self.local_environment
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
            .or_else(|| self.context.lookup_environment(name))
    }

    fn expand_tilde(&self, user: &str) -> Option<&str> {
        if user.is_empty() {
            return self
                .lookup_environment("HOME")
                .filter(|home| !home.is_empty())
                .or_else(|| self.context.expand_tilde(user));
        }

        self.context.expand_tilde(user)
    }

    fn consume_comment(&mut self, first: Option<char>) -> bool {
        let mut ch = first;
        while let Some(current) = ch {
            if current == '\n' {
                return true;
            }
            ch = self.get_char();
        }
        false
    }

    fn read_percent_word(&mut self) -> Result<LexToken, CommandParseError> {
        let word = self.read_word('%');
        if word.chars().all(|ch| ch == '%' || ch.is_ascii_digit()) {
            return Ok(LexToken::Token(word));
        }

        match word.as_str() {
            "%hidden" => {
                self.condition = true;
                Ok(LexToken::Hidden)
            }
            "%if" => {
                self.condition = true;
                Ok(LexToken::If)
            }
            "%else" => Ok(LexToken::Else),
            "%elif" => {
                self.condition = true;
                Ok(LexToken::Elif)
            }
            "%endif" => Ok(LexToken::Endif),
            _ => Err(CommandParseError::structural(self.line, "syntax error")),
        }
    }

    fn read_word(&mut self, first: char) -> String {
        let mut word = String::from(first);
        while let Some(ch) = self.get_char() {
            if matches!(ch, ' ' | '\t' | '\n') {
                self.unget_char(ch);
                break;
            }
            word.push(ch);
        }
        word
    }

    fn read_format(&mut self) -> Result<LexToken, CommandParseError> {
        let mut value = String::from("#{");
        let mut brackets = 1_u32;

        loop {
            let Some(ch) = self.get_char() else {
                return Err(CommandParseError::new(self.line, "invalid format"));
            };
            if ch == '\n' {
                return Err(CommandParseError::new(self.line, "invalid format"));
            }

            if ch == '#' {
                let Some(next) = self.get_char() else {
                    return Err(CommandParseError::new(self.line, "invalid format"));
                };
                if next == '\n' {
                    return Err(CommandParseError::new(self.line, "invalid format"));
                }
                if next == '{' {
                    brackets = brackets
                        .checked_add(1)
                        .ok_or_else(|| CommandParseError::new(self.line, "invalid format"))?;
                }
                value.push('#');
                value.push(next);
                continue;
            }

            if ch == '}' {
                brackets -= 1;
                value.push(ch);
                if brackets == 0 {
                    return Ok(LexToken::Format(value));
                }
                continue;
            }

            value.push(ch);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HashFormatMode {
    TokenOutsideCondition,
    CommentOutsideCondition,
}
