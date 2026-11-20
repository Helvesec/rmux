//! Application of a pane's accepted `ESC k` request to its canonical window.
//!
//! The policy is tmux 3.7b's, read from the pinned sources and measured
//! against the pinned binary (`arn1/oracle`):
//!
//! * a non-empty name turns the window's own `automatic-rename` off and names
//!   the window with the cleaned request, notifying every time — a name
//!   identical to the current one is still a rename (`input.c:2825-2828`,
//!   `window.c:404-413`);
//! * an empty name removes the window's local `automatic-rename` override and
//!   then, only if the inherited value is still off, names the window with the
//!   empty string; when the inherited value is on, the window merely regains
//!   its aptitude for automatic naming and the automatic producer notifies for
//!   whatever name it computes (`input.c:2820-2824`).

use rmux_proto::{OptionScopeSelector, RmuxError, SessionName, SetOptionMode, WindowTarget};

use super::HandlerState;
use crate::automatic_rename::automatic_rename_enabled;

/// What one accepted request did, for the caller that owns notification.
pub(crate) struct WindowRenameRequestOutcome {
    /// Sessions whose attached clients must re-render.
    pub(crate) refresh_sessions: Vec<SessionName>,
    /// Whether this request renamed the window itself, so `window-renamed` is
    /// due for it. An empty request that only restores automatic naming does
    /// not notify here: the automatic producer owns its own notification.
    pub(crate) renamed: bool,
}

/// tmux's `clean_name` for an untrusted source (`tmux.c:285-298`), reduced to
/// the rules an `ESC k` name can actually reach.
///
/// Two rules fire, in tmux's own order. `#` immediately followed by `(` becomes
/// `_` in the copy, so a pane cannot make its own window name expand a shell
/// command through the format engine. Then `utf8_stravis` with
/// `VIS_OCTAL|VIS_CSTYLE|VIS_TAB|VIS_NL` doubles every backslash, because `vis`
/// escapes a visible backslash whenever `VIS_NOSLASH` is absent — and it is.
/// Measured against the pinned binary: `a\b` is stored and notified as
/// `a\\b`, `c\\d` as `c\\\\d`, and `e\#(f` as `e\\_(f`.
///
/// The remaining `vis` branches cannot fire here, and each has its own reason
/// rather than one blanket claim: the rename table never collects C0
/// (`input/states.rs`), the parser's rename validity gate rejects `DEL` and
/// every bare 8-bit byte, `utf8_strvis` copies a complete multi-byte character
/// verbatim without calling `vis` at all, a space stays visible because
/// `VIS_SP` is not among tmux's flags, and a double quote is copied unchanged
/// because `VIS_DQ` is not either — the last one measured rather than argued.
fn clean_untrusted_window_name(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut cleaned = String::with_capacity(name.len());
    for (index, character) in name.char_indices() {
        match character {
            '#' if bytes.get(index + 1) == Some(&b'(') => cleaned.push('_'),
            '\\' => cleaned.push_str("\\\\"),
            _ => cleaned.push(character),
        }
    }
    cleaned
}

impl HandlerState {
    /// Applies one accepted request to the canonical owner of `target`.
    ///
    /// The caller has already re-resolved the pane's identity, so this only
    /// owns the naming policy and the family synchronization that every other
    /// rename entry point performs.
    pub(crate) fn apply_window_rename_request(
        &mut self,
        target: &WindowTarget,
        request: &str,
    ) -> Result<WindowRenameRequestOutcome, RmuxError> {
        if request.is_empty() {
            self.apply_empty_window_rename_request(target)
        } else {
            self.apply_named_window_rename_request(target, request)
        }
    }

    fn apply_named_window_rename_request(
        &mut self,
        target: &WindowTarget,
        request: &str,
    ) -> Result<WindowRenameRequestOutcome, RmuxError> {
        let name = clean_untrusted_window_name(request);
        let _ = self.rename_window(target.clone(), name)?;
        Ok(WindowRenameRequestOutcome {
            refresh_sessions: self
                .window_linked_session_family_list(target.session_name(), target.window_index()),
            renamed: true,
        })
    }

    fn apply_empty_window_rename_request(
        &mut self,
        target: &WindowTarget,
    ) -> Result<WindowRenameRequestOutcome, RmuxError> {
        // `options_remove_or_default` on the window's own entry: the inherited
        // value decides what happens next, so the override has to go first and
        // the explicit `rename-window` path, which would write a fresh `off`
        // override, cannot be reused here.
        self.options.set_by_name(
            OptionScopeSelector::Window(target.clone()),
            "automatic-rename",
            None,
            SetOptionMode::Replace,
            false,
            true,
            false,
        )?;
        self.synchronize_linked_window_options_from_slot(
            target.session_name(),
            target.window_index(),
        );

        if automatic_rename_enabled(&self.options, target.session_name(), target.window_index()) {
            // Inherited on: the window becomes nameable automatically again.
            // tmux calls no `window_set_name` here, so no `window-renamed` is
            // owed to this request; the automatic producer notifies for the
            // name it computes, if it computes one.
            self.sessions.enable_automatic_window_name(target)?;
            return Ok(WindowRenameRequestOutcome {
                refresh_sessions: self.window_linked_session_family_list(
                    target.session_name(),
                    target.window_index(),
                ),
                renamed: false,
            });
        }

        // Inherited off: the empty name is the window's name, and it notifies.
        let _ = self.sessions.rename_window(target, String::new())?;
        self.clear_auto_named_window_family(target.session_name(), target.window_index());
        self.synchronize_window_alias_family_from_slot(
            target.session_name(),
            target.window_index(),
        )?;
        Ok(WindowRenameRequestOutcome {
            refresh_sessions: self
                .window_linked_session_family_list(target.session_name(), target.window_index()),
            renamed: true,
        })
    }
}
