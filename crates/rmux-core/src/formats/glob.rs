use super::regex_cache::cached_regex;
use super::FormatModifier;

pub(super) fn format_fnmatch(pattern: &str, text: &str, fm: &FormatModifier) -> bool {
    let flags = fm.argv.first().map(String::as_str).unwrap_or_default();
    let case_insensitive = flags.contains('i');

    if flags.contains('r') {
        return cached_regex(pattern, case_insensitive).is_ok_and(|regex| regex.is_match(text));
    }
    // A folded pattern that writes no class name keeps the reading it had
    // before this lot: both sides lowercased whole, once, by
    // `str::to_lowercase`. That is what makes `İ` become `i` and a combining
    // dot, and it is why `#{m/i:i,İ}` answers 0 while `#{m/i:i?,İ}` answers 1.
    // Folding character by character is reserved for the patterns that really
    // hold a name, where the name itself must not be folded. Which of the two a
    // pattern is, is read and not guessed: looking for `[:` in the text of the
    // pattern called the ordinary list `[:I:]` a name and moved it too.
    if case_insensitive && !holds_a_class_name(pattern) {
        return glob_match(&pattern.to_lowercase(), &text.to_lowercase(), false);
    }
    glob_match(pattern, text, case_insensitive)
}

/// Minimal glob matching supporting `*`, `?`, and character classes `[...]`.
fn glob_match(pattern: &str, text: &str, fold: bool) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    glob_match_impl(&pat, &txt, fold)
}

/// What `m/i:` folds on the class route: whatever is COMPARED, and only that.
///
/// Lowercasing the whole pattern also lowercased the name of a character class,
/// which made `[:DIGIT:]` a valid spelling here while the oracle answers 0 for
/// it - `fnmatch(3)` receives `FNM_CASEFOLD`, which folds the characters being
/// compared and never looks at a class name at all.
///
/// Only the patterns that carry `[:` come here; the others keep the whole
/// lowercase they had, because these two are not the same reading wherever a
/// character lowercases to more than one - `İ` and a final sigma among them.
fn fold_char(ch: char, fold: bool) -> char {
    if fold {
        ch.to_lowercase().next().unwrap_or(ch)
    } else {
        ch
    }
}

/// The backtracking walk of `fnmatch(3)`, linear in pattern x text.
///
/// A `*` of the pattern is SYNTAX before it is a character, exactly like `[`.
/// Testing literal equality first let a `*` of the pattern consume a `*` of the
/// text as an ordinary character, and that star was then never remembered for
/// backtracking: `#{m:*Z*,*MZ}` and `#{m:a*b,a*xb}` answered 0, and the oracle
/// answers 1 for both, as it does for a window renamed `*build` and the pattern
/// `*ld`. Only the most recent star has to absorb one more character when an
/// atom fails, because every other atom consumes exactly one.
fn glob_match_impl(pat: &[char], txt: &[char], fold: bool) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    // Pattern index just past the latest run of stars, and the text index that
    // star currently stops before.
    let mut latest_star: Option<(usize, usize)> = None;

    while ti < txt.len() {
        if pat.get(pi) == Some(&'*') {
            pi = skip_stars(pat, pi);
            latest_star = Some((pi, ti));
            continue;
        }
        if let Some(next) = match_atom(pat, pi, txt[ti], fold) {
            pi = next;
            ti += 1;
            continue;
        }
        let Some((star_pi, star_ti)) = latest_star else {
            return false;
        };
        latest_star = Some((star_pi, star_ti + 1));
        pi = star_pi;
        ti = star_ti + 1;
    }

    skip_stars(pat, pi) == pat.len()
}

fn skip_stars(pat: &[char], mut pi: usize) -> usize {
    while pat.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi
}

/// Matches the one-character atom at `pat[pi]` - never a `*` - against `ch`,
/// and returns the pattern index after it, or `None` when it does not match or
/// the pattern is exhausted.
fn match_atom(pat: &[char], pi: usize, ch: char, fold: bool) -> Option<usize> {
    match *pat.get(pi)? {
        '?' => Some(pi + 1),
        // `[` is read as SYNTAX before it is read as a character. Testing
        // literal equality first made `[[]` - the class whose only member is an
        // opening bracket - consume that bracket as text, so it never matched
        // `[`, and let a pattern like `[!]]` walk past the class parser
        // entirely. The oracle answers 1 for `#{m:[[],[}`.
        //
        // A class that is never closed matches nothing: the oracle answers 0
        // for `#{m:job-[,job-[}` and `#{m:[abc,[abc}`.
        '[' => match match_char_class(&pat[pi..], ch, fold) {
            Some((true, width)) => Some(pi + width),
            Some((false, _)) | None => None,
        },
        // `\` takes the next character as an ordinary one, whatever it is:
        // the oracle answers 1 for `#{m:a\*b,a*b}` and 0 for `#{m:a\*b,axb}`.
        // A backslash that ends the pattern escapes nothing and matches
        // nothing - `#{m:#{@p},#{@t}}` with both set to `ab\` answers 0.
        '\\' => {
            let literal = *pat.get(pi + 1)?;
            (fold_char(literal, fold) == fold_char(ch, fold)).then_some(pi + 2)
        }
        literal => (fold_char(literal, fold) == fold_char(ch, fold)).then_some(pi + 1),
    }
}

/// One item of a bracket list, read the same way by the matcher and by the
/// discriminator in front of it.
enum ListItem {
    /// `[:name:]`: the name lies between `from` and `end - 2`, and `end` is the
    /// index just past the `:]` that closed it.
    Name { from: usize, end: usize },
    /// `[:` that nothing closes with `:]`.
    UnclosedName,
    /// `a-b`: one range from `first` to `last`, three characters wide, or
    /// more when an endpoint is written escaped.
    Range {
        first: char,
        last: char,
        width: usize,
    },
    /// One ordinary character, which may well be a `[`; two wide when it is
    /// written escaped.
    Literal { member: char, width: usize },
}

/// The member written at `pat[i]` and its width: a `\` takes the character
/// after it as an ordinary one, so `[\]]`, `[\!a]` and `[a\-z]` hold `]`, `!`
/// and `-` - the oracle answers 1 for `#{m:[\]]x,]x}`, 0 for `#{m:[\!a],b}`
/// and 0 for `#{m:[a\-z],m}`. A backslash that ends the pattern is itself,
/// and the list it stands in is never closed.
fn read_member(pat: &[char], i: usize) -> (char, usize) {
    match pat.get(i + 1) {
        Some(next) if pat[i] == '\\' => (*next, 2),
        _ => (pat[i], 1),
    }
}

/// What stands at `pat[i]` inside a bracket list.
///
/// A `[` followed by `:` is a name before it is a literal; a `[` that a range
/// has already taken as its endpoint is never read here at all, which is what
/// keeps `[a-[:DIGIT:]]` an ordinary list of members.
fn read_list_item(pat: &[char], i: usize) -> ListItem {
    if pat[i] == '[' && i + 1 < pat.len() && pat[i + 1] == ':' {
        return match class_name_end(pat, i + 2) {
            Some(end) => ListItem::Name { from: i + 2, end },
            None => ListItem::UnclosedName,
        };
    }
    let (first, first_width) = read_member(pat, i);
    let dash = i + first_width;
    if dash + 1 < pat.len() && pat[dash] == '-' {
        let (last, last_width) = read_member(pat, dash + 1);
        return ListItem::Range {
            first,
            last,
            width: first_width + 1 + last_width,
        };
    }
    ListItem::Literal {
        member: first,
        width: first_width,
    }
}

/// Whether `pattern` really writes a class name.
///
/// The folded reading of a pattern that holds no name has to stay the one it
/// had before this lot, and that cannot be decided by looking for `[:` in the
/// text of the pattern: `[:I:]` is a list of three ordinary members, and the
/// `[` of `[a-[:DIGIT:]]` has already been taken as the end of a range. So the
/// question is answered by the reading the matcher itself uses - each list is
/// opened, its negation and the `]` written first are passed, its items are
/// read in order, and the outside resumes after the `]` that closed it.
pub(super) fn holds_a_class_name(pattern: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let mut pi = 0;
    while pi < pat.len() {
        // An escaped `[` opens no list: `\[[:digit:]]` holds no name.
        if pat[pi] == '\\' {
            pi += 2;
            continue;
        }
        if pat[pi] != '[' {
            pi += 1;
            continue;
        }
        let mut i = pi + 1;
        if i < pat.len() && (pat[i] == '!' || pat[i] == '^') {
            i += 1;
        }
        if i < pat.len() && pat[i] == ']' {
            i += 1;
        }
        while i < pat.len() && pat[i] != ']' {
            match read_list_item(&pat, i) {
                ListItem::Name { .. } | ListItem::UnclosedName => return true,
                ListItem::Range { width, .. } | ListItem::Literal { width, .. } => i += width,
            }
        }
        pi = if i < pat.len() { i + 1 } else { i };
    }
    false
}

/// The end of the `[:name:]` whose name starts at `from`, or `None` when that
/// name is never closed.
fn class_name_end(pat: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < pat.len() {
        if pat[i] == ':' && pat[i + 1] == ']' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Whether the named class holds the character, or `None` when this reading
/// does not know that name.
///
/// Two characters reach here, and which one a name reads is measured, not
/// chosen: `ch` is what the TEXT carries and `folded` is that character after
/// `m/i:` has lowered it.
///
/// * `digit`, `alpha`, `alnum`, `punct`, `space`, `xdigit`, `print` and `graph`
///   read `ch`. The
///   oracle answers the same for `#{m:...}` and `#{m/i:...}` on the same text -
///   GALPHA1 measured it for `alpha`, GALNUM1 for `alnum`, GPUNCT1 for `punct`,
///   GSPACE1 for `space` and GXDIGIT1 for `xdigit`, where the flag neither
///   folds `G` into a member nor takes `F` away - and a class asked about the
///   folded character would not: `İ` and `K` would become letters under the
///   flag and not without it.
/// * `upper` and `lower` read `folded`, because that is what the oracle does
///   with them: `#{m/i:job-[[:upper:]],job-A}` is 0 and
///   `#{m/i:job-[[:lower:]],job-A}` is 1. With the flag the character is lowered
///   before the class is asked, so `upper` holds nothing and `lower` holds both
///   cases. GCASE1 measured that rather than presuming it from `alpha`.
///
/// GDIGIT1 measured `digit`, GALPHA1 `alpha`, GCASE1 `upper` and `lower`,
/// GALNUM1 `alnum`, GPUNCT1 `punct`, GSPACE1 `space`, GXDIGIT1 `xdigit` and
/// GPRINT1 `print` and `graph`, all against tmux 3.7b and all ASCII. Every other POSIX name stays unknown here on
/// purpose: an unknown name refuses the pattern, which is what this crate
/// already answered for those names, without the false positives their letters
/// used to produce.
fn class_holds(name: &[char], ch: char, folded: char) -> Option<bool> {
    if name.iter().copied().eq("digit".chars()) {
        return Some(ch.is_ascii_digit());
    }
    if name.iter().copied().eq("alpha".chars()) {
        return Some(ch.is_ascii_alphabetic());
    }
    if name.iter().copied().eq("alnum".chars()) {
        return Some(ch.is_ascii_alphanumeric());
    }
    if name.iter().copied().eq("punct".chars()) {
        return Some(ch.is_ascii_punctuation());
    }
    if name.iter().copied().eq("graph".chars()) {
        return Some(ch.is_ascii_graphic());
    }
    if name.iter().copied().eq("print".chars()) {
        // The visible characters and the space, and that space is the whole
        // difference between the two names.
        return Some(ch == ' ' || ch.is_ascii_graphic());
    }
    if name.iter().copied().eq("xdigit".chars()) {
        return Some(ch.is_ascii_hexdigit());
    }
    if name.iter().copied().eq("space".chars()) {
        // The six the oracle answered for, VERTICAL TAB among them.
        // `is_ascii_whitespace` follows the WhatWG list and leaves U+000B out -
        // its own documentation says the POSIX locale keeps it - so the list is
        // written here instead of borrowed.
        return Some(matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r'));
    }
    if name.iter().copied().eq("upper".chars()) {
        return Some(folded.is_ascii_uppercase());
    }
    if name.iter().copied().eq("lower".chars()) {
        return Some(folded.is_ascii_lowercase());
    }
    None
}

/// Matches a character class `[...]` at the start of `pat`, returning
/// `(matches, length_consumed)` or `None` if malformed.
fn match_char_class(pat: &[char], ch: char, fold: bool) -> Option<(bool, usize)> {
    if pat.is_empty() || pat[0] != '[' {
        return None;
    }
    // What the members are compared with, beside the character the text really
    // carries: the literals and the ranges use the folded one, and a name is
    // given both, because which of the two it reads depends on the name and was
    // measured for each.
    let folded = fold_char(ch, fold);
    let mut i = 1;
    let negate = i < pat.len() && (pat[i] == '!' || pat[i] == '^');
    if negate {
        i += 1;
    }

    let mut matched = false;
    // A `]` written FIRST - straight after `[`, or straight after the negation -
    // is a MEMBER of the class, not its terminator. glob(7) says so, and the
    // oracle answers 1 for `#{m:job-[]],job-]}` and 1 for `#{m:job-[!]],job-x}`.
    // Stopping at that first `]` read `[]]` as an empty class with a stray
    // bracket behind it, which matched nothing this journey needs.
    if i < pat.len() && pat[i] == ']' {
        if fold_char(pat[i], fold) == folded {
            matched = true;
        }
        i += 1;
    }
    while i < pat.len() && pat[i] != ']' {
        // `[:name:]` is SYNTAX inside the list, before its letters are members.
        // Reading it character by character made `job-[[:digit:]]` a list of
        // `[`, `:`, `d`, `i`, `g`, `i`, `t`, `:` closed by the inner `]`, with
        // the outer one left over as a literal: `job-7` was missed and `job-d]`
        // was found. The oracle answers 1 and 0 the other way round.
        //
        // Once a member has matched, the rest of the list is only CONSUMED:
        // that is what the oracle does, and it is why `job-[[:digit:][:nosuch:]]`
        // answers 1 for `job-7` while `job-[[:nosuch:][:digit:]]` answers 0.
        match read_list_item(pat, i) {
            ListItem::UnclosedName => return None,
            ListItem::Name { from, end } => {
                if !matched {
                    // A class cannot open a range: the oracle answers 0 for
                    // `#{m:job-[[:digit:]-x],job-7}` as it does for every text.
                    if pat.get(end) == Some(&'-')
                        && pat.get(end + 1).is_some_and(|next| *next != ']')
                    {
                        return None;
                    }
                    if class_holds(&pat[from..end - 2], ch, folded)? {
                        matched = true;
                    }
                }
                i = end;
            }
            ListItem::Range { first, last, width } => {
                if !matched && folded >= fold_char(first, fold) && folded <= fold_char(last, fold) {
                    matched = true;
                }
                i += width;
            }
            ListItem::Literal { member, width } => {
                if !matched && fold_char(member, fold) == folded {
                    matched = true;
                }
                i += width;
            }
        }
    }

    if i >= pat.len() {
        return None;
    }
    let result = if negate { !matched } else { matched };
    Some((result, i + 1))
}
