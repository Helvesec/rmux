use std::collections::VecDeque;

use rmux_proto::RmuxError;

pub(super) fn rebuild_shell_command(command_parts: Vec<String>) -> String {
    if command_parts.len() == 1 {
        return command_parts
            .into_iter()
            .next()
            .expect("single shell token");
    }

    command_parts
        .into_iter()
        .map(shell_command_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_command_token(token: String) -> String {
    format!("'{}'", token.replace('\'', "'\\''"))
}

pub(super) struct CommandTokens {
    tokens: VecDeque<String>,
}

impl CommandTokens {
    pub(super) fn new(tokens: Vec<String>) -> Self {
        Self {
            tokens: tokens.into_iter().collect(),
        }
    }

    pub(super) fn required(&mut self, description: &str) -> Result<String, RmuxError> {
        self.tokens
            .pop_front()
            .ok_or_else(|| RmuxError::Server(format!("missing {description}")))
    }

    pub(super) fn optional(&mut self) -> Option<String> {
        self.tokens.pop_front()
    }

    pub(super) fn peek(&self) -> Option<&str> {
        self.tokens.front().map(String::as_str)
    }

    pub(super) fn optional_compact_flags(&mut self, allowed: &str) -> Option<Vec<char>> {
        let token = self.peek()?;
        if !token.starts_with('-') || token == "-" || token == "--" || token.len() <= 2 {
            return None;
        }
        let flags = token.strip_prefix('-')?;
        if !flags.chars().all(|flag| allowed.contains(flag)) {
            return None;
        }
        let token = self.optional().expect("peeked flag token must exist");
        Some(
            token
                .strip_prefix('-')
                .expect("validated compact flag token")
                .chars()
                .collect(),
        )
    }

    pub(super) fn peek_is_flag(&self) -> bool {
        self.tokens
            .front()
            .is_some_and(|token| token.starts_with('-') && token != "-")
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub(super) fn remaining(self) -> Vec<String> {
        self.tokens.into_iter().collect()
    }

    pub(super) fn remaining_joined(self) -> String {
        self.tokens.into_iter().collect::<Vec<_>>().join(" ")
    }

    pub(super) fn no_extra(&self, command: &str) -> Result<(), RmuxError> {
        if let Some(extra) = self.tokens.front() {
            return Err(RmuxError::Server(format!(
                "unexpected argument '{extra}' for {command}"
            )));
        }
        Ok(())
    }
}
