//! `#{d:…}` - the parent folder of a path whose file is preceded by a run of
//! separators.
//!
//! The journey is a menu of parent folders with no limit at all: two readers,
//! one titled `/work/editor///main.rs` and one titled `/work/logs///app.log`,
//! and `#{pane_id}|#{pane_title}|#{d:pane_title}` is expected to give
//! `/work/editor` and `/work/logs`, so the editor's pane can be found even
//! though its title carries the file behind three slashes. This crate removes
//! the last component and returns everything that stands before its separator,
//! so it answers `/work/editor//` and no row of that menu equals the folder
//! looked for. A limit of twelve columns would cut those two trailing
//! separators off and hide the defect, which is why this journey asks for the
//! whole folder.
//!
//! The twelve answers below were measured before this file existed, on this
//! lot's own oracle image, with a real global option set to each exact string -
//! `set-option -g @path <value>`, read back, then `#{d:@path}` - in
//! `fdir2/cells/o2`:
//!
//! ```text
//! /work/editor///main.rs     /work/editor      /work/logs///app.log   /work/logs
//! /work/editor/main.rs       /work/editor      /work/editor///main.rs///  /work/editor
//! editor///main.rs           editor            /work//editor///main.rs    /work//editor
//! /main.rs                   /                 ///main.rs                 /
//! ```
//!
//! They say the parent keeps its interior separators and loses only the run
//! that closes it, and that a parent which is nothing but separators is the
//! root itself.
//!
//! Two of the four remaining cases were this crate's open bounds when those
//! twelve were measured: for `/` the engine and this crate agree on `/`; for
//! `///` the engine answered `/` where this crate answered `///`; for a value
//! holding the empty string the engine answers `.` where this crate answers the
//! empty string; and for an option that is not set at all the engine answers
//! the empty string, as this crate does.
//!
//! The `///` divergence is the one this round closes, and the menu below is
//! where it is asked for: a reader titled `///` is filed under the root, and a
//! reader titled `/work/logs///app.log` under `/work/logs`, so a menu of
//! parent folders holds exactly one root row and the tool of the root can be
//! found by it. The answer adopted for it is the one FDIR2 measured on its own
//! engine, kept in `fdir2/ORACLE-CONFRONTED.json`; no engine is
//! asked again here, and that lot's own receipts are untouched.
//!
//! The empty value is the one this round closes. A title that is really there
//! and holds nothing is filed under the current folder, `.`, which is what the
//! engine answered for it in those same twelve readings; a name nobody answers
//! to is absent, and an absent value has no folder at all and stays empty. The
//! two were the same string before this round, which is why the reader with an
//! empty title had no row of its own in a menu of parent folders.
//!
//! A value of exactly two separators still has NO reading of any engine behind
//! it - it was never measured - so it keeps the answer this crate already gave
//! it and is claimed as nothing else.

use super::*;

fn paths() -> FormatContext {
    FormatContext::new()
        .with_named_value("editor", "/work/editor///main.rs")
        .with_named_value("logs", "/work/logs///app.log")
        .with_named_value("single", "/work/editor/main.rs")
        .with_named_value("closed", "/work/editor///main.rs///")
        .with_named_value("relative", "editor///main.rs")
        .with_named_value("interior", "/work//editor///main.rs")
        .with_named_value("root_single", "/main.rs")
        .with_named_value("root_repeated", "///main.rs")
        .with_named_value("only_slash", "/")
        .with_named_value("only_slashes", "///")
        .with_named_value("nothing", "")
        .with_named_value("no_separator", "main.rs")
}

#[test]
fn the_folder_menu_finds_editor_after_repeated_separators() {
    let context = paths();

    // The destination: the folder each title is filed under, whole.
    assert_eq!(render_template("#{d:editor}", &context), "/work/editor");
    assert_eq!(render_template("#{d:logs}", &context), "/work/logs");
    assert_eq!(
        render_template("#{d:editor}|#{d:logs}", &context),
        "/work/editor|/work/logs"
    );

    // Measured neighbour: one separator or three, the answer is the same
    // folder, so the two rows of that menu cannot be told apart by it.
    assert_eq!(render_template("#{d:single}", &context), "/work/editor");
}

#[test]
fn relative_and_interior_separators_keep_the_parent_components() {
    let context = paths();

    // Measured: a relative path keeps its relative parent.
    assert_eq!(render_template("#{d:relative}", &context), "editor");

    // Measured: only the run that CLOSES the parent goes; the one inside it
    // stays exactly as it was written.
    assert_eq!(render_template("#{d:interior}", &context), "/work//editor");

    // The bound this helper does not move: a value with no separator at all is
    // filed under the current folder.
    assert_eq!(render_template("#{d:no_separator}", &context), ".");
}

#[test]
fn the_folder_menu_finds_dot_from_an_explicitly_empty_title() {
    let context = paths();

    // The destination: a title that is really there and holds nothing is filed
    // under the current folder, and the other reader under its own, so the menu
    // holds exactly one dot row and it belongs to the first.
    assert_eq!(render_template("#{d:nothing}", &context), ".");
    assert_eq!(render_template("#{d:logs}", &context), "/work/logs");
    assert_eq!(
        render_template("#{d:nothing}|#{d:logs}", &context),
        ".|/work/logs"
    );

    // What must not move with it: a name nobody answers to is absent, and an
    // absent value has no folder at all.
    assert_eq!(render_template("#{d:missing}", &context), "");
}

#[test]
fn empty_path_lookup_keeps_missing_basename_and_other_entry_points() {
    let context = paths();

    // D161/D9: present empty basename is `.`, while absence stays empty.
    assert_eq!(render_template("#{b:nothing}", &context), ".");
    assert_eq!(render_template("#{b:missing}", &context), "");

    // Without a path modifier, neither moves at all.
    assert_eq!(render_template("#{nothing}", &context), "");
    assert_eq!(render_template("#{missing}", &context), "");

    // An absent name, and a body naming nothing at all, are not folders.
    assert_eq!(render_template("#{d:missing}", &context), "");
    assert_eq!(render_template("#{d:}", &context), "");

    // A body that is itself a format is not reached by an outer `d`, and a
    // literal is returned as it is: both bypass this lookup entirely.
    assert_eq!(render_template("#{d:#{nothing}}", &context), "");
    assert_eq!(
        render_template("#{d;l:/work/editor/main.rs}", &context),
        "/work/editor/main.rs"
    );

    // A condition still reads an empty value as false, and the build
    // capability keeps its priority inside the lookup this round changes.
    assert_eq!(render_template("#{?nothing,yes,no}", &context), "no");
    assert_eq!(render_template("#{d:sixel_support}", &context), ".");

    // And a real path is untouched by any of it.
    assert_eq!(render_template("#{d:editor}", &context), "/work/editor");
}

#[test]
fn the_folder_menu_finds_root_from_an_all_separator_title() {
    let context = paths();

    // The destination: the reader whose title is nothing but separators is
    // filed under the root itself, and the other under its own folder, so the
    // menu holds exactly one root row and it belongs to the first.
    assert_eq!(render_template("#{d:only_slashes}", &context), "/");
    assert_eq!(render_template("#{d:logs}", &context), "/work/logs");
    assert_eq!(
        render_template("#{d:only_slashes}|#{d:logs}", &context),
        "/|/work/logs"
    );

    // The neighbours of that row, which already answered the root and are not
    // what this round changes: a title of one separator, and one whose file
    // stands behind a run of them.
    assert_eq!(render_template("#{d:only_slash}", &context), "/");
    assert_eq!(render_template("#{d:root_repeated}", &context), "/");
}

#[test]
fn trailing_file_separators_leave_the_parent_and_root_bounds_intact() {
    let context = paths();

    // Measured: separators closing the path itself are not the parent's.
    assert_eq!(render_template("#{d:closed}", &context), "/work/editor");

    // Measured: a parent that is nothing but separators is the root.
    assert_eq!(render_template("#{d:root_repeated}", &context), "/");
    assert_eq!(render_template("#{d:root_single}", &context), "/");

    // `/` is what both this crate and the engine answer, and it does not move.
    assert_eq!(render_template("#{d:only_slash}", &context), "/");

    // The bound this round closes: a title of three separators is the root,
    // which is what the engine answered when those twelve cases were measured.
    // This crate used to hand the separators back unchanged.
    assert_eq!(render_template("#{d:only_slashes}", &context), "/");

    // The bound this round closes: a value that is really there and holds
    // nothing is filed under the current folder, which is what the engine
    // answered for it. A name nobody answers to is absent, not empty, and keeps
    // the empty answer both give it.
    assert_eq!(render_template("#{d:nothing}", &context), ".");
    assert_eq!(render_template("#{d:missing}", &context), "");
}
