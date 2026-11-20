//! `[:punct:]`, and the tool a punctuation mark stands for.
//!
//! The journey is the inventory that finds the marked tool: two readers, one
//! titled `job-!` and one titled `job-a`, and
//! `#{pane_id}|#{m:job-[[:punct:]],#{pane_title}}` marks the first and not the
//! second.
//!
//! Nothing here is adopted: no `punct` case was found in the targeted evidence
//! of GDIGIT1, GALPHA1, GCASE1 or GALNUM1, so this lot measured its own
//! destination as well, on its own oracle image, before this file existed -
//! `gpunct1/cells/o1`, twenty-one cases under LC_ALL=C:
//!
//! ```text
//! #{m:job-[[:punct:]],job-!}   1    ,job-_}  1    ,job-[}  1    ,job-]}  1
//! #{m:job-[[:punct:]],job-a}   0    ,job-A}  0    ,job-0}  0    ,job-9}  0
//! #{m:job-[[:punct:]],job- }   0    ,job-}   0    ,job-!?} 0
//! #{m/i:job-[[:punct:]],job-!} 1    #{m/i:job-[[:punct:]],job-a}  0
//! #{m:job-[[:PUNCT:]],job-!}   0    folded 0
//! #{m:job-[![:punct:]],job-!}  0    ,job-a}  1
//! #{m:job-[[:punct:][:nosuch:]],job-!}  1   #{m:job-[[:nosuch:][:punct:]],job-!}  0
//! ```
//!
//! What the witnesses extend beyond those rows - the thirty-two ASCII marks one
//! by one, and the letters, digits, space and controls around them - is this
//! reading's own whitelist, asserted as coverage this lot chose rather than as
//! coverage the oracle answered for. The folded flag changes none of the
//! measured answers, which is why `punct` reads the character the text carries,
//! as `digit`, `alpha` and `alnum` do.

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
fn a_punctuated_suffix_finds_its_tool_and_a_lettered_one_does_not() {
    // The journey itself, both answers measured on this lot's oracle.
    assert!(matches("job-[[:punct:]]", "job-!"), "job-! carries a mark");
    assert!(
        !matches("job-[[:punct:]]", "job-a"),
        "job-a carries a letter"
    );

    // The other members and non-members the oracle answered for.
    assert!(matches("job-[[:punct:]]", "job-_"));
    assert!(matches("job-[[:punct:]]", "job-["));
    assert!(matches("job-[[:punct:]]", "job-]"));
    assert!(!matches("job-[[:punct:]]", "job-A"));
    assert!(!matches("job-[[:punct:]]", "job-0"));
    assert!(!matches("job-[[:punct:]]", "job-9"));
    assert!(!matches("job-[[:punct:]]", "job- "));

    // One class stands for one character, no more and no fewer.
    assert!(!matches("job-[[:punct:]]", "job-"));
    assert!(!matches("job-[[:punct:]]", "job-!?"));
    assert!(
        matches("job-[[:punct:]]*", "job-!?"),
        "a star takes the rest"
    );
}

#[test]
fn the_whole_ascii_whitelist_and_nothing_beside_it() {
    // Extended by this reading, not by the oracle: the thirty-two ASCII marks,
    // one at a time - the four ranges POSIX gives punct.
    let mut marks = 0;
    for code in 0x21u8..=0x7e {
        let member = char::from(code);
        let is_mark = !member.is_ascii_alphanumeric();
        if is_mark {
            marks += 1;
        }
        assert_eq!(
            matches("job-[[:punct:]]", &format!("job-{member}")),
            is_mark,
            "job-{member} against the punctuation class"
        );
    }
    assert_eq!(marks, 32, "ASCII holds thirty-two punctuation marks");

    // And what borders them: the letters, the digits, a space, a tab and a
    // control character are none of them.
    for other in [
        'a', 'z', 'A', 'Z', '0', '9', ' ', '\t', '\n', '\u{0}', '\u{7f}',
    ] {
        assert!(
            !matches("job-[[:punct:]]", &format!("job-{other}")),
            "job-{other:?} is not a punctuation mark"
        );
    }
}

#[test]
fn the_flag_changes_nothing_and_the_name_punct_keeps_its_case() {
    // Measured: the folded flag answers exactly as the plain one does, which is
    // why this class reads the character the text carries.
    assert!(matches_folded("job-[[:punct:]]", "job-!"));
    assert!(!matches_folded("job-[[:punct:]]", "job-a"));
    assert!(matches_folded("job-[[:punct:]]", "job-_"));

    // The name written PUNCT is not one this reading knows, folded or not.
    assert!(!matches("job-[[:PUNCT:]]", "job-!"));
    assert!(!matches_folded("job-[[:PUNCT:]]", "job-!"));
    assert!(
        !matches("job-[[:Punct:]]", "job-!"),
        "nor any other spelling"
    );

    // The negation, and the order in which a list is consumed: a known name
    // that matches is reached before an unknown one, and an unknown one reached
    // first refuses the pattern.
    assert!(!matches("job-[![:punct:]]", "job-!"));
    assert!(matches("job-[![:punct:]]", "job-a"));
    assert!(matches("job-[[:punct:][:nosuch:]]", "job-!"));
    assert!(!matches("job-[[:nosuch:][:punct:]]", "job-!"));
    assert!(
        matches("job-[x[:punct:]]", "job-x"),
        "a literal member first"
    );

    // And the guards the other names keep.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job-!"));
    assert!(
        !matches("job-[[:punct:]-x]", "job-!"),
        "nor may punct open a range"
    );
}
