//! The named class `[:digit:]`, and the difference between syntax and letters.
//!
//! Every expectation below was measured on the pinned tmux 3.7b BEFORE a line
//! of this lot was written, with `LC_ALL=C` and ASCII throughout, on a server
//! of its own: `format.c:4484-4511` hands `m:` to `fnmatch(3)` and adds
//! `FNM_CASEFOLD` only for `m/i:`, so what answers below is that `fnmatch`.
//! The cells are `gdigit1/cells/*/MEASURED.json`, 80 cases in five
//! rounds, each with its own raw bytes and its own exit code.
//!
//! ```text
//! #{m:job-[[:digit:]],job-7}        1      #{m:job-[[:digit:]],job-x}        0
//! #{m:job-[![:digit:]],job-7}       0      #{m:job-[![:digit:]],job-x}       1
//! #{m:job-[[:digit:]],job-d]}       0      #{m:job-[[:digit:]],job-10}       0
//! #{m:job-[x[:digit:]],job-x}       1      #{m:job-[x[:digit:]],job-7}       1
//! #{m:job-[[:nosuch:]],job-7}       0      #{m:job-[[:nosuch:]],job-n}       0
//! #{m/i:job-[[:digit:]],job-7}      1      #{m/i:job-[[:DIGIT:]],job-7}      0
//! ```
//!
//! What this lot closes is `digit`. Every other POSIX name stays unknown to
//! this reading on purpose - the two places where that still differs from the
//! oracle are asserted here as the divergence they are, so the bound is in the
//! code and not only in a remise.

use super::super::glob::format_fnmatch;
use super::super::glob::holds_a_class_name;
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
fn the_digit_class_finds_the_numbered_tool_and_leaves_the_others() {
    // The journey itself: the tool titled `job-7` is the one the inventory
    // marks, and the one titled `job-x` is not.
    assert!(matches("job-[[:digit:]]", "job-7"), "job-7 carries a digit");
    assert!(!matches("job-[[:digit:]]", "job-x"), "job-x does not");

    // One class stands for ONE character: two digits are not one, and none is
    // not one either.
    assert!(
        !matches("job-[[:digit:]]", "job-10"),
        "two digits are not one"
    );
    assert!(
        !matches("job-[[:digit:]]", "job-"),
        "and nothing is not one"
    );
    assert!(
        matches("job-[[:digit:]]*", "job-10"),
        "a star takes the rest"
    );
    assert!(
        matches("job-[[:digit:]][[:digit:]]", "job-10"),
        "or two classes"
    );

    // Anywhere in the text, with the stars doing their own work.
    assert!(matches("*[[:digit:]]*", "ab7cd"));
    assert!(!matches("*[[:digit:]]*", "abcd"));

    // Every ASCII digit, and nothing else of the neighbourhood.
    for digit in '0'..='9' {
        assert!(
            matches("job-[[:digit:]]", &format!("job-{digit}")),
            "job-{digit} carries a digit"
        );
    }
    for other in ['/', ':', 'a', 'Z', '-', ']', '['] {
        assert!(
            !matches("job-[[:digit:]]", &format!("job-{other}")),
            "job-{other} does not"
        );
    }
}

#[test]
fn the_class_is_read_as_syntax_before_it_is_read_as_letters() {
    // The false positives a character-by-character reading produces: the name
    // of the class became its own member list, and the `]` that closed it
    // became a literal to match.
    assert!(!matches("job-[[:digit:]]", "job-d]"), "d] is not a digit");
    assert!(!matches("job-[[:digit:]]", "job-:]"), "nor is :]");
    assert!(!matches("job-[[:digit:]]", "job-[]"), "nor is []");
    assert!(!matches("job-[[:digit:]]", "job-d"), "nor is d alone");
    assert!(
        !matches("job-[[:digit:]]", "job-[:digit:]"),
        "nor is the text the reading was built from"
    );

    // The same false positives for the names this lot does NOT close: they are
    // gone as well, because the syntax is read before the letters are.
    assert!(!matches("job-[[:upper:]]", "job-u]"), "u] is not upper");
    assert!(!matches("job-[[:alpha:]]", "job-a]"), "a] is not alpha");

    // `alpha` was one of those divergences and is one no longer: GALPHA1 closed
    // it, and the oracle's 1 is now this reading's answer too. Its own witnesses
    // are in glob_alpha_class.rs.
    assert!(
        matches("job-[[:alpha:]]", "job-a"),
        "alpha is closed now, and the oracle answers 1 here"
    );
    // `upper` was the last of those divergences and is one no longer: GCASE1
    // closed it beside `lower`, and its own witnesses are in glob_case_class.rs.
    assert!(
        matches("job-[[:upper:]]", "job-A"),
        "upper is closed now, and the oracle answers 1 here"
    );
}

#[test]
fn the_negated_digit_class_answers_the_other_way() {
    assert!(
        !matches("job-[![:digit:]]", "job-7"),
        "7 is the excluded one"
    );
    assert!(matches("job-[![:digit:]]", "job-x"), "x is not");
    assert!(
        matches("job-[^[:digit:]]", "job-x"),
        "^ negates the same way"
    );
    assert!(!matches("job-[^[:digit:]]", "job-7"));

    // Negated beside a literal.
    assert!(matches("job-[!x[:digit:]]", "job-y"), "y is neither");
    assert!(!matches("job-[!x[:digit:]]", "job-7"), "7 is one of them");
}

#[test]
fn a_class_written_beside_other_members_is_read_as_both() {
    assert!(matches("job-[x[:digit:]]", "job-x"), "the literal member");
    assert!(matches("job-[x[:digit:]]", "job-7"), "and the named one");
    assert!(!matches("job-[x[:digit:]]", "job-y"), "and nothing else");

    // The same list written the other way round.
    assert!(matches("job-[[:digit:]x]", "job-x"));
    assert!(matches("job-[[:digit:]x]", "job-7"));
    assert!(matches("job-[[:digit:]]x", "job-7x"), "a literal behind it");

    // A `-` that closes the list is a member, not a range.
    assert!(matches("job-[[:digit:]-]", "job-7"));
    assert!(matches("job-[[:digit:]-]", "job--"));
    assert!(
        matches("job-[-[:digit:]]", "job--"),
        "written first as well"
    );
    assert!(matches("job-[-[:digit:]]", "job-7"));

    // Two names in one list, the second one known here or not.
    assert!(
        matches("job-[[:digit:][:nosuch:]]", "job-7"),
        "the first matched"
    );
    assert!(matches("job-[x[:nosuch:]]", "job-x"), "the literal matched");
}

#[test]
fn a_name_this_reading_does_not_know_matches_nothing() {
    // An unknown name reached before anything has matched refuses the whole
    // pattern - the oracle answers 0 for both of these, and for the negation,
    // which is what says it is a refusal and not an empty member list.
    assert!(!matches("job-[[:nosuch:]]", "job-7"));
    assert!(
        !matches("job-[[:nosuch:]]", "job-n"),
        "not its own letters either"
    );
    assert!(
        !matches("job-[![:nosuch:]]", "job-7"),
        "negated, still refused"
    );
    assert!(
        !matches("*[[:nosuch:]]*", "job-n"),
        "stars do not rescue it"
    );
    assert!(!matches("job-[[::]]", "job-7"), "a name with nothing in it");
    assert!(!matches("job-[![::]]", "job-7"));
    assert!(
        !matches("job-[[:nosuch:][:digit:]]", "job-7"),
        "reached first"
    );
    assert!(
        !matches("job-[[:digit:][:nosuch:]]", "job-x"),
        "reached at all"
    );
    assert!(!matches("job-[[:nosuch:]x]", "job-x"), "before the literal");
    assert!(!matches("job-[[:nosuch:]x]", "job-y"));
    assert!(!matches("job-[![:digit:][:nosuch:]]", "job-x"));
    assert!(!matches("job-[![:digit:][:nosuch:]]", "job-7"));
}

#[test]
fn a_class_that_is_not_one_is_not_read_as_a_class() {
    // The bracket closed before the name was.
    assert!(!matches("job-[[:digit:]", "job-7"));
    assert!(!matches("job-[[:digit]", "job-7"));
    assert!(!matches("job-[[:digit:", "job-7"));

    // `[:digit:]` without the brackets that make it a class is a member list
    // holding its own letters - that is what the oracle answers, and this is
    // the one place where the letters really are the members.
    assert!(!matches("job-[:digit:]", "job-7"));
    assert!(matches("job-[:digit:]", "job-d"));

    // A class cannot open a range.
    assert!(!matches("job-[[:digit:]-x]", "job-7"));
    assert!(!matches("job-[[:digit:]-x]", "job-x"));
    assert!(!matches("job-[[:digit:]-x]", "job--"));
    assert!(!matches("job-[x-[:digit:]]", "job-7"), "nor close one");
    assert!(!matches("job-[a-[:digit:]]", "job-b"));
}

#[test]
fn folding_reaches_the_members_and_never_the_name() {
    // `m/i:` folds what is compared. The name of a class is not compared with
    // anything, so it is not folded: the oracle answers 0 for every spelling
    // of DIGIT but the one POSIX gives it.
    assert!(
        matches_folded("job-[[:digit:]]", "job-7"),
        "the name as it is"
    );
    assert!(
        !matches_folded("job-[[:DIGIT:]]", "job-7"),
        "in capitals, unknown"
    );
    assert!(
        !matches_folded("job-[[:DIGIT:]]", "job-d]"),
        "and no false positive"
    );
    assert!(
        !matches_folded("job-[[:DIGIT:]]*", "job-7x"),
        "nor with a star"
    );
    assert!(
        !matches("job-[[:DIGIT:]]", "job-7"),
        "unfolded, unknown as well"
    );
    assert!(
        !matches_folded("job-[[:digit:]]", "job-d]"),
        "folded, still no d]"
    );

    // What folding does reach: the literals around the class, and the members
    // of an ordinary list, in both directions.
    assert!(matches_folded("JOB-[[:digit:]]", "job-7"));
    assert!(matches_folded("[A]", "a"));
    assert!(matches_folded("[a]", "A"));
    assert!(!matches("[A]", "a"), "and only through the i flag");

    // The bare bracket whose members are letters: those DO fold.
    assert!(matches_folded("job-[:digit:]", "job-d"));
    assert!(matches_folded("job-[:DIGIT:]", "job-d"));
    assert!(matches_folded("job-[:DIGIT:]", "job-D"));
}

#[test]
fn what_this_class_reading_leaves_exactly_as_it_was() {
    // Ranges, stars, marks and the bracket members GCLASS closed: the class
    // reading is new syntax inside the list and takes nothing away.
    assert!(matches("job-[0-9]", "job-7"));
    assert!(matches("job-[!0-9]", "job-x"));
    assert!(matches("[a-c]", "b"));
    assert!(!matches("[a-c]", "d"));
    assert!(matches("job-*", "job-7"));
    assert!(matches("job-?", "job-7"));
    assert!(
        matches("job-[]]", "job-]"),
        "the ] written first is a member"
    );
    assert!(matches("job-[!]]", "job-x"));
    assert!(
        matches("[[]", "["),
        "and [ opens a class before it is a letter"
    );
    assert!(
        matches("job-[[x]", "job-["),
        "a [ that is a member, not a name"
    );
    assert!(matches("job-[[x]", "job-x"));
    assert!(
        !matches("[abc", "a"),
        "an unterminated class matches nothing"
    );
    assert!(!matches("job-[", "job-["));
}

#[test]
fn a_folded_pattern_with_no_class_name_keeps_the_whole_lowercase() {
    // The oracle for these two witnesses is not tmux: it is the source this
    // lot started from, 8ffa1c73, where `m/i:` lowercased the whole pattern
    // and the whole text with `str::to_lowercase` before matching. A character
    // whose lowercase mapping is longer than itself is where that shows, and
    // `\u{130}` is the one the review named.
    assert_eq!(
        "\u{130}".to_lowercase().chars().count(),
        2,
        "U+0130 lowercases to `i` and a combining dot"
    );

    // One character of pattern does not cover those two.
    assert!(
        !matches_folded("i", "\u{130}"),
        "i does not cover i + the dot"
    );
    assert!(!matches_folded("I", "\u{130}"), "nor does the capital");

    // And nothing of the ASCII folding around it moves.
    assert!(matches_folded("i", "I"));
    assert!(matches_folded("job-7", "JOB-7"));
}

#[test]
fn a_mark_still_covers_what_the_whole_lowercase_produced() {
    // The other half of the same reading: once the text is two characters, a
    // mark and a star reach the second one.
    assert!(
        matches_folded("i?", "\u{130}"),
        "the mark takes the combining dot"
    );
    assert!(matches_folded("i*", "\u{130}"), "and so does the star");

    // Unfolded, the pattern meets one character that is not `i`, and misses.
    assert!(
        !matches("i?", "\u{130}"),
        "without the flag there is nothing to fold"
    );
    assert!(!matches("i", "\u{130}"));
}

#[test]
fn an_ordinary_list_with_two_colons_is_not_a_class_name() {
    // `[:I:]` is a list holding `:`, `I` and `:` - the shape R1 measured
    // against the oracle as `job-[:digit:]` - and no class name at all. The
    // oracle for its folded reading is the source this lot started from,
    // 8ffa1c73, where the whole pattern and the whole text were lowercased
    // before anything was compared, so one member does not cover `i` and the
    // combining dot that `\u{130}` becomes.
    assert!(
        !matches_folded("[:I:]", "\u{130}"),
        "one member does not cover two characters"
    );
    assert!(
        matches_folded("[:I:]", "i"),
        "and it still holds its own members"
    );
    assert!(
        matches("[:I:]", "I"),
        "unfolded, the capital is the member written"
    );

    // The discriminator that chooses between the two readings, asked directly:
    // it reads a list the way the matcher reads one, and nothing else decides.
    assert!(!holds_a_class_name("[:I:]"), "three ordinary members");
    assert!(
        holds_a_class_name("[!][:digit:]]"),
        "a negation, a ] written first as a member, then a name"
    );
    assert!(
        !holds_a_class_name("[a-[:DIGIT:]]"),
        "a [ the range already took as its end"
    );
    assert!(
        holds_a_class_name("[[:DIGIT:]]"),
        "a name is written even when this reading does not know it"
    );

    // And the shapes around those four, read the same way.
    assert!(!holds_a_class_name("job-7"), "no list at all");
    assert!(
        !holds_a_class_name("job-["),
        "a list that opens and never closes"
    );
    assert!(
        !holds_a_class_name("[[]"),
        "a [ that is a member, not a name"
    );
    assert!(
        !holds_a_class_name("[:digit:]"),
        "the bare list of its letters"
    );
    assert!(
        holds_a_class_name("job-[[:digit:]]"),
        "the journey's own pattern"
    );
    assert!(
        holds_a_class_name("[x[:digit:]]"),
        "a name behind a literal member"
    );
    assert!(
        holds_a_class_name("a[:b:]c[[:digit:]]"),
        "an ordinary list first, then a name in the next one"
    );
}

#[test]
fn a_mark_behind_an_ordinary_list_covers_the_second_character() {
    assert!(
        matches_folded("[:I:]?", "\u{130}"),
        "the mark takes the combining dot"
    );
    assert!(
        !matches("[:I:]?", "\u{130}"),
        "without the flag there is one character to meet, and it is not a member"
    );
}
