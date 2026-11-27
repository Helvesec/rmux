use super::event::{Error, STACK_LIMIT};

/// Mask of enhancement flags understood by this codec profile.
pub const KNOWN_FLAGS: u32 = 31;

/// Screen whose independent keyboard stack is being addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    /// Main terminal screen.
    Main,
    /// Alternate terminal screen.
    Alternate,
}

/// Operation used by a set-flags control frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Apply {
    /// Replace the active flags.
    Replace,
    /// Add supplied flags to the active flags.
    Add,
    /// Remove supplied flags from the active flags.
    Remove,
}

/// One parsed Kitty keyboard state control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    /// Query the requested flags on the selected screen.
    Query,
    /// Mutate the active stack entry.
    Set {
        /// Requested flags before masking to [`KNOWN_FLAGS`].
        flags: u32,
        /// Set operation.
        mode: Apply,
    },
    /// Push a new active stack entry.
    Push(u32),
    /// Pop this many entries from the selected stack.
    Pop(u32),
}

/// Observable pure-state effect of applying a keyboard control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// No reply is produced.
    None,
    /// Reply with the requested known flags.
    QueryReply(u8),
}

impl Effect {
    /// Encodes the effect as a complete application reply frame.
    #[must_use]
    pub fn bytes(self) -> Vec<u8> {
        match self {
            Self::None => Vec::new(),
            Self::QueryReply(flags) => format!("\x1b[?{flags}u").into_bytes(),
        }
    }
}

/// Independent bounded Kitty keyboard flag stacks for one pane.
///
/// The caller supplies [`Screen`] so this type does not duplicate active-screen
/// authority. It contains no pane identity, runtime generation, or routing
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneKeyboard {
    main: Vec<u8>,
    alternate: Vec<u8>,
}

impl Default for PaneKeyboard {
    fn default() -> Self {
        Self {
            main: vec![0],
            alternate: vec![0],
        }
    }
}

impl PaneKeyboard {
    /// Builds state from oldest-to-active stacks without truncation.
    pub fn from_stacks(main: Vec<u8>, alternate: Vec<u8>) -> Result<Self, Error> {
        if main.len() > STACK_LIMIT || alternate.len() > STACK_LIMIT {
            return Err(Error::StackLimit);
        }
        if main
            .iter()
            .chain(&alternate)
            .any(|flags| u32::from(*flags) > KNOWN_FLAGS)
        {
            return Err(Error::InvalidFrame);
        }
        Ok(Self { main, alternate })
    }

    /// Returns the selected oldest-to-active stack.
    #[must_use]
    pub fn stack(&self, screen: Screen) -> &[u8] {
        match screen {
            Screen::Main => &self.main,
            Screen::Alternate => &self.alternate,
        }
    }

    /// Returns the active requested flags, or zero for an empty stack.
    #[must_use]
    pub fn requested(&self, screen: Screen) -> u8 {
        self.stack(screen).last().copied().unwrap_or(0)
    }

    /// Returns encoding-effective flags while preserving requested query state.
    ///
    /// Associated-text reporting (bit 16) is ineffective without all-key
    /// reporting (bit 8) under the selected policy.
    #[must_use]
    pub fn effective(&self, screen: Screen) -> u8 {
        effective_flags(self.requested(screen))
    }

    /// Applies one parsed control to the selected screen stack.
    pub fn apply(&mut self, screen: Screen, control: Control) -> Effect {
        let old = self.requested(screen);
        let stack = match screen {
            Screen::Main => &mut self.main,
            Screen::Alternate => &mut self.alternate,
        };

        match control {
            Control::Query => return Effect::QueryReply(old),
            Control::Set { flags, mode } => {
                let flags = known_flags(flags);
                let new = match mode {
                    Apply::Replace => flags,
                    Apply::Add => old | flags,
                    Apply::Remove => old & !flags,
                };
                if let Some(active) = stack.last_mut() {
                    *active = new;
                } else {
                    stack.push(new);
                }
            }
            Control::Push(flags) => {
                if stack.len() == STACK_LIMIT {
                    stack.remove(0);
                }
                stack.push(known_flags(flags));
            }
            Control::Pop(count) => {
                let count = usize::try_from(count).unwrap_or(usize::MAX);
                stack.truncate(stack.len().saturating_sub(count));
            }
        }
        Effect::None
    }

    /// Applies a full-reset state transition to both keyboard stacks.
    pub fn ris(&mut self) {
        *self = Self::default();
    }

    /// Applies a soft-reset state transition, which retains keyboard stacks.
    pub const fn decstr(&mut self) {}
}

pub(super) fn effective_flags(requested: u8) -> u8 {
    if requested & 8 == 0 {
        requested & !16
    } else {
        requested
    }
}

fn known_flags(flags: u32) -> u8 {
    (flags & KNOWN_FLAGS) as u8
}
