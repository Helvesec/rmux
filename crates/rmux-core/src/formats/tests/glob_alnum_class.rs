//! `[:alnum:]`, and a suffix of exactly two alphanumeric characters.
//!
//! The journey is the inventory that finds the tool whose suffix is
//! alphanumeric: two readers, one titled `job-a7` and one titled `job-a]`, and
//! `#{pane_id}|#{m:job-[[:alnum:]][[:alnum:]],#{pane_title}}` marks the first
//! and not the second.
//!
//! Nothing here is adopted from an earlier lot: no case of `alnum` was measured
//! in the GDIGIT1, GALPHA1 or GCASE1 evidence, so this lot measured its own
//! destination too, on its own oracle image, before this file existed -
//! `galnum1/cells/o1`, twenty-seven cases, LC_ALL=C:
//!
//! ```text
//! #{m:job-[[:alnum:]][[:alnum:]],job-a7}    1    …,job-a]}     0
//! …,job-Aa}   1   …,job-09}   1   …,job-A0}   1   …,job-a_}   0   …,job-]a}   0
//! #{m:job-[[:alnum:]],job-a}  1   ,job-A}  1   ,job-0}  1   ,job-9}  1
//! #{m:job-[[:alnum:]],job-_}  0   ,job-]}  0
//! two classes against one character 0, and against three 0
//! #{m/i:…,job-a7}  1   #{m/i:job-[[:alnum:]],job-A}  1   ,job-a}  1
//! #{m:job-[[:ALNUM:]],job-a}  0   folded 0   twice over 0
//! #{m:job-[![:alnum:]],job-_}  1   ,job-a}  0
//! #{m:job-[[:alnum:][:nosuch:]],job-a}  1   #{m:job-[[:nosuch:][:alnum:]],job-a}  0
//! ```
//!
//! What was measured is what those rows say. What the witnesses below extend
//! beyond it - every ASCII letter and digit one by one, and the punctuation
//! around the three ranges - is this reading's own ASCII whitelist, asserted as
//! coverage this lot chose rather than as coverage the oracle answered for.
//! The folded flag changes none of the measured answers, which is why `alnum`
//! is asked about the character the text carries, as `alpha` and `digit` are.

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
fn a_suffix_of_two_alphanumeric_characters_finds_its_tool() {
    // The journey itself, both answers measured on this lot's oracle.
    assert!(
        matches("job-[[:alnum:]][[:alnum:]]", "job-a7"),
        "job-a7 is a letter and a digit"
    );
    assert!(
        !matches("job-[[:alnum:]][[:alnum:]]", "job-a]"),
        "job-a] is a letter and a bracket"
    );

    // The other pairs the oracle answered for.
    assert!(matches("job-[[:alnum:]][[:alnum:]]", "job-Aa"));
    assert!(matches("job-[[:alnum:]][[:alnum:]]", "job-09"));
    assert!(matches("job-[[:alnum:]][[:alnum:]]", "job-A0"));
    assert!(!matches("job-[[:alnum:]][[:alnum:]]", "job-a_"));
    assert!(!matches("job-[[:alnum:]][[:alnum:]]", "job-]a"));

    // Two classes stand for two characters, no more and no fewer.
    assert!(!matches("job-[[:alnum:]][[:alnum:]]", "job-a"));
    assert!(!matches("job-[[:alnum:]][[:alnum:]]", "job-a7b"));
    assert!(
        matches("job-[[:alnum:]][[:alnum:]]*", "job-a7b"),
        "a star takes the rest"
    );
}

#[test]
fn one_class_holds_the_letters_and_the_digits_and_nothing_else() {
    // Measured: a small letter, a capital, the first digit and the last.
    assert!(matches("job-[[:alnum:]]", "job-a"));
    assert!(matches("job-[[:alnum:]]", "job-A"));
    assert!(matches("job-[[:alnum:]]", "job-0"));
    assert!(matches("job-[[:alnum:]]", "job-9"));
    assert!(!matches("job-[[:alnum:]]", "job-_"));
    assert!(!matches("job-[[:alnum:]]", "job-]"));

    // Extended by this reading, not by the oracle: the whole ASCII whitelist,
    // one character at a time, and the punctuation that borders the three
    // ranges on both sides.
    for member in ('a'..='z').chain('A'..='Z').chain('0'..='9') {
        assert!(
            matches("job-[[:alnum:]]", &format!("job-{member}")),
            "job-{member} is alphanumeric"
        );
    }
    for other in ['/', ':', '@', '[', '`', '{', '-', ']', '_', ' ', '.', '~'] {
        assert!(
            !matches("job-[[:alnum:]]", &format!("job-{other}")),
            "job-{other} is not"
        );
    }

    // Beside the names the earlier lots closed, in one list.
    assert!(matches("job-[[:alpha:][:digit:]]", "job-7"));
    assert!(matches("job-[[:alnum:]]", "job-7"));
}

#[test]
fn the_flag_changes_nothing_and_the_name_alnum_keeps_its_case() {
    // Measured: the folded flag answers exactly as the plain one does, which is
    // why this class reads the character the text carries.
    assert!(matches_folded("job-[[:alnum:]][[:alnum:]]", "job-a7"));
    assert!(matches_folded("job-[[:alnum:]]", "job-A"));
    assert!(matches_folded("job-[[:alnum:]]", "job-a"));
    assert!(!matches_folded("job-[[:alnum:]]", "job-_"));

    // The name written ALNUM is not one this reading knows, folded or not.
    assert!(!matches("job-[[:ALNUM:]]", "job-a"));
    assert!(!matches_folded("job-[[:ALNUM:]]", "job-a"));
    assert!(!matches("job-[[:ALNUM:]][[:ALNUM:]]", "job-a7"));
    assert!(
        !matches("job-[[:Alnum:]]", "job-a"),
        "nor any other spelling"
    );

    // The negation, and the order in which a list is consumed: a known name
    // that matches is reached before an unknown one, and an unknown one reached
    // first refuses the pattern.
    assert!(matches("job-[![:alnum:]]", "job-_"));
    assert!(!matches("job-[![:alnum:]]", "job-a"));
    assert!(matches("job-[[:alnum:][:nosuch:]]", "job-a"));
    assert!(!matches("job-[[:nosuch:][:alnum:]]", "job-a"));
    assert!(
        matches("job-[x[:alnum:]]", "job-x"),
        "a literal member first"
    );

    // And the guards the other names keep.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job-a"));
    assert!(
        !matches("job-[[:alnum:]-x]", "job-a"),
        "nor may alnum open a range"
    );
}
