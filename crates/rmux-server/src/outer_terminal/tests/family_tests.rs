use super::super::{OuterTerminal, OuterTerminalContext};
use rmux_core::OptionStore;

fn assert_family(contexts: &[&[(&str, &str)]], expected: &str) {
    for context in contexts {
        let terminal = OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::from_pairs(context),
        );
        assert_eq!(terminal.features_string(), expected, "context: {context:?}");
    }
}

#[test]
fn default_mintty_features() {
    assert_family(
        &[
            &[("TERM", "mintty")],
            &[("TERM", "plain"), ("TERM_PROGRAM", "MiNtTy")],
        ],
        "256,bpaste,ccolour,clipboard,cstyle,extkeys,margins,mouse,overline,RGB,sixel,strikethrough,title,usstyle",
    );
}

#[test]
fn default_tmux_features() {
    assert_family(
        &[&[("TERM", "tmux-256color")]],
        "256,bpaste,ccolour,clipboard,hyperlinks,cstyle,extkeys,focus,mouse,overline,RGB,strikethrough,title,usstyle",
    );
}

#[test]
fn default_rxvt_unicode_features() {
    assert_family(
        &[&[("TERM", "rxvt-unicode-256color")]],
        "256,bpaste,ccolour,cstyle,focus,ignorefkeys,mouse,title",
    );
}

#[test]
fn default_iterm2_features() {
    assert_family(
        &[
            &[("TERM", "iterm")],
            &[("TERM", "plain"), ("TERM_PROGRAM", "iTeRm.ApP")],
        ],
        "256,bpaste,clipboard,hyperlinks,cstyle,extkeys,margins,mouse,osc7,RGB,strikethrough,sync,title,usstyle",
    );
}

#[test]
fn default_foot_features() {
    assert_family(
        &[&[("TERM", "foot")], &[("TERM", "foot-extra")]],
        "256,bpaste,clipboard,cstyle,extkeys,focus,mouse,RGB,sixel,strikethrough,title",
    );
}

#[test]
fn default_mlterm_features() {
    assert_family(
        &[
            &[("TERM", "mlterm")],
            &[("TERM", "plain"), ("TERM_PROGRAM", "MlTeRm")],
        ],
        "256,bpaste,clipboard,cstyle,extkeys,mouse,RGB,sixel,strikethrough,title",
    );
}

#[test]
fn default_kitty_features() {
    assert_family(
        &[
            &[("TERM", "xterm-kitty")],
            &[("TERM", "xterm-ghostty")],
            &[("TERM", "plain"), ("TERM_PROGRAM", "GhOsTtY")],
        ],
        "256,bpaste,ccolour,clipboard,hyperlinks,kitty-graphics,cstyle,extkeys,focus,margins,mouse,osc7,overline,RGB,strikethrough,sync,title,usstyle",
    );
    assert_family(
        &[
            &[("TERM", "wezterm")],
            &[("TERM", "plain"), ("TERM_PROGRAM", "WeZtErM")],
        ],
        "256,bpaste,ccolour,clipboard,hyperlinks,kitty-graphics,cstyle,extkeys,focus,margins,mouse,osc7,overline,RGB,sixel,strikethrough,sync,title,usstyle",
    );
}

#[test]
fn default_xterm_features() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::default(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    // Default declarations are reported; fallback-only capabilities remain usable.
    assert_eq!(
        terminal.features_string(),
        "bpaste,ccolour,clipboard,cstyle,focus,title"
    );
    assert!(terminal.supports_256);
    assert!(terminal.supports_rgb);
    assert!(terminal.supports_mouse);
    assert!(terminal.clipboard_template.is_some());
    assert!(terminal.cursor_colour_template.is_some());
    assert!(terminal.cursor_style_template.is_some());
    assert!(terminal.enable_extkeys.is_some());
    assert!(terminal.supports_strikethrough);
}
