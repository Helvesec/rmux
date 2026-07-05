use super::lexer::{LexToken, Lexer, SpannedToken};
use super::{
    CommandArgument, CommandGrouping, CommandParseError, EnvironmentAssignment, ParsedCommand,
    ParsedCommands,
};

// Real tmux bounds parser recursion with YYMAXDEPTH; rmux's recursive-descent
// parser must too. `{ … }` blocks (parse_command) and `%if … %endif` branches
// (parse_condition) both re-enter parse_until, so without a cap an attacker can
// nest either one arbitrarily deep and overflow the stack (a SIGABRT DoS reachable
// from control-mode). Cap the shared recursion node and fail closed with a parse
// error well before the native stack is exhausted. Keep this below the known
// 2 MiB worker-stack overflow range for later recursive tree passes too.
const MAX_NESTING_DEPTH: usize = 124;

pub(super) struct GrammarParser<'a> {
    lexer: Lexer<'a>,
    grouping: CommandGrouping,
    command_line_mode: CommandLineMode,
    peeked: Option<SpannedToken>,
    depth: usize,
}

impl<'a> GrammarParser<'a> {
    pub(super) fn new(lexer: Lexer<'a>, grouping: CommandGrouping) -> Self {
        Self {
            lexer,
            grouping,
            command_line_mode: CommandLineMode::Start,
            peeked: None,
            depth: 0,
        }
    }

    pub(super) fn new_source_file(lexer: Lexer<'a>, grouping: CommandGrouping) -> Self {
        Self {
            lexer,
            grouping,
            command_line_mode: CommandLineMode::End,
            peeked: None,
            depth: 0,
        }
    }

    pub(super) fn parse_all(&mut self) -> Result<ParsedCommands, CommandParseError> {
        self.parse_until(&[], false, true)
    }

    fn parse_until(
        &mut self,
        stop_directives: &[ConditionStop],
        stop_on_close_brace: bool,
        active: bool,
    ) -> Result<ParsedCommands, CommandParseError> {
        // Bound every recursion cycle (braces and %if branches both re-enter here).
        // On overflow we fail closed; the error aborts the whole parse, so the
        // matching decrement before the Ok return only needs to keep sibling
        // blocks at the same nesting level from accumulating depth.
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err(CommandParseError::structural(
                self.peek_line().unwrap_or(0),
                "command nesting too deep",
            ));
        }
        let mut commands = ParsedCommands::with_grouping(self.grouping);

        loop {
            match self.peek()? {
                LexToken::Eof => break,
                LexToken::CloseBrace if stop_on_close_brace => break,
                LexToken::CloseBrace => {
                    return Err(CommandParseError::structural(
                        self.peek_line()?,
                        "unmatched }",
                    ));
                }
                LexToken::Else if stop_directives.contains(&ConditionStop::Else) => break,
                LexToken::Elif if stop_directives.contains(&ConditionStop::Elif) => break,
                LexToken::Endif if stop_directives.contains(&ConditionStop::Endif) => break,
                LexToken::Else | LexToken::Elif | LexToken::Endif => {
                    return Err(CommandParseError::structural(
                        self.peek_line()?,
                        "unexpected condition directive",
                    ));
                }
                LexToken::Newline | LexToken::Semicolon => {
                    self.advance()?;
                }
                LexToken::Hidden => {
                    if let Some(assignment) = self.parse_hidden_assignment(active)? {
                        commands.push_assignment(assignment);
                    }
                }
                LexToken::Equals(_) => {
                    let (assignment, command) = self.parse_assignment_or_command(active)?;
                    if let Some(assignment) = assignment {
                        commands.push_assignment(assignment);
                    }
                    if active {
                        if let Some(command) = command {
                            commands.push_command(command);
                        }
                    }
                }
                LexToken::If => {
                    commands.append(self.parse_condition(active)?);
                }
                LexToken::Token(_) => {
                    let command = self.parse_command(active)?;
                    if active {
                        commands.push_command(command);
                    }
                }
                LexToken::OpenBrace => {
                    return Err(CommandParseError::structural(
                        self.peek_line()?,
                        "unexpected {",
                    ));
                }
                LexToken::Format(_) => {
                    return Err(CommandParseError::structural(
                        self.peek_line()?,
                        "unexpected format",
                    ));
                }
            }
        }

        self.depth -= 1;
        Ok(commands)
    }

    fn parse_hidden_assignment(
        &mut self,
        active: bool,
    ) -> Result<Option<EnvironmentAssignment>, CommandParseError> {
        self.expect_hidden()?;
        let equals = self.expect_equals("%hidden must be followed by name=value")?;
        self.ensure_statement_boundary("%hidden name=value must be a complete statement")?;
        let assignment = EnvironmentAssignment::from_equals(equals, true);
        if active {
            self.lexer.put_assignment(&assignment);
            Ok(Some(assignment))
        } else {
            Ok(None)
        }
    }

    fn parse_assignment_or_command(
        &mut self,
        active: bool,
    ) -> Result<(Option<EnvironmentAssignment>, Option<ParsedCommand>), CommandParseError> {
        let assignment = EnvironmentAssignment::from_equals(
            self.expect_equals("expected name=value assignment")?,
            false,
        );
        if active {
            self.lexer.put_assignment(&assignment);
        }
        let command = match self.peek()? {
            LexToken::Token(_) => Some(self.parse_command(active)?),
            LexToken::Newline
            | LexToken::Semicolon
            | LexToken::CloseBrace
            | LexToken::Eof
            | LexToken::Else
            | LexToken::Elif
            | LexToken::Endif => None,
            _ => {
                return Err(CommandParseError::structural(
                    self.peek_line()?,
                    "name=value assignment must be followed by a command or statement boundary",
                ))
            }
        };
        Ok((active.then_some(assignment), command))
    }

    fn parse_condition(&mut self, active: bool) -> Result<ParsedCommands, CommandParseError> {
        self.expect_if()?;
        let condition = self.expect_condition_value("%if must be followed by a condition")?;
        let mut selected = ParsedCommands::with_grouping(self.grouping);
        let mut matched = self.lexer.context.condition_is_true(&condition);

        let true_branch = self.parse_until(
            &[
                ConditionStop::Else,
                ConditionStop::Elif,
                ConditionStop::Endif,
            ],
            false,
            active && matched,
        )?;
        if matched {
            selected = true_branch;
        }

        while matches!(self.peek()?, LexToken::Elif) {
            self.advance()?;
            let condition = self.expect_condition_value("%elif must be followed by a condition")?;
            let branch_matches = !matched && self.lexer.context.condition_is_true(&condition);
            let branch = self.parse_until(
                &[
                    ConditionStop::Else,
                    ConditionStop::Elif,
                    ConditionStop::Endif,
                ],
                false,
                active && branch_matches,
            )?;
            if branch_matches {
                matched = true;
                selected = branch;
            }
        }

        if matches!(self.peek()?, LexToken::Else) {
            self.advance()?;
            let branch_active = active && !matched;
            let branch = self.parse_until(&[ConditionStop::Endif], false, branch_active)?;
            if branch_active {
                selected = branch;
            }
        }

        self.expect_endif()?;
        Ok(selected)
    }

    fn parse_command(&mut self, active: bool) -> Result<ParsedCommand, CommandParseError> {
        let (name, start_line) = self.expect_token_with_line("expected command name")?;
        let mut arguments = Vec::new();

        loop {
            match self.peek()? {
                LexToken::Token(_) => {
                    arguments.push(CommandArgument::String(
                        self.expect_token("expected argument")?,
                    ));
                }
                LexToken::Equals(_) => {
                    arguments.push(CommandArgument::String(
                        self.expect_equals("expected argument")?,
                    ));
                }
                LexToken::OpenBrace => {
                    self.advance()?;
                    let nested = self.parse_until(&[], true, active)?;
                    self.expect_close_brace()?;
                    arguments.push(CommandArgument::Commands(nested));
                }
                LexToken::Newline
                | LexToken::Semicolon
                | LexToken::CloseBrace
                | LexToken::Eof
                | LexToken::Else
                | LexToken::Elif
                | LexToken::Endif => break,
                LexToken::Hidden | LexToken::If | LexToken::Format(_) => {
                    return Err(CommandParseError::structural(
                        self.peek_line()?,
                        "unexpected token in command",
                    ));
                }
            }
        }

        let line = match self.command_line_mode {
            CommandLineMode::Start => start_line,
            CommandLineMode::End => self.peek_line()?,
        };
        Ok(ParsedCommand::with_lines(name, arguments, start_line, line))
    }

    fn peek(&mut self) -> Result<LexToken, CommandParseError> {
        self.peek_token().map(|token| token.token.clone())
    }

    fn peek_line(&mut self) -> Result<usize, CommandParseError> {
        self.peek_token().map(|token| token.line)
    }

    fn advance(&mut self) -> Result<(), CommandParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lexer.next_token()?);
        }
        self.peeked = None;
        Ok(())
    }

    fn peek_token(&mut self) -> Result<&SpannedToken, CommandParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lexer.next_token()?);
        }
        Ok(self.peeked.as_ref().expect("peeked token is populated"))
    }

    fn expect_hidden(&mut self) -> Result<(), CommandParseError> {
        match self.peek()? {
            LexToken::Hidden => {
                self.advance()?;
                Ok(())
            }
            _ => Err(CommandParseError::structural(
                self.peek_line()?,
                "expected %hidden",
            )),
        }
    }

    fn expect_if(&mut self) -> Result<(), CommandParseError> {
        match self.peek()? {
            LexToken::If => {
                self.advance()?;
                Ok(())
            }
            _ => Err(CommandParseError::structural(
                self.peek_line()?,
                "expected %if",
            )),
        }
    }

    fn expect_endif(&mut self) -> Result<(), CommandParseError> {
        match self.peek()? {
            LexToken::Endif => {
                self.advance()?;
                Ok(())
            }
            _ => Err(CommandParseError::structural(
                self.peek_line()?,
                "expected %endif",
            )),
        }
    }

    fn expect_close_brace(&mut self) -> Result<(), CommandParseError> {
        match self.peek()? {
            LexToken::CloseBrace => {
                self.advance()?;
                Ok(())
            }
            _ => Err(CommandParseError::structural(
                self.peek_line()?,
                "missing }",
            )),
        }
    }

    fn expect_condition_value(&mut self, error: &str) -> Result<String, CommandParseError> {
        match self.peek()? {
            LexToken::Token(value) | LexToken::Format(value) => {
                self.advance()?;
                Ok(value)
            }
            _ => Err(CommandParseError::structural(self.peek_line()?, error)),
        }
    }

    fn expect_token(&mut self, error: &str) -> Result<String, CommandParseError> {
        self.expect_token_with_line(error).map(|(value, _)| value)
    }

    fn expect_token_with_line(
        &mut self,
        error: &str,
    ) -> Result<(String, usize), CommandParseError> {
        let line = self.peek_line()?;
        match self.peek()? {
            LexToken::Token(value) => {
                self.advance()?;
                Ok((value, line))
            }
            _ => Err(CommandParseError::structural(line, error)),
        }
    }

    fn expect_equals(&mut self, error: &str) -> Result<String, CommandParseError> {
        match self.peek()? {
            LexToken::Equals(value) => {
                self.advance()?;
                Ok(value)
            }
            _ => Err(CommandParseError::structural(self.peek_line()?, error)),
        }
    }

    fn ensure_statement_boundary(&mut self, error: &str) -> Result<(), CommandParseError> {
        match self.peek()? {
            LexToken::Newline
            | LexToken::Semicolon
            | LexToken::CloseBrace
            | LexToken::Eof
            | LexToken::Else
            | LexToken::Elif
            | LexToken::Endif => Ok(()),
            _ => Err(CommandParseError::structural(self.peek_line()?, error)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionStop {
    Else,
    Elif,
    Endif,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandLineMode {
    Start,
    End,
}
