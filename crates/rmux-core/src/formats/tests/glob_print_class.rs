//! `[:print:]` and `[:graph:]`, and the two roles one inventory tells apart.
//!
//! The journey is a single inventory asked of two readers: A titled `job- ` -
//! `6a6f622d20`, its last byte a space - and B titled `job-!` -
//! `6a6f622d21`. `#{pane_id}|#{pane_title}|#{m:job-[[:print:]],#{pane_title}}|
//! #{m:job-[[:graph:]],#{pane_title}}` answers `1|0` for the first and `1|1`
//! for the second, so B is the line whose graph answered 1 and A the line
//! whose print answered 1 while its graph answered 0.
//!
//! Nothing here is adopted: neither name appears in the acquis, so this lot
//! measured both of them, side by side, on its own oracle image before this
//! file existed - `gprint1/cells/o1`, thirty-nine cases:
//!
//! ```text
//!            space  !  ~  a  7  HT  LF  VT  FF  CR  US  DEL  none  two
//! print        1    1  1  1  1   0   0   0   0   0   0   0     0    0
//! graph        0    1  1  1  1   0   -   -   -   -   0   0     0    0
//! folded print 1    1                     names PRINT/GRAPH: 0, folded 0
//! folded graph 0    1                     negations: print space 0, HT 1
//!                                                    graph space 1, mark 0
//! #{m:job-[[:print:][:nosuch:]],job- } 1   the unknown one first 0
//! ```
//!
//! `isprint(3)` and `isgraph(3)` on this machine say the same thing in words -
//! "including space" and "except space" - and those are manual pages, not
//! measurements; the engine was asked anyway. What the witnesses add beyond
//! those thirty-nine rows is a sweep of the ASCII block, one code point at a
//! time against each name, and it is this reading's own whitelist. Nothing is
//! claimed for another locale or for Unicode, and `LC_ALL` in an environment
//! does not prove the LC_CTYPE an engine ended up with.

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
fn one_inventory_tells_the_printable_space_from_the_visible_mark() {
    // The two roles, exactly as the inventory reads them.
    assert!(matches("job-[[:print:]]", "job- "), "a space is printable");
    assert!(
        !matches("job-[[:graph:]]", "job- "),
        "and it is not visible"
    );
    assert!(matches("job-[[:print:]]", "job-!"), "a mark is printable");
    assert!(matches("job-[[:graph:]]", "job-!"), "and it is visible too");

    // The titles this journey pins, byte for byte.
    assert_eq!("job- ".as_bytes(), &[0x6a, 0x6f, 0x62, 0x2d, 0x20]);
    assert_eq!("job-!".as_bytes(), &[0x6a, 0x6f, 0x62, 0x2d, 0x21]);

    // One class stands for one character, for either name.
    assert!(!matches("job-[[:print:]]", "job-"));
    assert!(!matches("job-[[:print:]]", "job-!!"));
    assert!(!matches("job-[[:graph:]]", "job-"));
    assert!(!matches("job-[[:graph:]]", "job-!!"));
    assert!(
        matches("job-[[:print:]]*", "job-!!"),
        "a star takes the rest"
    );
}

#[test]
fn print_holds_the_space_and_everything_visible_up_to_the_tilde() {
    // Measured members and non-members.
    for member in [' ', '!', '~', 'a', '7'] {
        assert!(
            matches("job-[[:print:]]", &format!("job-{member}")),
            "job-{member:?} is printable"
        );
    }
    for outside in ['\t', '\n', '\u{b}', '\u{c}', '\r', '\u{1f}', '\u{7f}'] {
        assert!(
            !matches("job-[[:print:]]", &format!("job-{outside}")),
            "job-{outside:?} is not printable"
        );
    }

    // Extended by this reading: the whole ASCII block, one code point at a time.
    let mut members = 0;
    for code in 0x00u8..=0x7f {
        let ch = char::from(code);
        let is_member = (0x20..=0x7e).contains(&code);
        if is_member {
            members += 1;
        }
        assert_eq!(
            matches("job-[[:print:]]", &format!("job-{ch}")),
            is_member,
            "job-{ch:?} against the printable class"
        );
    }
    assert_eq!(
        members, 95,
        "the space and the ninety-four visible characters"
    );
}

#[test]
fn graph_holds_everything_print_holds_except_the_space() {
    for member in ['!', '~', 'a', '7'] {
        assert!(
            matches("job-[[:graph:]]", &format!("job-{member}")),
            "job-{member:?} is visible"
        );
    }
    for outside in [' ', '\t', '\u{1f}', '\u{7f}'] {
        assert!(
            !matches("job-[[:graph:]]", &format!("job-{outside}")),
            "job-{outside:?} is not visible"
        );
    }

    // Extended by this reading: the same sweep, one code point past the space.
    let mut members = 0;
    for code in 0x00u8..=0x7f {
        let ch = char::from(code);
        let is_member = (0x21..=0x7e).contains(&code);
        if is_member {
            members += 1;
        }
        assert_eq!(
            matches("job-[[:graph:]]", &format!("job-{ch}")),
            is_member,
            "job-{ch:?} against the visible class"
        );
    }
    assert_eq!(members, 94, "the ninety-four visible characters");
    assert!(
        matches("job-[[:print:]]", "job- ") && !matches("job-[[:graph:]]", "job- "),
        "the space is the whole difference between the two"
    );
}

#[test]
fn the_flags_the_names_the_negations_and_the_order_of_a_list() {
    // Measured: the folded flag answers exactly as the plain one does, for both.
    assert!(matches_folded("job-[[:print:]]", "job- "));
    assert!(!matches_folded("job-[[:graph:]]", "job- "));
    assert!(matches_folded("job-[[:print:]]", "job-!"));
    assert!(matches_folded("job-[[:graph:]]", "job-!"));

    // Neither name is known in capitals, folded or not.
    assert!(!matches("job-[[:PRINT:]]", "job-!"));
    assert!(!matches_folded("job-[[:PRINT:]]", "job-!"));
    assert!(!matches("job-[[:GRAPH:]]", "job-!"));
    assert!(!matches_folded("job-[[:GRAPH:]]", "job-!"));

    // The negations, measured on both sides of each name.
    assert!(!matches("job-[![:print:]]", "job- "));
    assert!(matches("job-[![:print:]]", "job-\t"));
    assert!(matches("job-[![:graph:]]", "job- "));
    assert!(!matches("job-[![:graph:]]", "job-!"));

    // The order in which a list is consumed, and a literal member first.
    assert!(matches("job-[[:print:][:nosuch:]]", "job- "));
    assert!(!matches("job-[[:nosuch:][:print:]]", "job- "));
    assert!(matches("job-[x[:graph:]]", "job-x"));

    // And the guards the other names keep.
    assert!(!matches("job-[[:nosuch:]]", "job-n"));
    assert!(!matches("job-[[::]]", "job-!"));
    assert!(
        !matches("job-[[:print:]-x]", "job-!"),
        "nor may print open a range"
    );
    assert!(!matches("job-[[:graph:]-x]", "job-!"), "nor may graph");
}
