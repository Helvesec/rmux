//! `#{m:}`, the `*` of the pattern that meets a `*` of the text, and the `\`
//! that escapes the next character.
//!
//! Every expectation here was measured on the pinned tmux 3.7b before this lot,
//! on a server of its own, on Linux and on macOS, with the same answers on
//! both:
//!
//! ```text
//! display -p '#{m:*Z*,*MZ}|#{m:*a,*xa}|#{m:a*b,a*xb}|#{m:*.txt,*a.txt}'  1|1|1|1
//! rename-window '*build' ; display -p '#{m:*ld,#{window_name}}'          1
//! if -F '#{m:*rc,*vimrc}' 'display -p yes' 'display -p no'               yes
//! #{m:*[[:digit:]],*x7} 1   #{m:*[[:digit:]],**7} 1   #{m/i:*[[:digit:]],*X7} 1
//! #{m:*[[:digit:]],*x} 0   #{m:*[[:digit:]],*x7y} 0
//! #{m:**,*} 1   #{m:*,*} 1   #{m:?*,**} 1   #{m:*?,*} 1
//! #{m:a\*b,a*b} 1   #{m:a\*b,axb} 0   #{m:\?,?} 1   #{m:\?,x} 0
//! #{m:\[x],[x]} 1   #{m:\[x],x} 0     #{m:a\\b,a\b} 1  #{m:a\b\c,abc} 1
//! #{m:[\]]x,]x} 1   #{m:[\!a],!} 1    #{m:[\!a],b} 0   #{m:[!\!]x,!x} 0
//! #{m:[a\-z],-} 1   #{m:[a\-z],m} 0   #{m:[\a-c],b} 1  #{m:[a-\c],b} 1
//! #{m:[\\],\} 1     #{m:[x\,x} 0       #{m/i:\A*,a*} 1
//! #{m:[\[:digit:]]],d]]} 1                #{m:[\[:digit:]]],7]]} 0
//! @p = @t = 'ab\' : #{m:#{@p},#{@t}} 0    #{m:ab\\,#{@t}} 1
//! ```
//!
//! Four lists answer differently on the two hosts, and the Linux reading is
//! kept on every host. On macOS this is a divergence from tmux, declared as
//! C-D96 in tests/reference/tmux_compat/divergences.toml:
//!
//! ```text
//!                           tmux Linux   tmux macOS   rmux
//! #{m:[7\]],7}                   1            0          1
//! #{m:[[:digit:]\]],7}           1            0          1
//! #{m:[a-c\],b}                  0            1          0
//! #{m:[?\--[:digit:],7}          1            0          1
//! ```
//!
//! tmux hands the pattern to the `fnmatch(3)` of its libc. The `rangematch` of
//! Darwin's libc honours a `\` only until a member matches, and then skips to
//! the first `]` without it, so where `[7\]]` ends depends on the character
//! being tested; it also refuses a range that ends on `[:`. Reading a list the
//! Darwin way on macOS would need a platform selection in this crate.

use super::super::glob::format_fnmatch;
use super::super::FormatModifier;
use super::*;

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

/// A window renamed `*build`, and nothing else.
struct StarredWindow;

impl FormatVariables for StarredWindow {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        match variable {
            FormatVariable::WindowName => Some("*build".to_owned()),
            _ => None,
        }
    }
}

#[test]
fn a_star_of_the_pattern_stays_a_star_in_front_of_a_star_of_the_text() {
    assert_eq!(
        render_template(
            "#{m:*Z*,*MZ}|#{m:*a,*xa}|#{m:a*b,a*xb}|#{m:*.txt,*a.txt}",
            &StaticWindowValues
        ),
        "1|1|1|1"
    );
}

#[test]
fn a_window_renamed_with_a_leading_star_is_found_by_its_suffix() {
    assert_eq!(
        render_template("#{m:*ld,#{window_name}}", &StarredWindow),
        "1"
    );
    assert_eq!(render_template("#{m:*ld,*vimrc}", &StarredWindow), "0");
    assert_eq!(
        render_template("#{?#{m:*rc,*vimrc},yes,no}", &StaticWindowValues),
        "yes"
    );
}

#[test]
fn stars_and_question_marks_against_stars_of_the_text() {
    for (pattern, text) in [
        ("**", "*"),
        ("*", "*"),
        ("?*", "**"),
        ("*?", "*"),
        ("*Z*", "*MZ"),
        ("a*b", "a*xb"),
    ] {
        assert!(
            format_fnmatch(pattern, text, &plain()),
            "{pattern} must find {text}"
        );
        assert!(
            format_fnmatch(pattern, text, &folded()),
            "{pattern} must find {text} under m/i:"
        );
    }
    assert!(!format_fnmatch("*Z*", "*MY", &plain()));
    assert!(!format_fnmatch("a*b", "a*xc", &plain()));
}

/// The class route of `m/i:` and the plain route both walk the same loop: the
/// star in front of the class must absorb the `*` AND the `x` of the text
/// before the class meets the digit. Reading that star as a literal `*` left
/// nothing to backtrack to when `[[:digit:]]` then met `x`.
#[test]
fn a_star_in_front_of_a_class_backtracks_over_a_star_of_the_text() {
    assert!(format_fnmatch("*[[:digit:]]", "*x7", &plain()));
    assert!(format_fnmatch("*[[:digit:]]", "*X7", &folded()));
    assert!(format_fnmatch("*[[:digit:]]", "**7", &plain()));
    assert!(!format_fnmatch("*[[:digit:]]", "*x", &plain()));
    assert!(!format_fnmatch("*[[:digit:]]", "*x7y", &plain()));
}

#[test]
fn a_backslash_takes_the_next_character_as_an_ordinary_one() {
    assert_eq!(
        render_template(
            "#{m:*Z*,*MZ}|#{m:*a,*xa}|#{m:a*b,a*xb}|#{m:*.txt,*a.txt}|#{m:a\\*b,a*b}",
            &StaticWindowValues
        ),
        "1|1|1|1|1"
    );
    for (pattern, text, expected) in [
        ("a\\*b", "a*b", true),
        ("a\\*b", "axb", false),
        ("\\?", "?", true),
        ("\\?", "x", false),
        ("\\[x]", "[x]", true),
        ("\\[x]", "x", false),
        ("a\\\\b", "a\\b", true),
        ("a\\b\\c", "abc", true),
        ("*\\*", "ab*", true),
        ("*\\*", "ab", false),
        ("*\\?*", "a?b", true),
        ("*\\?*", "ab", false),
    ] {
        assert_eq!(
            format_fnmatch(pattern, text, &plain()),
            expected,
            "{pattern} against {text}"
        );
    }
    assert!(format_fnmatch("\\A*", "a*", &folded()));
}

#[test]
fn a_backslash_that_ends_the_pattern_matches_nothing() {
    assert!(!format_fnmatch("ab\\", "ab\\", &plain()));
    assert!(!format_fnmatch("ab\\", "ab", &plain()));
    assert!(!format_fnmatch("x*\\", "xyz\\", &plain()));
    assert!(format_fnmatch("ab\\\\", "ab\\", &plain()));
}

#[test]
fn a_backslash_inside_a_list_escapes_its_member() {
    for (pattern, text, expected) in [
        ("[\\]]x", "]x", true),
        ("[a\\]]x", "]x", true),
        ("[\\]a]", "a", true),
        ("[!\\]]x", "]x", false),
        ("[!\\]]x", "ax", true),
        ("[\\!a]", "!", true),
        ("[\\!a]", "b", false),
        ("[!\\!]x", "!x", false),
        ("[a\\-z]", "-", true),
        ("[a\\-z]", "m", false),
        ("[\\-a]x", "-x", true),
        ("[\\a-c]", "b", true),
        ("[a-\\c]", "b", true),
        ("[\\a-c]", "\\", false),
        ("[\\\\]", "\\", true),
        ("[a\\\\]x", "\\x", true),
        ("[x\\", "x", false),
    ] {
        assert_eq!(
            format_fnmatch(pattern, text, &plain()),
            expected,
            "{pattern} against {text}"
        );
    }
    assert!(format_fnmatch("[\\A-\\C]x", "bx", &folded()));
    assert!(format_fnmatch("[\\A]*", "a*", &folded()));
}

/// An escaped `[` opens no name, and the named classes already closed keep
/// their reading beside an escaped member.
#[test]
fn an_escaped_bracket_is_never_a_class_name() {
    assert!(format_fnmatch("[\\[:digit:]]]", "d]]", &plain()));
    assert!(!format_fnmatch("[\\[:digit:]]]", "7]]", &plain()));
    assert!(format_fnmatch("[\\[:digit:]]]", "[]]", &plain()));
    assert!(!format_fnmatch("\\[[:digit:]]", "[7", &plain()));
    assert!(!format_fnmatch("\\[[:UPPER:]]", "[a", &folded()));
    assert!(format_fnmatch("[[:digit:]\\]]", "]", &plain()));
    assert!(format_fnmatch("[[:digit:]\\x]", "7", &plain()));
}

/// The four lists of the module documentation, where the `fnmatch(3)` of glibc
/// and of Darwin part ways, keep the glibc reading on every host: a `\` is
/// honoured up to the `]` that closes the list, and a range may end on `[`.
#[test]
fn a_list_reads_its_escapes_up_to_its_closing_bracket_on_every_host() {
    for (pattern, text, expected) in [
        ("[7\\]]", "7", true),
        ("[[:digit:]\\]]", "7", true),
        ("[a-c\\]", "b", false),
        ("[?\\--[:digit:]", "7", true),
    ] {
        assert_eq!(
            format_fnmatch(pattern, text, &plain()),
            expected,
            "{pattern} against {text}"
        );
    }
}
