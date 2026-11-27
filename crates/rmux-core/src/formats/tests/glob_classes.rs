//! A glob class whose first member is a literal `]`, and the syntax of `[`.
//!
//! Every expectation here is the pinned tmux 3.7b, measured before any line of
//! this lot: `format.c:4486-4512` sends `m:` to `fnmatch(3)` and only `m/r:`
//! and `m/ri:` to a separate regex, and `tmux.1:6160-6175` points at glob(7),
//! where a `]` written first inside a class is a MEMBER and `!` first is the
//! negation. The oracle answered, on the titles `job-]` and `job-x`:
//!
//! ```text
//! #{pane_id}|#{m:job-[]],#{pane_title}}   %0|1   %1|0
//! #{pane_id}|#{m:job-[!]],#{pane_title}}  %0|0   %1|1
//! #{m:[[],[}                             1
//! ```
//!
//! LC_ALL=C and ASCII throughout. Named POSIX classes are not measured here and
//! stay outside this lot.

use super::super::glob::format_fnmatch;
use super::super::FormatModifier;

/// The plain `m:` modifier - no flags, so `fnmatch` with none either.
fn plain() -> FormatModifier {
    FormatModifier {
        modifier: "m".to_owned(),
        argv: Vec::new(),
    }
}

/// The `m/i:` modifier, which folds case.
fn folded() -> FormatModifier {
    FormatModifier {
        modifier: "m".to_owned(),
        argv: vec!["i".to_owned()],
    }
}

fn matches(pattern: &str, text: &str) -> bool {
    format_fnmatch(pattern, text, &plain())
}

#[test]
fn a_closing_bracket_written_first_is_a_member_of_the_class() {
    // The journey itself: the tool titled `job-]` is the one the class finds,
    // and the other is not.
    assert!(matches("job-[]]", "job-]"), "job-[]] must find job-]");
    assert!(!matches("job-[]]", "job-x"), "job-[]] must not find job-x");

    // Negated, the same first `]` is still the member being excluded.
    assert!(
        !matches("job-[!]]", "job-]"),
        "job-[!]] must not find job-]"
    );
    assert!(matches("job-[!]]", "job-x"), "job-[!]] must find job-x");

    // The member list continues past that first `]`, so the class is neither
    // empty nor terminated by it.
    assert!(matches("[]x]", "]"), "] is a member");
    assert!(matches("[]x]", "x"), "and so is x");
    assert!(!matches("[]x]", "y"), "y is not");
    assert!(matches("[^]x]", "y"), "^ negates the same list");
    assert!(!matches("[^]x]", "]"), "so ] is excluded");

    // A `]` that is NOT first keeps closing the class.
    assert!(matches("[x]", "x"));
    assert!(
        !matches("[x]", "]"),
        "a trailing ] closes, it does not join"
    );
}

#[test]
fn an_opening_bracket_opens_a_class_before_it_is_a_literal() {
    // `[[]` is the class whose only member is `[`. Reading the `[` as a literal
    // first made this fail, and let `[!]]` slip past the class parser.
    assert!(matches("[[]", "["), "[[] must find [");
    assert!(!matches("[[]", "]"), "[[] must not find ]");
    assert!(matches("a[[]b", "a[b"), "and it composes inside a pattern");

    // A class holding both brackets, written the only way glob(7) allows.
    assert!(matches("[][]", "["), "an opening bracket is a member");
    assert!(matches("[][]", "]"), "and so is ]");
    assert!(!matches("[][]", "x"), "x is not");

    // The negated form the journey uses, against an opening bracket.
    assert!(matches("[!]]", "["), "[ is not the excluded ]");
}

#[test]
fn the_neighbours_this_touches_keep_their_meaning() {
    // Stars, marks and ranges are untouched by the class reading.
    assert!(matches("*", "anything"));
    assert!(matches("job-*", "job-]"));
    assert!(matches("job-?", "job-]"));
    assert!(matches("[a-c]", "b"));
    assert!(!matches("[a-c]", "d"));
    assert!(matches("[!a-c]", "d"));
    assert!(matches("a*c", "abbbc"));
    assert!(!matches("a*c", "abbb"));

    // A class that is never closed is not a class; the oracle keeps the
    // pattern unmatched rather than inventing a member list.
    assert!(
        !matches("[abc", "a"),
        "an unterminated class matches nothing"
    );
    assert!(
        !matches("job-[", "job-["),
        "including at the end of a pattern"
    );

    // Case folding still reaches the class, and only through the `i` flag.
    assert!(format_fnmatch("[A]", "a", &folded()));
    assert!(!format_fnmatch("[A]", "a", &plain()));
    assert!(format_fnmatch("JOB-[]]", "job-]", &folded()));
}
