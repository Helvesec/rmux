//! `[:space:]`, and the tool whose title ends in one real space.
//!
//! The journey is the inventory that finds it: two readers, one titled `job- ` -
//! `6a6f622d20`, its last byte a space - and one titled `job-!`, and
//! `#{pane_id}|#{pane_title}|#{m:job-[[:space:]],#{pane_title}}` marks the
//! first and not the second, with the title read back between delimiters so
//! that its trailing byte is seen rather than assumed.
//!
//! Nothing here is adopted: no space witness or measurement was found in the
//! acquis, so this lot measured its own destination as well, on its own oracle
//! image, before this file existed - `gspace1/cells/o1`,
//! twenty-two cases under LC_ALL=C, the control characters passed as exact
//! argv and never injected as a title:
//!
//! ```text
//! #{m:job-[[:space:]],job- }    1     ,job-!}   0
//! HT 1   LF 1   VT 1   FF 1   CR 1
//! a 0    7 0    ESC 0   DEL 0    no character 0    two spaces 0
//! #{m/i:…,job- } 1    #{m/i:…,job-a} 0
//! #{m:job-[[:SPACE:]],job- } 0   folded 0
//! #{m:job-[![:space:]],job- } 0   ,job-a} 1
//! #{m:job-[[:space:][:nosuch:]],job- } 1   #{m:job-[[:nosuch:][:space:]],job- } 0
//! ```
//!
//! VERTICAL TAB is the row that decides how this is written. Rust's
//! `is_ascii_whitespace` follows the WhatWG list - SPACE, HT, LF, FF, CR - and
//! says in its own documentation that the POSIX locale includes U+000B as
//! well; the oracle answers 1 for it. So the class is an explicit whitelist of
//! the six, on the character the text carries, and not that helper and not the
//! Unicode question.

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
fn a_title_ending_in_one_space_finds_its_tool() {
    // The destination, with its last byte written out rather than trusted.
    assert_eq!("job- ".as_bytes(), &[0x6a, 0x6f, 0x62, 0x2d, 0x20]);
    assert!(matches("job-[[:space:]]", "job- "), "job- ends in a space");
    assert!(!matches("job-[[:space:]]", "job-!"), "job-! ends in a mark");

    // One class stands for one character, no more and no fewer.
    assert!(!matches("job-[[:space:]]", "job-"));
    assert!(!matches("job-[[:space:]]", "job-  "));
    assert!(
        matches("job-[[:space:]]*", "job-  "),
        "a star takes the rest"
    );
}

#[test]
fn the_six_members_the_oracle_answered_for_and_their_neighbours() {
    // Measured, one row each: VERTICAL TAB among them, which is the whole
    // reason this class is not `is_ascii_whitespace`.
    for member in [' ', '\t', '\n', '\u{b}', '\u{c}', '\r'] {
        assert!(
            matches("job-[[:space:]]", &format!("job-{member}")),
            "job-{member:?} is whitespace to the oracle"
        );
    }
    assert!(
        matches("job-[[:space:]]", "job-\u{b}"),
        "vertical tab is a member; Rust's is_ascii_whitespace leaves it out"
    );

    // Measured non-members, and the neighbours of the control block this
    // reading adds around them: the six are the whole list.
    for other in ['a', '7', '\u{1b}', '\u{7f}'] {
        assert!(!matches("job-[[:space:]]", &format!("job-{other}")));
    }
    for code in 0x00u8..=0x7f {
        let ch = char::from(code);
        let is_member = matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r');
        assert_eq!(
            matches("job-[[:space:]]", &format!("job-{ch}")),
            is_member,
            "job-{ch:?} against the space class"
        );
    }
}

#[test]
fn the_flag_changes_nothing_and_the_name_space_keeps_its_case() {
    // Measured: the folded flag answers exactly as the plain one does, which is
    // why this class reads the character the text carries.
    assert!(matches_folded("job-[[:space:]]", "job- "));
    assert!(!matches_folded("job-[[:space:]]", "job-a"));

    // The name written SPACE is not one this reading knows, folded or not.
    assert!(!matches("job-[[:SPACE:]]", "job- "));
    assert!(!matches_folded("job-[[:SPACE:]]", "job- "));
    assert!(
        !matches("job-[[:Space:]]", "job- "),
        "nor any other spelling"
    );

    // The negation, and the order in which a list is consumed.
    assert!(!matches("job-[![:space:]]", "job- "));
    assert!(matches("job-[![:space:]]", "job-a"));
    assert!(matches("job-[[:space:][:nosuch:]]", "job- "));
    assert!(!matches("job-[[:nosuch:][:space:]]", "job- "));
    assert!(
        matches("job-[x[:space:]]", "job-x"),
        "a literal member first"
    );

    // And the guards the other names keep.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job- "));
    assert!(
        !matches("job-[[:space:]-x]", "job- "),
        "nor may space open a range"
    );
}
