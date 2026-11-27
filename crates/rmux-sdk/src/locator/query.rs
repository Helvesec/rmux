use crate::{PaneSnapshot, Result, RmuxError};

use super::{LocatorFilter, LocatorMatch, LocatorQuery, LocatorSelection, LocatorText};

pub(super) fn evaluate_query(
    query: &LocatorQuery,
    snapshot: &PaneSnapshot,
) -> Result<Vec<LocatorMatch>> {
    match query {
        LocatorQuery::Text(text) => evaluate_text(text, snapshot),
        LocatorQuery::Or(left, right) => {
            let mut matches = evaluate_query(left, snapshot)?;
            for item in evaluate_query(right, snapshot)? {
                if !matches.iter().any(|existing| existing == &item) {
                    matches.push(item);
                }
            }
            Ok(matches)
        }
        LocatorQuery::And(left, right) => {
            let left = evaluate_query(left, snapshot)?;
            let right = evaluate_query(right, snapshot)?;
            Ok(left
                .into_iter()
                .filter(|item| right.iter().any(|other| other == item))
                .collect())
        }
    }
}

fn evaluate_text(text: &LocatorText, snapshot: &PaneSnapshot) -> Result<Vec<LocatorMatch>> {
    match text {
        LocatorText::Literal(needle) => Ok(snapshot
            .find_text_all(needle)
            .into_iter()
            .map(|text_match| LocatorMatch { text_match })
            .collect()),
        #[cfg(feature = "regex")]
        LocatorText::Regex(pattern) => evaluate_regex(&compile_pattern(pattern)?, snapshot),
        #[cfg(feature = "regex")]
        LocatorText::CompiledRegex(regex) => evaluate_regex(regex.as_regex(), snapshot),
    }
}

#[cfg(feature = "regex")]
fn compile_pattern(pattern: &str) -> Result<regex::Regex> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1_000_000)
        .dfa_size_limit(1_000_000)
        .build()
        .map_err(|error| RmuxError::invalid_regex(pattern, error.to_string()))
}

#[cfg(feature = "regex")]
fn evaluate_regex(regex: &regex::Regex, snapshot: &PaneSnapshot) -> Result<Vec<LocatorMatch>> {
    let mut matches = Vec::new();
    for row in 0..snapshot.rows {
        let line = snapshot.row_text(row);
        for item in regex.find_iter(&line) {
            if let Some(text_match) = crate::extract::text_match_for_rendered_row_range(
                snapshot,
                row,
                &line,
                item.start(),
                item.end(),
            ) {
                matches.push(LocatorMatch { text_match });
            }
        }
    }
    Ok(matches)
}

pub(super) fn apply_filter(matches: &mut Vec<LocatorMatch>, filter: &LocatorFilter) -> Result<()> {
    if filter.visible == Some(false) {
        return Err(RmuxError::from_proto(rmux_proto::RmuxError::Server(
            "locator filter visible=false is unsupported for terminal snapshots; use wait_for_state(LocatorState::Hidden) or expect().to_be_hidden()"
                .to_owned(),
        )));
    }
    if let Some(needle) = &filter.has_text {
        matches.retain(|item| item.text_match.text.contains(needle));
    }
    if let Some(needle) = &filter.has_not_text {
        matches.retain(|item| !item.text_match.text.contains(needle));
    }
    Ok(())
}

pub(super) fn apply_selection(
    matches: Vec<LocatorMatch>,
    selection: LocatorSelection,
) -> Vec<LocatorMatch> {
    match selection {
        LocatorSelection::Strict => matches,
        LocatorSelection::First => matches.into_iter().take(1).collect(),
        LocatorSelection::Last => matches.into_iter().rev().take(1).collect(),
        LocatorSelection::Nth(index) => matches.into_iter().nth(index).into_iter().collect(),
    }
}

pub(super) fn describe_query(query: &LocatorQuery) -> String {
    match query {
        LocatorQuery::Text(LocatorText::Literal(text)) => format!("text={text:?}"),
        #[cfg(feature = "regex")]
        LocatorQuery::Text(LocatorText::Regex(pattern)) => format!("regex={pattern:?}"),
        #[cfg(feature = "regex")]
        LocatorQuery::Text(LocatorText::CompiledRegex(regex)) => {
            format!("regex={:?}", regex.as_regex().as_str())
        }
        LocatorQuery::Or(left, right) => {
            format!("({} or {})", describe_query(left), describe_query(right))
        }
        LocatorQuery::And(left, right) => {
            format!("({} and {})", describe_query(left), describe_query(right))
        }
    }
}

#[cfg(all(test, feature = "regex"))]
mod tests {
    use super::*;
    use crate::{PaneCell, PaneCursor, PaneGlyph};

    fn cell(text: &str) -> PaneCell {
        PaneCell::new(PaneGlyph::new(text, 1))
    }

    fn wide(text: &str, width: u8) -> PaneCell {
        PaneCell::new(PaneGlyph::new(text, width))
    }

    fn snapshot_from_rows(rows: &[&str]) -> PaneSnapshot {
        let cols = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0) as u16;
        let mut cells = Vec::new();
        for row in rows {
            let mut written = 0_u16;
            for ch in row.chars() {
                cells.push(cell(&ch.to_string()));
                written += 1;
            }
            while written < cols {
                cells.push(cell(" "));
                written += 1;
            }
        }
        PaneSnapshot::new(cols, rows.len() as u16, cells, PaneCursor::default())
            .expect("valid snapshot")
    }

    #[test]
    fn regex_matches_use_terminal_columns_for_wide_cells() {
        let snapshot = PaneSnapshot::new(
            4,
            1,
            vec![cell("A"), wide("界", 2), PaneCell::padding(), cell("B")],
            PaneCursor::default(),
        )
        .expect("valid snapshot");

        let regex = compile_pattern("界B").expect("valid regex");
        let matches = evaluate_regex(&regex, &snapshot).expect("regex evaluates");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text_match.start_col, 1);
        assert_eq!(matches[0].text_match.end_col, 4);
        assert_eq!(matches[0].text_match.text, "界B");
    }

    #[test]
    fn locator_or_deduplicates_and_and_intersects_by_coordinates() {
        let snapshot = snapshot_from_rows(&["Ready Ready Done"]);
        let ready = LocatorQuery::Text(LocatorText::Literal("Ready".to_owned()));
        let done = LocatorQuery::Text(LocatorText::Literal("Done".to_owned()));

        let same_or = LocatorQuery::Or(Box::new(ready.clone()), Box::new(ready.clone()));
        assert_eq!(evaluate_query(&same_or, &snapshot).unwrap().len(), 2);

        let mixed_or = LocatorQuery::Or(Box::new(ready.clone()), Box::new(done.clone()));
        assert_eq!(evaluate_query(&mixed_or, &snapshot).unwrap().len(), 3);

        let same_and = LocatorQuery::And(Box::new(ready.clone()), Box::new(ready));
        assert_eq!(evaluate_query(&same_and, &snapshot).unwrap().len(), 2);

        let empty_and = LocatorQuery::And(
            Box::new(LocatorQuery::Text(LocatorText::Literal("Ready".to_owned()))),
            Box::new(done),
        );
        assert!(evaluate_query(&empty_and, &snapshot).unwrap().is_empty());
    }

    #[test]
    fn visible_false_filter_rejects_terminal_snapshot_queries() {
        let snapshot = snapshot_from_rows(&["Ready"]);
        let query = LocatorQuery::Text(LocatorText::Literal("Ready".to_owned()));
        let mut matches = evaluate_query(&query, &snapshot).unwrap();

        let error = apply_filter(
            &mut matches,
            &LocatorFilter {
                visible: Some(false),
                ..LocatorFilter::default()
            },
        )
        .expect_err("visible=false is unsupported for terminal snapshots");

        assert!(error.to_string().contains("visible=false"));
    }

    #[test]
    fn nth_selection_out_of_range_returns_no_match() {
        let snapshot = snapshot_from_rows(&["Ready"]);
        let query = LocatorQuery::Text(LocatorText::Literal("Ready".to_owned()));
        let matches = evaluate_query(&query, &snapshot).unwrap();

        let selected = apply_selection(matches, LocatorSelection::Nth(4));

        assert!(selected.is_empty());
    }
}
