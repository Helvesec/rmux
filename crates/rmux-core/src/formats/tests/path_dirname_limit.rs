//! `#{=12;d:…}` - the parent folder of a path, then twelve columns of it.
//!
//! The journey is a menu of parent folders: two readers, one titled
//! `/work/editor/main.rs` and one titled `/work/logs/app.log`, and
//! `#{pane_id}|#{pane_title}|#{=12;d:pane_title}` is expected to give
//! `/work/editor` and `/work/logs`, so the editor's pane can be found even
//! though its title carries the file. This crate cuts to twelve columns first
//! and takes the folder afterwards, so `/work/editor` is cut out of A's title
//! before the folder is taken and A answers `/work`. B's `/work/logs` already
//! answers as it should: what is missing is the folder expected of A, not a
//! confusion of the two lines.
//!
//! The fourteen answers below were measured before this file existed, on this
//! lot's own oracle image, with real global options and one format each -
//! `set-option -g @path <value>` then the format - in
//! `fdir1/cells/o1`:
//!
//! ```text
//! #{=12;d:@path}  /work/editor   and  /work/logs      #{d;=12:@path}  /work/editor
//! #{=6;d:@path}   /work/         #{=-6;d:@path}  editor   #{=/8/...;d:@path}  /work/ed...
//! #{=12:#{d:@path}}  /work/editor        #{=12;d:#{@path}}  /work/editor
//! #{d;l:/work/editor/main.rs}  /work/editor/main.rs
//! #{d;s/editor$/project/:@path}  /work/project      #{d;p16:@path}  "/work/editor    "
//! #{d;E:@indirect}  .            #{b;d:@path}  .    #{d;q:@spaced}  /work/my\ editor
//! ```
//!
//! They say the folder is taken at the lookup, before the expand, the
//! substitution, the limit and the padding, and that the order the modifiers
//! are written in changes nothing. The two nested rows answer the same string
//! for different reasons: the explicit one takes the folder inside and the
//! limit outside, while the raw one is only cut - the outer `d` never reaches
//! into a body that is itself a format.
//!
//! What this lot does not close is written down as it stands: a literal is
//! returned as it is and a literal WITH a limit keeps its own early-return
//! debt; the empty, absent and multiple-separator readings of both helpers are
//! untouched; and the named-condition branch of this crate carries no such
//! flags at all.

use super::*;

fn paths() -> FormatContext {
    FormatContext::new()
        .with_named_value("editor", "/work/editor/main.rs")
        .with_named_value("logs", "/work/logs/app.log")
        .with_named_value("indirect", "#{editor}")
        .with_named_value("spaced", "/work/my editor/main.rs")
}

#[test]
fn the_menu_shows_twelve_columns_of_each_parent_folder() {
    let context = paths();

    // The destination: the folder each title is filed under.
    assert_eq!(render_template("#{=12;d:editor}", &context), "/work/editor");
    assert_eq!(render_template("#{=12;d:logs}", &context), "/work/logs");
    assert_eq!(
        render_template("#{=12;d:editor}|#{=12;d:logs}", &context),
        "/work/editor|/work/logs"
    );

    // The order the two modifiers are written in does not change the answer.
    assert_eq!(render_template("#{d;=12:editor}", &context), "/work/editor");
}

#[test]
fn the_folder_is_taken_at_the_lookup_and_everything_else_follows_it() {
    let context = paths();

    // Measured: the limit, on either side and with a marker, cuts the folder
    // and not the path it came from.
    assert_eq!(render_template("#{=6;d:editor}", &context), "/work/");
    assert_eq!(render_template("#{=-6;d:editor}", &context), "editor");
    assert_eq!(
        render_template("#{=/8/...;d:editor}", &context),
        "/work/ed..."
    );

    // Measured: the substitution, the padding and the expand all see the
    // folder, because it was taken before any of them ran.
    assert_eq!(
        render_template("#{d;s/editor$/project/:editor}", &context),
        "/work/project"
    );
    assert_eq!(
        render_template("#{d;p16:editor}", &context),
        "/work/editor    "
    );
    assert_eq!(render_template("#{d;E:indirect}", &context), ".");

    // Measured neighbour: with both names the short one is taken first, so what
    // is left has no separator and its folder is the current one.
    assert_eq!(render_template("#{b;d:editor}", &context), ".");
    assert_eq!(
        render_template("#{d;q:spaced}", &context),
        "/work/my\\ editor"
    );
}

#[test]
fn a_nested_body_a_literal_and_the_two_names_together() {
    let context = paths();

    // Measured: both nested rows answer the same string for different reasons -
    // the explicit one takes the folder inside and is cut outside, the raw one
    // is only cut, because the outer `d` does not reach into it.
    assert_eq!(
        render_template("#{=12:#{d:editor}}", &context),
        "/work/editor"
    );
    assert_eq!(
        render_template("#{=12;d:#{editor}}", &context),
        "/work/editor"
    );

    // Measured: a literal is returned as it is.
    assert_eq!(
        render_template("#{d;l:/work/editor/main.rs}", &context),
        "/work/editor/main.rs"
    );

    // An extension of this reading, not a row the oracle was asked: written the
    // other way round, the two names still run short-name-then-folder.
    assert_eq!(render_template("#{d;b:editor}", &context), ".");

    // The bounds neither helper moves: nothing to look up stays nothing, and a
    // value that holds no separator is filed under the current folder.
    assert_eq!(render_template("#{d:missing}", &context), "");
    assert_eq!(render_template("#{=12;d:missing}", &context), "");
    assert_eq!(
        render_template("#{d:session_name}", &StaticWindowValues),
        "."
    );
}
