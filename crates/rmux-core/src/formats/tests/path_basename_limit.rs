//! `#{=6;b:…}` - the short name of a path, then six columns of it.
//!
//! The journey is a menu of short names limited to six columns: two readers,
//! one titled `/work/editor/` and one titled `/work/logs/`, and
//! `#{pane_id}|#{pane_title}|#{=6;b:pane_title}` is expected to give `editor`
//! and `logs`. This crate truncates first and takes the short name afterwards,
//! so both titles become `work` and the two lines cannot be told apart.
//!
//! The fourteen answers below were measured before this file existed, on this
//! lot's own oracle image, with real global options and one format each -
//! `set-option -g @path <value>` then the format, never a literal dressed up as
//! a variable - in `fblim1/cells/o2`:
//!
//! ```text
//! #{=6;b:@path}   /work/editor/  editor      #{=6;b:@path}   /work/logs/  logs
//! #{b;=6:@path}   editor         #{=3;b:@path}   edi        #{=-3;b:@path}  tor
//! #{=/3/...;b:@path}  edi...     #{=6:#{b:@path}}  editor   #{=6;b:#{@path}}  /work/
//! #{b;l:/work/editor/}  /work/editor/          #{b;s/^editor$/logs/:@path}  logs
//! #{b;p8:@path}  "editor  "      #{b;E:@indirect}  /work/editor/
//! #{b;d:@path}  .                #{b;q:@spaced}  my\ editor
//! ```
//!
//! They say where the short name is taken: at the lookup, before the expand,
//! the substitution, the limit and the padding - and the order the modifiers
//! are written in does not change it. A body that is itself a format keeps only
//! what it writes inside: `#{=6;b:#{@path}}` answers `/work/`, so the outer `b`
//! never reaches it, while `#{=6:#{b:@path}}` answers `editor`. A literal is
//! returned as it is.
//!
//! What this lot does not close is written down as it stands: the empty and
//! absent bounds FBASE1 declared are untouched, a literal WITH a limit keeps
//! its early return, and the named-condition branch does not carry these flags
//! in this crate at all.

use super::*;

fn paths() -> FormatContext {
    FormatContext::new()
        .with_named_value("editor", "/work/editor/")
        .with_named_value("logs", "/work/logs/")
        .with_named_value("indirect", "#{editor}")
        .with_named_value("spaced", "/work/my editor/")
}

#[test]
fn the_menu_shows_six_columns_of_each_short_name() {
    let context = paths();

    // The destination: the two lines the inventory has to tell apart.
    assert_eq!(render_template("#{=6;b:editor}", &context), "editor");
    assert_eq!(render_template("#{=6;b:logs}", &context), "logs");
    assert_eq!(
        render_template("#{=6;b:editor}|#{=6;b:logs}", &context),
        "editor|logs"
    );

    // The order the two modifiers are written in does not change the answer.
    assert_eq!(render_template("#{b;=6:editor}", &context), "editor");
}

#[test]
fn the_short_name_is_taken_at_the_lookup_and_everything_else_follows_it() {
    let context = paths();

    // Measured: the limit, on either side and with a marker, cuts the short
    // name and not the path.
    assert_eq!(render_template("#{=3;b:editor}", &context), "edi");
    assert_eq!(render_template("#{=-3;b:editor}", &context), "tor");
    assert_eq!(render_template("#{=/3/...;b:editor}", &context), "edi...");

    // Measured: the substitution, the padding and the expand all see the short
    // name, because it was taken before any of them ran.
    assert_eq!(
        render_template("#{b;s/^editor$/logs/:editor}", &context),
        "logs"
    );
    assert_eq!(render_template("#{b;p8:editor}", &context), "editor  ");
    assert_eq!(
        render_template("#{b;E:indirect}", &context),
        "/work/editor/"
    );

    // Measured neighbours, and their bound: `d` and `q` still run after the
    // short name is taken, which is what makes these two answers what they are.
    assert_eq!(render_template("#{b;d:editor}", &context), ".");
    assert_eq!(render_template("#{b;q:spaced}", &context), "my\\ editor");
}

#[test]
fn a_nested_body_keeps_what_it_writes_and_a_literal_keeps_itself() {
    let context = paths();

    // Measured: a body that is itself a format keeps only the modifiers it
    // writes inside; the outer short name does not reach into it.
    assert_eq!(render_template("#{=6:#{b:editor}}", &context), "editor");
    assert_eq!(render_template("#{=6;b:#{editor}}", &context), "/work/");

    // Measured: a literal is returned as it is.
    assert_eq!(
        render_template("#{b;l:/work/editor/}", &context),
        "/work/editor/"
    );

    // The bounds this lot does not close, written down as bounds. The empty and
    // absent readings are FBASE1's and are untouched here.
    assert_eq!(render_template("#{b:missing}", &context), "");
    assert_eq!(render_template("#{=6;b:missing}", &context), "");

    // And the short name of a lookup that holds no separator is the value
    // itself, limit or no limit.
    assert_eq!(
        render_template("#{b:session_name}", &StaticWindowValues),
        "alpha"
    );
    assert_eq!(
        render_template("#{=3;b:session_name}", &StaticWindowValues),
        "alp"
    );
}
