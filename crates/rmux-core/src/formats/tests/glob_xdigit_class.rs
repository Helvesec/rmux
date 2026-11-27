//! `[:xdigit:]`, and the tool whose title ends in a hexadecimal digit.
//!
//! The journey is the inventory that finds it: two readers, one titled `job-F`
//! and one titled `job-G` - one letter apart, on either side of the end of the
//! hexadecimal alphabet - and
//! `#{pane_id}|#{pane_title}|#{m:job-[[:xdigit:]],#{pane_title}}` marks the
//! first and not the second, with each title read back between delimiters.
//!
//! Nothing here is adopted: no xdigit case was found in the targeted texts, so
//! this lot measured its own destination as well, on its own oracle image,
//! before this file existed - `gxdigit1/cells/o1`, twenty-five
//! cases:
//!
//! ```text
//! #{m:job-[[:xdigit:]],job-F}  1    ,job-G}  0
//! 0 1   9 1   A 1   a 1   f 1
//! / 0   : 0   @ 0   ` 0   g 0   ! 0   space 0
//! no character 0    two characters 0
//! #{m/i:…,job-F} 1    #{m/i:…,job-G} 0
//! #{m:job-[[:XDIGIT:]],job-F} 0    folded 0
//! #{m:job-[![:xdigit:]],job-F} 0    ,job-G} 1
//! #{m:job-[[:xdigit:][:nosuch:]],job-F} 1   #{m:job-[[:nosuch:][:xdigit:]],job-F} 0
//! ```
//!
//! The four boundaries are the point of that list: `/` and `:` sit either side
//! of the digits, `@` below `A`, and the backtick below `a`, with `g` just past
//! `f`. `isxdigit(3)` on this machine says the same twenty-two characters
//! "regardless of locale" - a manual page, not a measurement, and the engine
//! was asked anyway. What the witnesses add beyond the measured rows - a sweep
//! of the ASCII block, one code point at a time - is this reading's own
//! whitelist, and nothing is claimed for any other locale or for Unicode.

use super::super::glob::format_fnmatch;
use super::super::FormatModifier;

/// The plain `m:` modifier - no flags, so `fnmatch` with none either.
fn plain() -> FormatModifier {
    FormatModifier {
        modifier: "m".to_owned(),
        argv: Vec::new(),
    }
}

/// The `m/i:` modifier, the only one that folds case.
fn folded() -> FormatModifier {
    FormatModifier {
        modifier: "m".to_owned(),
        argv: vec!["i".to_owned()],
    }
}

fn matches(pattern: &str, text: &str) -> bool {
    format_fnmatch(pattern, text, &plain())
}

fn matches_folded(pattern: &str, text: &str) -> bool {
    format_fnmatch(pattern, text, &folded())
}

#[test]
fn a_hexadecimal_suffix_finds_its_tool_and_the_letter_past_it_does_not() {
    assert!(
        matches("job-[[:xdigit:]]", "job-F"),
        "F is a hexadecimal digit"
    );
    assert!(
        !matches("job-[[:xdigit:]]", "job-G"),
        "G is one letter too far"
    );

    // One class stands for one character, no more and no fewer.
    assert!(!matches("job-[[:xdigit:]]", "job-"));
    assert!(!matches("job-[[:xdigit:]]", "job-Ff"));
    assert!(
        matches("job-[[:xdigit:]]*", "job-Ff"),
        "a star takes the rest"
    );
}

#[test]
fn the_three_ranges_and_the_four_boundaries_around_them() {
    // Measured members, one row each.
    for member in ['0', '9', 'A', 'a', 'f'] {
        assert!(
            matches("job-[[:xdigit:]]", &format!("job-{member}")),
            "job-{member} is a hexadecimal digit"
        );
    }
    // Measured boundaries: the characters immediately outside the three ranges.
    for outside in ['/', ':', '@', '`', 'g', '!', ' '] {
        assert!(
            !matches("job-[[:xdigit:]]", &format!("job-{outside}")),
            "job-{outside:?} is outside them"
        );
    }

    // Extended by this reading, not by the oracle: the whole ASCII block, one
    // code point at a time, against the three ranges.
    let mut members = 0;
    for code in 0x00u8..=0x7f {
        let ch = char::from(code);
        let is_member =
            ch.is_ascii_digit() || ('A'..='F').contains(&ch) || ('a'..='f').contains(&ch);
        if is_member {
            members += 1;
        }
        assert_eq!(
            matches("job-[[:xdigit:]]", &format!("job-{ch}")),
            is_member,
            "job-{ch:?} against the hexadecimal class"
        );
    }
    assert_eq!(members, 22, "ten digits and twelve letters");
}

#[test]
fn the_flag_changes_nothing_and_the_name_xdigit_keeps_its_case() {
    // Measured: the folded flag answers exactly as the plain one does - it does
    // not fold `G` into a member, and it does not take `F` away.
    assert!(matches_folded("job-[[:xdigit:]]", "job-F"));
    assert!(!matches_folded("job-[[:xdigit:]]", "job-G"));
    assert!(matches_folded("job-[[:xdigit:]]", "job-f"));

    // The name written XDIGIT is not one this reading knows, folded or not.
    assert!(!matches("job-[[:XDIGIT:]]", "job-F"));
    assert!(!matches_folded("job-[[:XDIGIT:]]", "job-F"));
    assert!(
        !matches("job-[[:Xdigit:]]", "job-F"),
        "nor any other spelling"
    );

    // The negation, and the order in which a list is consumed.
    assert!(!matches("job-[![:xdigit:]]", "job-F"));
    assert!(matches("job-[![:xdigit:]]", "job-G"));
    assert!(matches("job-[[:xdigit:][:nosuch:]]", "job-F"));
    assert!(!matches("job-[[:nosuch:][:xdigit:]]", "job-F"));
    assert!(
        matches("job-[x[:xdigit:]]", "job-x"),
        "a literal member first"
    );

    // And the guards the other names keep.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job-F"));
    assert!(
        !matches("job-[[:xdigit:]-x]", "job-F"),
        "nor may xdigit open a range"
    );
}
