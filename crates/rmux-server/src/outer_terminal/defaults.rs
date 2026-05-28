pub(super) const DEFAULT_TSL: &str = "\x1b]0;";
pub(super) const DEFAULT_FSL: &str = "\x07";
pub(super) const DEFAULT_SWD: &str = "\x1b]7;";
pub(super) const DEFAULT_MS: &str = "\x1b]52;%p1%s;%p2%s\x07";
pub(super) const DEFAULT_HLS: &str = "\x1b]8;%p1%s;%p2%s\x1b\\";
pub(super) const DEFAULT_SS: &str = "\x1b[%p1%d q";
pub(super) const DEFAULT_SE: &str = "\x1b[2 q";
pub(super) const DEFAULT_CS: &str = "\x1b]12;%p1%s\x07";
pub(super) const DEFAULT_CR: &str = "\x1b]112\x07";
pub(super) const DEFAULT_ENBP: &str = "\x1b[?2004h";
pub(super) const DEFAULT_DSBP: &str = "\x1b[?2004l";
pub(super) const DEFAULT_ENFCS: &str = "\x1b[?1004h";
pub(super) const DEFAULT_DSFCS: &str = "\x1b[?1004l";
pub(super) const DEFAULT_ENEKS: &str = "\x1b[>4;2m";
pub(super) const DEFAULT_DSEKS: &str = "\x1b[>4m";
pub(super) const DEFAULT_ENMG: &str = "\x1b[?69h";
pub(super) const DEFAULT_DSMG: &str = "\x1b[?69l";
pub(super) const DEFAULT_SYNC: &str = "\x1b[?2026%?%p1%{1}%-%tl%eh%;";

pub(super) const DEFAULT_MINTTY_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "ccolour",
    "cstyle",
    "extkeys",
    "margins",
    "overline",
    "usstyle",
    "sixel",
];
pub(super) const DEFAULT_TMUX_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "ccolour",
    "cstyle",
    "extkeys",
    "focus",
    "overline",
    "usstyle",
    "hyperlinks",
];
pub(super) const DEFAULT_RXVT_UNICODE_FEATURES: &[&str] = &[
    "256",
    "bpaste",
    "ccolour",
    "cstyle",
    "mouse",
    "title",
    "ignorefkeys",
];
pub(super) const DEFAULT_ITERM2_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "cstyle",
    "extkeys",
    "margins",
    "usstyle",
    "sync",
    "osc7",
    "hyperlinks",
];
pub(super) const DEFAULT_FOOT_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "cstyle",
    "extkeys",
    "sixel",
];
pub(super) const DEFAULT_MLTERM_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "cstyle",
    "extkeys",
    "sixel",
];
pub(super) const DEFAULT_KITTY_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "ccolour",
    "cstyle",
    "extkeys",
    "focus",
    "margins",
    "overline",
    "usstyle",
    "sync",
    "osc7",
    "hyperlinks",
    "kitty-graphics",
];
pub(super) const DEFAULT_XTERM_FEATURES: &[&str] = &[
    "256",
    "RGB",
    "bpaste",
    "clipboard",
    "mouse",
    "strikethrough",
    "title",
    "ccolour",
    "cstyle",
    "extkeys",
    "focus",
];
