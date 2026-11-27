//! `[:upper:]` and `[:lower:]`, and what `m/i:` does to a class about case.
//!
//! The journey is one inventory asked twice: two tools whose titles differ only
//! by the case of their suffix - `job-A` and `job-a` - are told apart by
//! `#{m:job-[[:upper:]],#{pane_title}}` and `#{m:job-[[:lower:]],#{pane_title}}`,
//! and each is then addressed by the identity its own class marked.
//!
//! Two of the answers are the pinned tmux 3.7b as GDIGIT1 measured it -
//! `cells/o4/MEASURED.json`, sha256 539eaa5997bbd89dbe23effb95a3ce207679fa45e0
//! b366e4e741cde12924fd63, `310a` and `300a` - adopted without being replayed:
//! `#{m:job-[[:upper:]],job-A}` is 1 and `#{m:job-[[:upper:]],job-7}` is 0.
//!
//! The rest was measured on this lot's own oracle image before this file
//! existed, in `gcase1/cells/o1`, twenty-seven cases:
//!
//! ```text
//! #{m:job-[[:upper:]],job-a}       0    #{m:job-[[:lower:]],job-a}       1
//! #{m:job-[[:lower:]],job-A}       0    #{m:job-[[:lower:]],job-7}       0
//! #{m/i:job-[[:upper:]],job-A}     0    #{m/i:job-[[:upper:]],job-a}     0
//! #{m/i:job-[[:lower:]],job-a}     1    #{m/i:job-[[:lower:]],job-A}     1
//! #{m/i:JOB-[[:upper:]],job-A}     0    #{m/i:JOB-[[:lower:]],job-a}     1
//! ```
//!
//! That is the measurement this lot was told not to presume from `alpha`, and
//! it does not resemble it: with the folded flag the character is lowered
//! before the class is asked, so `upper` holds NOTHING and `lower` holds both
//! cases. `alpha` keeps being asked about the character the text carries, which
//! is what GALPHA1 measured and wrote down; a class about case is asked about
//! the folded one, which is what this lot measured. The names `UPPER` and
//! `LOWER` are unknown to that engine in every spelling asked, folded or not.

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
fn the_two_classes_tell_the_two_suffixes_apart() {
    // The journey itself: one title answers one class and the other answers the
    // other, and neither answers both.
    assert!(
        matches("job-[[:upper:]]", "job-A"),
        "job-A is the capital one"
    );
    assert!(!matches("job-[[:upper:]]", "job-a"), "job-a is not");
    assert!(
        matches("job-[[:lower:]]", "job-a"),
        "job-a is the small one"
    );
    assert!(!matches("job-[[:lower:]]", "job-A"), "job-A is not");

    // Neither holds what is not a letter at all.
    assert!(!matches("job-[[:upper:]]", "job-7"));
    assert!(!matches("job-[[:lower:]]", "job-7"));

    // Every ASCII letter, on the side it belongs to and not on the other.
    for letter in 'A'..='Z' {
        assert!(matches("job-[[:upper:]]", &format!("job-{letter}")));
        assert!(!matches("job-[[:lower:]]", &format!("job-{letter}")));
    }
    for letter in 'a'..='z' {
        assert!(matches("job-[[:lower:]]", &format!("job-{letter}")));
        assert!(!matches("job-[[:upper:]]", &format!("job-{letter}")));
    }
    // And the neighbours of the two ranges belong to neither.
    for other in ['@', '[', '`', '{', '/', ':', '0', '9', '-', ']', ' '] {
        assert!(!matches("job-[[:upper:]]", &format!("job-{other}")));
        assert!(!matches("job-[[:lower:]]", &format!("job-{other}")));
    }
}

#[test]
fn the_folded_flag_lowers_the_character_before_a_class_about_case_sees_it() {
    // Measured, not presumed: with `m/i:` the capital is lowered first, so
    // `upper` holds nothing at all and `lower` holds both cases.
    assert!(
        !matches_folded("job-[[:upper:]]", "job-A"),
        "folded, upper holds nothing - the oracle answers 0"
    );
    assert!(!matches_folded("job-[[:upper:]]", "job-a"));
    assert!(matches_folded("job-[[:lower:]]", "job-a"));
    assert!(
        matches_folded("job-[[:lower:]]", "job-A"),
        "folded, lower holds the capital too - the oracle answers 1"
    );

    // The literals around the class fold as they always did, and that does not
    // rescue `upper`.
    assert!(!matches_folded("JOB-[[:upper:]]", "job-A"));
    assert!(matches_folded("JOB-[[:lower:]]", "job-a"));

    // `alpha` is not asked the same question, and GALPHA1's answer stands: it
    // reads the character the text carries, so the flag changes nothing for it.
    assert!(matches("job-[[:alpha:]]", "job-A"));
    assert!(matches_folded("job-[[:alpha:]]", "job-A"));
    assert!(matches_folded("job-[[:alpha:]]", "job-a"));
}

#[test]
fn the_names_upper_and_lower_keep_their_case_and_their_neighbours_hold() {
    // Unknown in every spelling asked, with the flag and without it.
    assert!(!matches("job-[[:UPPER:]]", "job-A"));
    assert!(!matches_folded("job-[[:UPPER:]]", "job-A"));
    assert!(!matches("job-[[:LOWER:]]", "job-a"));
    assert!(!matches_folded("job-[[:LOWER:]]", "job-a"));
    assert!(
        !matches("job-[[:Upper:]]", "job-A"),
        "nor any other spelling"
    );

    // The negations, on both sides.
    assert!(matches("job-[![:upper:]]", "job-a"));
    assert!(!matches("job-[![:upper:]]", "job-A"));
    assert!(matches("job-[![:lower:]]", "job-A"));
    assert!(!matches("job-[![:lower:]]", "job-a"));

    // One list holding both names takes either letter and nothing else.
    assert!(matches("job-[[:upper:][:lower:]]", "job-A"));
    assert!(matches("job-[[:upper:][:lower:]]", "job-a"));
    assert!(!matches("job-[[:upper:][:lower:]]", "job-7"));

    // A letter of a name with the bracket behind it is not a member, and one
    // class is one character.
    assert!(!matches("job-[[:upper:]]", "job-u]"));
    assert!(!matches("job-[[:lower:]]", "job-l]"));
    assert!(!matches("job-[[:upper:]]", "job-AB"));
    assert!(!matches("job-[[:lower:]]", "job-ab"));
    assert!(!matches("job-[[:upper:]]", "job-"));

    // The guards the other names keep: unknown stays unknown, and a class may
    // not open a range.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job-a"));
    assert!(!matches("job-[[:upper:]-x]", "job-A"));
}
