//! `#{sixel_support}` — the one format variable that describes the running
//! binary instead of a session, window, pane, or client.
//!
//! Expectations come from the pinned tmux 3.7b oracle, measured on a private
//! socket with `-f /dev/null`, and they pin both the *shape* — always a single
//! digit, never empty, identical on every entry path, unmoved by any client —
//! and the *digit*, because the digit is a claim about this server binary that
//! a client is entitled to act on.

use super::super::{render_template, resolve::sixel_support_value};
use super::{FormatContext, FormatVariable, FormatVariables};
use crate::SIXEL_SUPPORT;

/// The digit RMUX owes its clients until it has tmux's server-side SIXEL
/// authority.
///
/// Measured on the pinned oracle pair, both built from one source tree: without
/// `ENABLE_SIXEL` the token is `0` and primary DA is `ESC[?1;2c`; with
/// `--enable-sixel` the token is `1` and DA is `ESC[?1;2;4c`. That flag buys a
/// retained, scalable, croppable, redrawable image store in screen state, SIXEL
/// parsed ahead of the passthrough gate, and a text fallback for clients that
/// cannot render images.
///
/// RMUX has none of it. It parses a bare-`q` DCS and forwards the bytes
/// transiently to one attached client, only when `allow-passthrough` — off by
/// default — is on and only when that client's outer terminal advertises
/// `sixel`; nothing is retained, so nothing is replayed to a later subscriber
/// or redrawn after a resize, the web transport discards every DCS, and the
/// child-facing primary DA stays non-SIXEL. This build is therefore in the
/// oracle's unsupported state, and says so.
const UNSUPPORTED: &str = "0";

/// A context that claims every name, including `sixel_support`.
///
/// tmux oracle: `setenv -g sixel_support ZZZ` then `#{sixel_support}` still
/// prints `0`, because `format_find` consults `format_table[]` before the
/// per-format tree and the environment. A context value must not win here.
struct ShadowingValues;

impl FormatVariables for ShadowingValues {
    fn format_value(&self, _variable: FormatVariable) -> Option<String> {
        Some("SHADOW".to_owned())
    }

    fn format_value_by_name(&self, _name: &str) -> Option<String> {
        Some("SHADOW".to_owned())
    }
}

#[test]
fn sixel_support_resolves_with_no_session_window_pane_or_client() {
    // tmux oracle B1/B3: `display-message -p '#{sixel_support}'` answers `0`
    // with a detached session and no client at all — never the empty string.
    assert_eq!(
        render_template("#{sixel_support}", &FormatContext::new()),
        UNSUPPORTED
    );
    assert_eq!(
        render_template("X#{sixel_support}Y", &FormatContext::new()),
        format!("X{UNSUPPORTED}Y")
    );
}

#[test]
fn sixel_support_answers_a_single_digit_in_the_zero_one_domain() {
    // tmux `format_cb_sixel_support` returns exactly "1" or "0", never the
    // empty string that the advertised-but-unimplemented token used to expand
    // to. The domain is kept whole so a future build that earns the capability
    // renders `1` by flipping the constant alone.
    assert_eq!(sixel_support_value(true), "1");
    assert_eq!(sixel_support_value(false), "0");

    // What this build claims. The resolver renders the constant, and the
    // constant must report the oracle's unsupported state.
    assert_eq!(sixel_support_value(SIXEL_SUPPORT), UNSUPPORTED);
}

#[test]
fn sixel_support_is_not_shadowed_by_context_values() {
    assert_eq!(
        render_template("#{sixel_support}", &ShadowingValues),
        UNSUPPORTED
    );
    // Control: every other name still comes from the context.
    assert_eq!(
        render_template("#{session_name}", &ShadowingValues),
        "SHADOW"
    );
}

#[test]
fn sixel_support_feeds_conditionals_comparisons_and_modifiers() {
    let context = FormatContext::new();

    // Pinned tmux 3.7b oracle, unsupported build: conditionals `NO|ZERO`,
    // comparisons `1|0`, modifiers `1|0    |0|0`. The digit drives truthiness,
    // comparison, length, padding, basename and quoting like any other table
    // string — `0` is a defined false, not the empty string the parent
    // expanded to, so `#{==:#{sixel_support},0}` is the answerable probe an
    // application actually writes.
    assert_eq!(render_template("#{?sixel_support,YES,NO}", &context), "NO");
    assert_eq!(
        render_template("#{?#{==:#{sixel_support},0},ZERO,OTHER}", &context),
        "ZERO"
    );
    assert_eq!(render_template("#{==:#{sixel_support},0}", &context), "1");
    assert_eq!(render_template("#{==:#{sixel_support},1}", &context), "0");
    assert_eq!(render_template("#{!=:#{sixel_support},0}", &context), "0");
    assert_eq!(render_template("#{n:sixel_support}", &context), "1");
    assert_eq!(
        render_template("#{p5:sixel_support}", &context),
        format!("{UNSUPPORTED}    ")
    );
    assert_eq!(render_template("#{b:sixel_support}", &context), UNSUPPORTED);
    assert_eq!(render_template("#{q:sixel_support}", &context), UNSUPPORTED);
}

#[test]
fn sixel_support_survives_nested_and_literal_forms() {
    let context = FormatContext::new();

    // tmux oracle B7: `#{l:...}` keeps the token literal, and F14/F16: a value
    // still holding the token is only expanded through `#{E:...}`.
    assert_eq!(
        render_template("#{l:#{sixel_support}}", &context),
        "#{sixel_support}"
    );
    assert_eq!(
        render_template(
            "#{E:@alias}",
            &FormatContext::new().with_named_value("@alias", "#{sixel_support}")
        ),
        UNSUPPORTED
    );
}

#[test]
fn unknown_tokens_stay_empty_next_to_the_resolved_capability() {
    // tmux oracle B9/F25: an unknown name expands to nothing. The new build
    // capability arm must not turn the generic lookup into a catch-all.
    assert_eq!(
        render_template("A#{no_such_token_x}B", &FormatContext::new()),
        "AB"
    );
    // A near-miss of the resolved name must not match: the arm is exact.
    assert_eq!(
        render_template("A#{sixel_supported}B", &FormatContext::new()),
        "AB"
    );
}
