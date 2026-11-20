//! `#{b:…}` on a path that ends in a slash, and the short name it should give.
//!
//! The journey is an inventory that finds the editor's pane by the short name
//! of its title: two readers, one titled `/work/editor/` and one titled
//! `/work/logs/`, and `#{pane_id}|#{pane_title}|#{b:pane_title}` is expected to
//! give `editor` for the first and `logs` for the second. `formats.rs` takes
//! the suffix after the LAST slash, so a title that ends in one gives the empty
//! string and no line can be found.
//!
//! The answers below were measured before this file existed, on this lot's own
//! oracle image, by setting a global user option and reading the modifier back
//! from it - `set-option -g @path <value>` then `#{b:@path}`, never a literal
//! written into the format - in `fbase1/cells/o3`:
//!
//! ```text
//! /work/editor/    editor      /work/editor      editor
//! /work/editor///  editor      editor///         editor
//! editor           editor      /                 /
//! ///              /           @path set to ""   .
//! @path unset      (empty)
//! ```
//!
//! An explicitly empty value answers `.`, while an absent name stays empty.
//! Both bounds were independently remeasured on tmux 3.7b for D161/D9.
//! Nothing is claimed for a backslash, for `std::path` or for any rule that
//! depends on the operating system.

use super::*;

#[test]
fn the_editor_and_the_logs_are_found_by_the_short_name_of_their_titles() {
    let context = FormatContext::new()
        .with_named_value("editor", "/work/editor/")
        .with_named_value("logs", "/work/logs/");

    // The destination itself: the two names the inventory has to derive.
    assert_eq!(render_template("#{b:editor}", &context), "editor");
    assert_eq!(render_template("#{b:logs}", &context), "logs");

    // And the pair as one line reads them, which is what the journey extracts.
    assert_eq!(
        render_template("#{b:editor}|#{b:logs}", &context),
        "editor|logs"
    );
}

#[test]
fn a_path_keeps_its_short_name_however_many_slashes_close_it() {
    let context = FormatContext::new()
        .with_named_value("plain", "/work/editor")
        .with_named_value("one", "/work/editor/")
        .with_named_value("three", "/work/editor///")
        .with_named_value("bare", "editor")
        .with_named_value("bare_three", "editor///")
        .with_named_value("root", "/")
        .with_named_value("root_three", "///")
        .with_named_value("blank", "");

    // Measured, one row each: the trailing slashes are closed off, not read.
    assert_eq!(render_template("#{b:plain}", &context), "editor");
    assert_eq!(render_template("#{b:one}", &context), "editor");
    assert_eq!(render_template("#{b:three}", &context), "editor");
    assert_eq!(render_template("#{b:bare}", &context), "editor");
    assert_eq!(render_template("#{b:bare_three}", &context), "editor");
    assert_eq!(render_template("#{b:root}", &context), "/");
    assert_eq!(render_template("#{b:root_three}", &context), "/");

    // A present empty value has a basename; an absent name has none.
    assert_eq!(
        render_template("#{b:blank}", &context),
        ".",
        "a present empty value has basename `.` in measured tmux 3.7b"
    );
    assert_eq!(render_template("#{b:missing}", &context), "");

    // The neighbours this touches keep their meaning: dirname is untouched, and
    // a chain still sees the short name first.
    assert_eq!(render_template("#{d:one}", &context), "/work");
    assert_eq!(render_template("#{n:#{b:one}}", &context), "6");
}

#[test]
fn empty_basename_preserves_presence_through_modifiers_and_bypass_paths() {
    let present = FormatContext::new().with_named_value("path", "");
    let absent = FormatContext::new();

    for (context, expected) in [
        (&present, "[]|[.]|[.]|[no]|[0]|[1]|[1]|[1]|[.]"),
        (&absent, "[]|[]|[]|[no]|[0]|[0]|[0]|[0]|[]"),
    ] {
        assert_eq!(
            render_template(
                "[#{path}]|[#{b:path}]|[#{d:path}]|[#{?path,yes,no}]|[#{n:path}]|[#{b;n:path}]|[#{n;b:path}]|[#{n:#{b:path}}]|[#{b;d:path}]",
                context,
            ),
            expected
        );
        assert_eq!(
            render_template(
                "[#{b:}]|[#{d:}]|[#{b:#{path}}]|[#{d:#{path}}]|[#{b;l:}]|[#{d;l:}]",
                context,
            ),
            "[]|[]|[]|[]|[]|[]"
        );
    }
}
