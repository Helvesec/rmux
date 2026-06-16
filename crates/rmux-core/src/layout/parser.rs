use rmux_proto::RmuxError;

use super::{LayoutCell, LayoutDirection, LayoutGeometry};

const MAX_LAYOUT_PARSE_DEPTH: usize = 128;

pub(super) struct LayoutParser<'a> {
    input: &'a str,
    offset: usize,
}

impl<'a> LayoutParser<'a> {
    pub(super) const fn new(input: &'a str) -> Self {
        Self { input, offset: 0 }
    }

    pub(super) fn is_eof(&self) -> bool {
        self.offset == self.input.len()
    }

    pub(super) fn parse_cell(&mut self) -> Result<LayoutCell, RmuxError> {
        self.parse_cell_at_depth(0)
    }

    fn parse_cell_at_depth(&mut self, depth: usize) -> Result<LayoutCell, RmuxError> {
        if depth > MAX_LAYOUT_PARSE_DEPTH {
            return Err(RmuxError::Server("layout is too deeply nested".to_owned()));
        }

        let width = self.parse_number()?;
        self.expect('x')?;
        let height = self.parse_number()?;
        self.expect(',')?;
        let x = self.parse_number()?;
        self.expect(',')?;
        let y = self.parse_number()?;
        if self.peek_char() == Some(',') {
            let saved = self.offset;
            self.offset += 1;
            let _ = self.parse_number()?;
            if self.peek_char() == Some('x') {
                self.offset = saved;
            }
        }

        let geometry = LayoutGeometry::new(width, height, x, y);
        match self.peek_char() {
            Some('{') => {
                self.offset += 1;
                let mut children = Vec::new();
                loop {
                    children.push(self.parse_cell_at_depth(depth + 1)?);
                    match self.peek_char() {
                        Some(',') => {
                            self.offset += 1;
                        }
                        Some('}') => {
                            self.offset += 1;
                            break;
                        }
                        _ => return Err(RmuxError::Server("invalid layout".to_owned())),
                    }
                }
                Ok(LayoutCell::split(
                    LayoutDirection::LeftRight,
                    geometry,
                    children,
                ))
            }
            Some('[') => {
                self.offset += 1;
                let mut children = Vec::new();
                loop {
                    children.push(self.parse_cell_at_depth(depth + 1)?);
                    match self.peek_char() {
                        Some(',') => {
                            self.offset += 1;
                        }
                        Some(']') => {
                            self.offset += 1;
                            break;
                        }
                        _ => return Err(RmuxError::Server("invalid layout".to_owned())),
                    }
                }
                Ok(LayoutCell::split(
                    LayoutDirection::TopBottom,
                    geometry,
                    children,
                ))
            }
            Some(',') | Some('}') | Some(']') | None => Ok(LayoutCell::pane(geometry)),
            _ => Err(RmuxError::Server("invalid layout".to_owned())),
        }
    }

    fn parse_number(&mut self) -> Result<u32, RmuxError> {
        let start = self.offset;
        while self
            .peek_char()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.offset += 1;
        }
        if self.offset == start {
            return Err(RmuxError::Server("invalid layout".to_owned()));
        }
        self.input[start..self.offset]
            .parse::<u32>()
            .map_err(|_| RmuxError::Server("invalid layout".to_owned()))
    }

    fn expect(&mut self, expected: char) -> Result<(), RmuxError> {
        match self.peek_char() {
            Some(character) if character == expected => {
                self.offset += character.len_utf8();
                Ok(())
            }
            _ => Err(RmuxError::Server("invalid layout".to_owned())),
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }
}
