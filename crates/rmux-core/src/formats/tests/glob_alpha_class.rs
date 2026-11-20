//! The named class `[:alpha:]`, and the character it is asked about.
//!
//! The journey is the inventory that finds the lettered tool: two readers, one
//! titled `job-a` and one titled `job-a]`, and
//! `#{pane_id}|#{m:job-[[:alpha:]],#{pane_title}}` marks the first and not the
//! second. Those two answers are the pinned tmux 3.7b as GDIGIT1 measured it -
//! `cells/o5/MEASURED.json`, sha256 ba20b30885c9a0d6cc4d1f668f22fc1542bb4f84
//! 55cd3937944592285b806c04, `310a` and `300a` - adopted here without being
//! replayed.
//!
//! The neighbours this lot needed were measured on its own oracle image before
//! this file existed, LC_ALL=C, in `galpha1/cells/o1`:
//!
//! ```text
//! #{m:job-[[:alpha:]],job-A}      1     #{m/i:job-[[:alpha:]],job-A}     1
//! #{m/i:job-[[:alpha:]],job-a}    1     #{m:job-[[:ALPHA:]],job-a}       0
//! #{m/i:job-[[:ALPHA:]],job-a}    0     #{m/i:job-[[:ALPHA:]],job-A}     0
//! ```
//!
//! and two texts that are not ASCII and whose lowercase is:
//!
//! ```text
//! #{m:job-[[:alpha:]],job-İ}      1     #{m/i:job-[[:alpha:]],job-İ}     1
//! #{m:job-[[:alpha:]],job-K}      1     #{m/i:job-[[:alpha:]],job-K}     1
//! #{m:job-??,job-İ}               0     #{m:job-???,job-K}               0
//! ```
//!
//! The last two say those texts are ONE character to that engine, not two or
//! three bytes: tmux takes a UTF-8 LC_CTYPE of its own, so its `iswalpha`
//! answers for the whole Unicode letter. This lot closes ASCII `alpha`, so it
//! answers 0 there, and that divergence is asserted below as the divergence it
//! is rather than measured away. What the measurement DOES settle is that the
//! engine answers the same with the folded flag and without it, so the reading
//! here must not depend on that flag either - and that is why `alpha` is asked
//! about the character the text really carries, not the folded one.

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
fn the_alpha_class_finds_the_lettered_tool_and_leaves_the_bracketed_one() {
    // The journey itself, and the two answers adopted from the oracle.
    assert!(
        matches("job-[[:alpha:]]", "job-a"),
        "job-a carries a letter"
    );
    assert!(
        !matches("job-[[:alpha:]]", "job-a]"),
        "job-a] carries a letter and a bracket, and one class is one character"
    );

    // Every ASCII letter, in both cases.
    for letter in ('a'..='z').chain('A'..='Z') {
        assert!(
            matches("job-[[:alpha:]]", &format!("job-{letter}")),
            "job-{letter} carries a letter"
        );
    }
    // And the neighbours of the two ranges, which are not letters.
    for other in ['@', '[', '`', '{', '/', ':', '0', '9', '-', ']', ' '] {
        assert!(
            !matches("job-[[:alpha:]]", &format!("job-{other}")),
            "job-{other} does not"
        );
    }

    // One class is one character here as everywhere else.
    assert!(
        !matches("job-[[:alpha:]]", "job-ab"),
        "two letters are not one"
    );
    assert!(!matches("job-[[:alpha:]]", "job-"), "and none is not one");
    assert!(
        matches("job-[[:alpha:]]*", "job-ab"),
        "a star takes the rest"
    );

    // The negation, and the class beside a literal member.
    assert!(!matches("job-[![:alpha:]]", "job-a"));
    assert!(matches("job-[![:alpha:]]", "job-7"));
    assert!(matches("job-[7[:alpha:]]", "job-7"), "the literal member");
    assert!(matches("job-[7[:alpha:]]", "job-a"), "and the named one");
    assert!(!matches("job-[7[:alpha:]]", "job-."), "and nothing else");

    // Beside the name this lot's neighbour closed.
    assert!(matches("job-[[:digit:][:alpha:]]", "job-7"));
    assert!(matches("job-[[:digit:][:alpha:]]", "job-a"));
    assert!(!matches("job-[[:digit:][:alpha:]]", "job-."));
}

#[test]
fn the_name_alpha_keeps_its_case_and_the_others_stay_unknown() {
    // Measured on this lot's oracle: ALPHA is not a name that engine knows,
    // with the folded flag or without it, exactly as DIGIT is not.
    assert!(!matches("job-[[:ALPHA:]]", "job-a"));
    assert!(!matches_folded("job-[[:ALPHA:]]", "job-a"));
    assert!(!matches_folded("job-[[:ALPHA:]]", "job-A"));
    assert!(
        !matches("job-[[:Alpha:]]", "job-a"),
        "nor any other spelling"
    );

    // The unknown names GDIGIT1 closed are still unknown, and still refuse the
    // pattern rather than inventing members out of their letters.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job-a"));
    assert!(!matches("job-[[:DIGIT:]]", "job-7"));
    assert!(
        matches("job-[[:upper:]]", "job-A"),
        "upper was not that lot; GCASE1 closed it"
    );
    assert!(
        !matches("job-[[:alpha:]-x]", "job-a"),
        "nor may alpha open a range"
    );
}

#[test]
fn alpha_is_asked_about_the_character_the_text_really_carries() {
    // The flag does not change what alpha holds - the oracle answers 1 for a
    // capital folded and unfolded, and so does this.
    assert!(matches("job-[[:alpha:]]", "job-A"));
    assert!(matches_folded("job-[[:alpha:]]", "job-A"));
    assert!(matches_folded("job-[[:alpha:]]", "job-a"));

    // `İ` and `K` are not ASCII letters, and their lowercase mappings - `i` and
    // `k` - are. Asking alpha about the folded character would make them
    // letters under `m/i:` and not under `m:`, which is a reading the oracle
    // does not have: it answers 1 for both. This lot closes ASCII alpha, so it
    // answers 0 for both, and that is a divergence, not an agreement.
    assert!(
        !matches("job-[[:alpha:]]", "job-\u{130}"),
        "U+0130 is not an ASCII letter; the oracle answers 1 here"
    );
    assert!(
        !matches_folded("job-[[:alpha:]]", "job-\u{130}"),
        "and the folded flag must not make it one"
    );
    assert!(
        !matches("job-[[:alpha:]]", "job-\u{212a}"),
        "U+212A is not an ASCII letter either; the oracle answers 1 here"
    );
    assert!(
        !matches_folded("job-[[:alpha:]]", "job-\u{212a}"),
        "and the folded flag must not make it one"
    );

    // What the folding does keep reaching: the literals and the ordinary
    // members around the class.
    assert!(matches_folded("JOB-[[:alpha:]]", "job-a"));
    assert!(matches_folded("job-[A]", "job-a"));
    assert!(matches_folded("job-[a-c]", "job-B"));
}
