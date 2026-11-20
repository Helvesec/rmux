//! `status-keys vi` editing of the command prompt.
//!
//! This mirrors `status_prompt_translate_key` of the pinned tmux 3.7b source
//! (`status.c:1038-1186`): a prompt carries an entry mode and a command mode,
//! Escape moves from the first to the second, and the command mode maps vi keys
//! onto the editing events the prompt already understands. tmux re-reads
//! `status-keys` for every key (`status.c:1472`), after the key/numeric/single
//! prompts have already returned, so the option is resolved per key here too.
//!
//! The six word motions tmux routes through its own vi word keys (`b`, `B`,
//! `e`, `E`, `w`, `W`) have no prompt event to borrow, so they travel as their
//! own translation and are applied by the three helpers at the bottom of this
//! file, transcribed from `status_prompt_forward_word`,
//! `status_prompt_end_word` and `status_prompt_backward_word`
//! (`status.c:1320-1423`) with the separator lists each key reaches
//! (`status.c:1597-1634`): the effective `word-separators` for a lowercase key
//! and an empty list for an uppercase one.

use super::PromptInputEvent;

/// The effective `status-keys` value for one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum StatusKeys {
    #[default]
    Emacs,
    Vi,
}

impl StatusKeys {
    /// Reads a resolved `status-keys` value; anything but `vi` keeps the
    /// default emacs editing, as the option's choice table already guarantees.
    pub(super) fn from_option_value(value: Option<&str>) -> Self {
        match value {
            Some("vi") => Self::Vi,
            _ => Self::Emacs,
        }
    }
}

/// Which half of the vi prompt the client is in.
///
/// tmux opens every prompt in entry mode (`status.c:731`) and exposes the mode
/// to the renderer and to `#{command_prompt}` (`status.c:882-911`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum PromptEditMode {
    #[default]
    Entry,
    Command,
}

/// One of the three word movements of the vi prompt, with the separator list
/// its key reaches: a lowercase key honours `word-separators`, an uppercase one
/// runs on an empty list, which collapses every non-space run into one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViWordMotion {
    /// `w` and `W`: to the start of the next word.
    NextWordStart { honour_separators: bool },
    /// `e` and `E`: to the end of the current or next word.
    WordEnd { honour_separators: bool },
    /// `b` and `B`: back to the start of the previous word.
    PreviousWordStart { honour_separators: bool },
}

impl ViWordMotion {
    fn honour_separators(self) -> bool {
        match self {
            Self::NextWordStart { honour_separators }
            | Self::WordEnd { honour_separators }
            | Self::PreviousWordStart { honour_separators } => honour_separators,
        }
    }

    /// Where this motion leaves the cursor, in characters.
    pub(super) fn apply(self, buffer: &str, cursor: usize, word_separators: &str) -> usize {
        let chars: Vec<char> = buffer.chars().collect();
        let separators = if self.honour_separators() {
            word_separators
        } else {
            ""
        };
        match self {
            Self::NextWordStart { .. } => next_word_start(&chars, cursor, separators),
            Self::WordEnd { .. } => word_end(&chars, cursor, separators),
            Self::PreviousWordStart { .. } => previous_word_start(&chars, cursor, separators),
        }
    }
}

/// What the table decided for one key, mirroring the three returns of the
/// source translator.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ViTranslation {
    /// Handle this event through the ordinary editing path (`return 1`).
    Event(PromptInputEvent),
    /// Append this key to the buffer (`return 2`).
    Insert(PromptInputEvent),
    /// Escape left entry mode: the caller enters command mode and steps the
    /// cursor back one character.
    EnterCommandMode,
    /// A word movement the prompt applies itself, since no prompt event carries
    /// one (`return 1` onto the source's own vi word keys).
    Motion(ViWordMotion),
    /// The key belonged to the mode itself and edits nothing (`return 0`).
    Consumed,
}

/// Translates one key for `status-keys vi`, updating the mode as the source does.
pub(super) fn translate(mode: &mut PromptEditMode, event: PromptInputEvent) -> ViTranslation {
    match mode {
        PromptEditMode::Entry => translate_entry(mode, event),
        PromptEditMode::Command => translate_command(mode, event),
    }
}

fn translate_entry(mode: &mut PromptEditMode, event: PromptInputEvent) -> ViTranslation {
    match event {
        PromptInputEvent::Escape | PromptInputEvent::Ctrl('[') => {
            *mode = PromptEditMode::Command;
            ViTranslation::EnterCommandMode
        }
        PromptInputEvent::Ctrl(
            'a' | 'c' | 'e' | 'g' | 'h' | 'k' | 'n' | 'p' | 't' | 'u' | 'v' | 'w' | 'y',
        )
        | PromptInputEvent::Tab
        | PromptInputEvent::Enter
        | PromptInputEvent::Backspace
        | PromptInputEvent::Delete
        | PromptInputEvent::Down
        | PromptInputEvent::End
        | PromptInputEvent::Home
        | PromptInputEvent::Left
        | PromptInputEvent::Right
        | PromptInputEvent::Up => ViTranslation::Event(event),
        // The source also lets C-Left and C-Right through untouched; this
        // prompt has no word motion for them, so they reach the same no-op
        // either way.
        PromptInputEvent::KeyName(ref name) if name == "C-Left" || name == "C-Right" => {
            ViTranslation::Event(event)
        }
        // Every other key is appended, which for a control or named key means
        // the prompt ignores it exactly as `append_key` does.
        other => ViTranslation::Insert(other),
    }
}

fn translate_command(mode: &mut PromptEditMode, event: PromptInputEvent) -> ViTranslation {
    if matches!(event, PromptInputEvent::Backspace) {
        return ViTranslation::Event(PromptInputEvent::Left);
    }

    // The keys that return to entry mode, some of them carrying an edit or a
    // movement with them, exactly as the first switch of the source does.
    let returning = match event {
        PromptInputEvent::Char('A') => Some(ViTranslation::Event(PromptInputEvent::End)),
        PromptInputEvent::Char('I') => Some(ViTranslation::Event(PromptInputEvent::Home)),
        PromptInputEvent::Char('C') => Some(ViTranslation::Event(PromptInputEvent::Ctrl('k'))),
        PromptInputEvent::Char('s') => Some(ViTranslation::Event(PromptInputEvent::Delete)),
        PromptInputEvent::Char('a') => Some(ViTranslation::Event(PromptInputEvent::Right)),
        PromptInputEvent::Char('S') => Some(ViTranslation::Event(PromptInputEvent::Ctrl('u'))),
        PromptInputEvent::Char('i') => Some(ViTranslation::Consumed),
        _ => None,
    };
    if let Some(translation) = returning {
        *mode = PromptEditMode::Entry;
        return translation;
    }

    match event {
        // Escape and C-[ stay in command mode and edit nothing.
        PromptInputEvent::Escape | PromptInputEvent::Ctrl('[') => ViTranslation::Consumed,
        PromptInputEvent::Char('$') => ViTranslation::Event(PromptInputEvent::End),
        PromptInputEvent::Char('0' | '^') => ViTranslation::Event(PromptInputEvent::Home),
        PromptInputEvent::Char('D') => ViTranslation::Event(PromptInputEvent::Ctrl('k')),
        PromptInputEvent::Char('X') => ViTranslation::Event(PromptInputEvent::Backspace),
        PromptInputEvent::Char('d') => ViTranslation::Event(PromptInputEvent::Ctrl('u')),
        PromptInputEvent::Char('p') => ViTranslation::Event(PromptInputEvent::Ctrl('y')),
        PromptInputEvent::Char('q') => ViTranslation::Event(PromptInputEvent::Ctrl('c')),
        PromptInputEvent::Char('x') | PromptInputEvent::Delete => {
            ViTranslation::Event(PromptInputEvent::Delete)
        }
        PromptInputEvent::Char('j') | PromptInputEvent::Down => {
            ViTranslation::Event(PromptInputEvent::Down)
        }
        PromptInputEvent::Char('h') | PromptInputEvent::Left => {
            ViTranslation::Event(PromptInputEvent::Left)
        }
        PromptInputEvent::Char('l') | PromptInputEvent::Right => {
            ViTranslation::Event(PromptInputEvent::Right)
        }
        PromptInputEvent::Char('k') | PromptInputEvent::Up => {
            ViTranslation::Event(PromptInputEvent::Up)
        }
        PromptInputEvent::Ctrl('h' | 'c') | PromptInputEvent::Enter => ViTranslation::Event(event),
        PromptInputEvent::Char('b') => ViTranslation::Motion(ViWordMotion::PreviousWordStart {
            honour_separators: true,
        }),
        PromptInputEvent::Char('B') => ViTranslation::Motion(ViWordMotion::PreviousWordStart {
            honour_separators: false,
        }),
        PromptInputEvent::Char('e') => ViTranslation::Motion(ViWordMotion::WordEnd {
            honour_separators: true,
        }),
        PromptInputEvent::Char('E') => ViTranslation::Motion(ViWordMotion::WordEnd {
            honour_separators: false,
        }),
        PromptInputEvent::Char('w') => ViTranslation::Motion(ViWordMotion::NextWordStart {
            honour_separators: true,
        }),
        PromptInputEvent::Char('W') => ViTranslation::Motion(ViWordMotion::NextWordStart {
            honour_separators: false,
        }),
        // Every other key is ignored, as the source does.
        _ => ViTranslation::Consumed,
    }
}

/// Whether this character is the space the source counts as one: a single-byte,
/// single-width ' ' (`status.c:981-988`).
fn is_space(chars: &[char], index: usize) -> bool {
    matches!(chars.get(index), Some(' '))
}

/// Whether this character belongs to the separator list. The source only
/// classifies single-byte, single-width characters, so a multi-byte character is
/// never a separator (`status.c:973-979`); an index past the end reads the
/// buffer's terminator, which is neither a space nor a separator.
fn in_list(separators: &str, chars: &[char], index: usize) -> bool {
    match chars.get(index) {
        Some(&ch) if ch.len_utf8() == 1 => separators.contains(ch),
        _ => false,
    }
}

/// `w` and `W`: the vi branch of `status_prompt_forward_word`
/// (`status.c:1319-1357`), which skips the run of spaces so the cursor lands on
/// the start of the next word.
fn next_word_start(chars: &[char], cursor: usize, separators: &str) -> usize {
    let size = chars.len();
    let mut index = cursor;
    if index == size {
        return index;
    }
    let word_is_separators = in_list(separators, chars, index) && !is_space(chars, index);
    loop {
        index += 1;
        if is_space(chars, index) {
            while index != size && is_space(chars, index) {
                index += 1;
            }
            break;
        }
        if index == size || word_is_separators != in_list(separators, chars, index) {
            break;
        }
    }
    index
}

/// `e` and `E`: `status_prompt_end_word` (`status.c:1359-1394`). Reaching the
/// end of the buffer leaves the cursor one past the last character, exactly as
/// the source does.
fn word_end(chars: &[char], cursor: usize, separators: &str) -> usize {
    let size = chars.len();
    let mut index = cursor;
    if index == size {
        return index;
    }
    loop {
        index += 1;
        if index == size {
            return index;
        }
        if !is_space(chars, index) {
            break;
        }
    }
    let word_is_separators = in_list(separators, chars, index);
    loop {
        index += 1;
        if index == size {
            break;
        }
        if is_space(chars, index) || word_is_separators != in_list(separators, chars, index) {
            break;
        }
    }
    index - 1
}

/// `b` and `B`: `status_prompt_backward_word` (`status.c:1396-1423`).
fn previous_word_start(chars: &[char], cursor: usize, separators: &str) -> usize {
    let mut index = cursor;
    while index != 0 {
        index -= 1;
        if !is_space(chars, index) {
            break;
        }
    }
    let word_is_separators = in_list(separators, chars, index);
    while index != 0 {
        index -= 1;
        if is_space(chars, index) || word_is_separators != in_list(separators, chars, index) {
            index += 1;
            break;
        }
    }
    index
}

#[cfg(test)]
#[path = "../../handler_prompt/vi/tests.rs"]
mod tests;
