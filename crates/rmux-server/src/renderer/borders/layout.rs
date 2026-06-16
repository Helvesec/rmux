use std::collections::BTreeMap;

use rmux_core::{Pane, PaneGeometry, Window};

use crate::pane_visible_geometry::clip_pane_geometry;

use super::super::StatusGeometry;

pub(super) fn border_layout_cells_with_geometry(
    window: &Window,
    active_pane_index: u32,
    geometry: StatusGeometry,
    indicators_colour: bool,
) -> Vec<BorderLayoutCell> {
    let size = geometry.content_size();
    let panes = window.panes();
    if panes.len() <= 1 || size.cols == 0 || size.rows == 0 {
        return Vec::new();
    }

    let mut vertical_segments = Vec::new();
    let mut horizontal_segments = Vec::new();
    let pane_geometries = panes
        .iter()
        .map(|pane| {
            (
                pane.index(),
                content_pane_geometry(pane, geometry.content_rows),
            )
        })
        .collect::<Vec<_>>();

    for (index, (left_index, left_geometry)) in pane_geometries.iter().enumerate() {
        for (right_index, right_geometry) in pane_geometries.iter().skip(index + 1) {
            if let Some(segment) =
                shared_vertical_boundary(*left_index, *left_geometry, *right_index, *right_geometry)
            {
                vertical_segments.push(segment);
            }
            if let Some(segment) = shared_horizontal_boundary(
                *left_index,
                *left_geometry,
                *right_index,
                *right_geometry,
            ) {
                horizontal_segments.push(segment);
            }
        }
    }
    vertical_segments = normalize_vertical_segments(vertical_segments);
    horizontal_segments = normalize_horizontal_segments(horizontal_segments);

    let mut cells = BTreeMap::new();
    for segment in &vertical_segments {
        for y in segment.start..=segment.end {
            let state = cell_state_mut(&mut cells, segment.x, y);
            state.mark_vertical(y > segment.start, y < segment.end);
            state.add_adjacent_pane(segment.first_index);
            state.add_adjacent_pane(segment.second_index);
        }
    }

    for segment in &horizontal_segments {
        let connects_left = segment.start > 0
            && vertical_boundary_continues(&cells, segment.start - 1, segment.y, size.rows);
        let connects_right = segment.end + 1 < size.cols
            && vertical_boundary_continues(&cells, segment.end + 1, segment.y, size.rows);

        for x in segment.start..=segment.end {
            let state = cell_state_mut(&mut cells, x, segment.y);
            state.mark_horizontal(
                x > segment.start || connects_left,
                x < segment.end || connects_right,
            );
            state.add_adjacent_pane(segment.first_index);
            state.add_adjacent_pane(segment.second_index);
        }

        if connects_left {
            let left_adjacent =
                vertical_neighbour_panes(&cells, segment.start - 1, segment.y, size.rows);
            let (up, down) =
                vertical_boundary_directions(&cells, segment.start - 1, segment.y, size.rows);
            let left_state = cell_state_mut(&mut cells, segment.start - 1, segment.y);
            left_state.mark_vertical(up, down);
            left_state.mark_horizontal(false, true);
            left_state.add_adjacent_pane(segment.first_index);
            left_state.add_adjacent_pane(segment.second_index);
            for pane_index in left_adjacent {
                left_state.add_adjacent_pane(pane_index);
            }
        }
        if connects_right {
            let right_adjacent =
                vertical_neighbour_panes(&cells, segment.end + 1, segment.y, size.rows);
            let (up, down) =
                vertical_boundary_directions(&cells, segment.end + 1, segment.y, size.rows);
            let right_state = cell_state_mut(&mut cells, segment.end + 1, segment.y);
            right_state.mark_vertical(up, down);
            right_state.mark_horizontal(true, false);
            right_state.add_adjacent_pane(segment.first_index);
            right_state.add_adjacent_pane(segment.second_index);
            for pane_index in right_adjacent {
                right_state.add_adjacent_pane(pane_index);
            }
        }
    }

    cells
        .into_iter()
        .filter_map(|((x, y), cell)| {
            let output_y = y.saturating_add(geometry.content_y_offset);
            if output_y >= geometry.terminal_size.rows {
                return None;
            }
            let (owner_pane_index, active) = border_cell_owner_and_activity(
                x,
                y,
                &cell.adjacent_panes,
                &pane_geometries,
                active_pane_index,
                indicators_colour,
            );
            Some(BorderLayoutCell {
                x,
                y: output_y,
                glyph: cell.glyph()?,
                owner_pane_index,
                active,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct VerticalBoundarySegment {
    x: u16,
    start: u16,
    end: u16,
    first_index: u32,
    second_index: u32,
}

#[derive(Debug, Clone, Copy)]
struct HorizontalBoundarySegment {
    y: u16,
    start: u16,
    end: u16,
    first_index: u32,
    second_index: u32,
}

#[derive(Debug, Clone, Default)]
struct BorderCellState {
    vertical: bool,
    vertical_up: bool,
    vertical_down: bool,
    horizontal: bool,
    horizontal_left: bool,
    horizontal_right: bool,
    adjacent_panes: Vec<u32>,
}

impl BorderCellState {
    fn mark_vertical(&mut self, up: bool, down: bool) {
        self.vertical = true;
        self.vertical_up |= up;
        self.vertical_down |= down;
    }

    fn mark_horizontal(&mut self, left: bool, right: bool) {
        self.horizontal = true;
        self.horizontal_left |= left;
        self.horizontal_right |= right;
    }

    fn add_adjacent_pane(&mut self, pane_index: u32) {
        if !self.adjacent_panes.contains(&pane_index) {
            self.adjacent_panes.push(pane_index);
        }
    }

    fn has_vertical(&self) -> bool {
        self.vertical
    }

    fn glyph(&self) -> Option<char> {
        let left = self.horizontal && (self.horizontal_left || !self.horizontal_right);
        let right = self.horizontal && (self.horizontal_right || !self.horizontal_left);
        let up = self.vertical && (self.vertical_up || !self.vertical_down);
        let down = self.vertical && (self.vertical_down || !self.vertical_up);

        if self.vertical && self.horizontal {
            return Some(match (left, right, up, down) {
                (true, true, true, true) => '┼',
                (true, true, false, true) => '┬',
                (true, true, true, false) => '┴',
                (true, false, true, true) => '┤',
                (false, true, true, true) => '├',
                (true, false, true, false) => '┘',
                (true, false, false, true) => '┐',
                (false, true, true, false) => '└',
                (false, true, false, true) => '┌',
                (false, false, true, true) => '│',
                (true, true, false, false) => '─',
                _ => '┼',
            });
        }
        if self.vertical {
            return Some('│');
        }
        if self.horizontal {
            return Some('─');
        }
        None
    }
}

fn vertical_boundary_continues(
    cells: &BTreeMap<(u16, u16), BorderCellState>,
    x: u16,
    y: u16,
    total_rows: u16,
) -> bool {
    cells
        .get(&(x, y))
        .is_some_and(BorderCellState::has_vertical)
        || (y > 0
            && cells
                .get(&(x, y - 1))
                .is_some_and(BorderCellState::has_vertical))
        || (y.saturating_add(1) < total_rows
            && cells
                .get(&(x, y + 1))
                .is_some_and(BorderCellState::has_vertical))
}

fn vertical_boundary_directions(
    cells: &BTreeMap<(u16, u16), BorderCellState>,
    x: u16,
    y: u16,
    total_rows: u16,
) -> (bool, bool) {
    let up = y > 0
        && cells
            .get(&(x, y - 1))
            .is_some_and(BorderCellState::has_vertical);
    let down = y.saturating_add(1) < total_rows
        && cells
            .get(&(x, y + 1))
            .is_some_and(BorderCellState::has_vertical);
    (up, down)
}

fn vertical_neighbour_panes(
    cells: &BTreeMap<(u16, u16), BorderCellState>,
    x: u16,
    y: u16,
    total_rows: u16,
) -> Vec<u32> {
    let mut panes = Vec::new();
    for neighbour_y in [
        y.checked_sub(1),
        Some(y),
        (y.saturating_add(1) < total_rows).then_some(y + 1),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(neighbour) = cells.get(&(x, neighbour_y)) {
            for pane_index in &neighbour.adjacent_panes {
                if !panes.contains(pane_index) {
                    panes.push(*pane_index);
                }
            }
        }
    }
    panes
}

fn cell_state_mut(
    cells: &mut BTreeMap<(u16, u16), BorderCellState>,
    x: u16,
    y: u16,
) -> &mut BorderCellState {
    cells.entry((x, y)).or_default()
}

fn shared_vertical_boundary(
    first_index: u32,
    first: PaneGeometry,
    second_index: u32,
    second: PaneGeometry,
) -> Option<VerticalBoundarySegment> {
    let (left, right) = if first.x() <= second.x() {
        ((first_index, first), (second_index, second))
    } else {
        ((second_index, second), (first_index, first))
    };
    if left.1.cols() == 0
        || right.1.cols() == 0
        || left.1.rows() == 0
        || right.1.rows() == 0
        || left.1.x().saturating_add(left.1.cols()).saturating_add(1) != right.1.x()
    {
        return None;
    }

    let start = left.1.y().max(right.1.y());
    let end_exclusive = left
        .1
        .y()
        .saturating_add(left.1.rows())
        .min(right.1.y().saturating_add(right.1.rows()));
    if start >= end_exclusive {
        return None;
    }

    Some(VerticalBoundarySegment {
        x: left.1.x().saturating_add(left.1.cols()),
        start,
        end: end_exclusive.saturating_sub(1),
        first_index: left.0,
        second_index: right.0,
    })
}

fn shared_horizontal_boundary(
    first_index: u32,
    first: PaneGeometry,
    second_index: u32,
    second: PaneGeometry,
) -> Option<HorizontalBoundarySegment> {
    let (top, bottom) = if first.y() <= second.y() {
        ((first_index, first), (second_index, second))
    } else {
        ((second_index, second), (first_index, first))
    };
    if top.1.cols() == 0
        || bottom.1.cols() == 0
        || top.1.rows() == 0
        || bottom.1.rows() == 0
        || top.1.y().saturating_add(top.1.rows()).saturating_add(1) != bottom.1.y()
    {
        return None;
    }

    let start = top.1.x().max(bottom.1.x());
    let end_exclusive = top
        .1
        .x()
        .saturating_add(top.1.cols())
        .min(bottom.1.x().saturating_add(bottom.1.cols()));
    if start >= end_exclusive {
        return None;
    }

    Some(HorizontalBoundarySegment {
        y: top.1.y().saturating_add(top.1.rows()),
        start,
        end: end_exclusive.saturating_sub(1),
        first_index: top.0,
        second_index: bottom.0,
    })
}

fn normalize_vertical_segments(
    mut segments: Vec<VerticalBoundarySegment>,
) -> Vec<VerticalBoundarySegment> {
    segments.sort_by_key(|segment| (segment.x, segment.start, segment.end));
    let mut merged: Vec<VerticalBoundarySegment> = Vec::with_capacity(segments.len());

    for segment in segments {
        if let Some(previous) = merged.last_mut() {
            if previous.x == segment.x
                && previous.first_index == segment.first_index
                && previous.second_index == segment.second_index
                && segment.start <= previous.end.saturating_add(2)
            {
                previous.end = previous.end.max(segment.end);
                continue;
            }
        }
        merged.push(segment);
    }

    merged
}

fn normalize_horizontal_segments(
    mut segments: Vec<HorizontalBoundarySegment>,
) -> Vec<HorizontalBoundarySegment> {
    segments.sort_by_key(|segment| (segment.y, segment.start, segment.end));
    let mut merged: Vec<HorizontalBoundarySegment> = Vec::with_capacity(segments.len());

    for segment in segments {
        if let Some(previous) = merged.last_mut() {
            if previous.y == segment.y
                && previous.first_index == segment.first_index
                && previous.second_index == segment.second_index
                && segment.start <= previous.end.saturating_add(2)
            {
                previous.end = previous.end.max(segment.end);
                continue;
            }
        }
        merged.push(segment);
    }

    merged
}

pub(in crate::renderer) fn content_pane_geometry(pane: &Pane, content_rows: u16) -> PaneGeometry {
    clip_pane_geometry(pane.geometry(), content_rows)
}

fn border_cell_owner_and_activity(
    x: u16,
    y: u16,
    adjacent_panes: &[u32],
    pane_geometries: &[(u32, PaneGeometry)],
    active_pane_index: u32,
    indicators_colour: bool,
) -> (Option<u32>, bool) {
    if adjacent_panes.is_empty() {
        return (None, false);
    }

    if indicators_colour && pane_geometries.len() == 2 {
        if let Some((owner, active)) = two_pane_border_owner_and_activity(
            x,
            y,
            adjacent_panes,
            pane_geometries,
            active_pane_index,
        ) {
            return (Some(owner), active);
        }
    }

    if adjacent_panes.contains(&active_pane_index) {
        return (Some(active_pane_index), true);
    }

    (
        pane_geometries
            .iter()
            .find_map(|(pane_index, _)| adjacent_panes.contains(pane_index).then_some(*pane_index)),
        false,
    )
}

fn two_pane_border_owner_and_activity(
    x: u16,
    y: u16,
    adjacent_panes: &[u32],
    pane_geometries: &[(u32, PaneGeometry)],
    active_pane_index: u32,
) -> Option<(u32, bool)> {
    let mut panes = pane_geometries
        .iter()
        .filter(|(pane_index, _)| adjacent_panes.contains(pane_index))
        .copied()
        .collect::<Vec<_>>();
    if panes.len() != 2 {
        return None;
    }

    panes.sort_by_key(|(_, geometry)| (geometry.x(), geometry.y()));
    let (left_index, left) = panes[0];
    let (right_index, right) = panes[1];
    if left.x().saturating_add(left.cols()).saturating_add(1) == right.x() {
        let start = left.y().max(right.y());
        let end = left
            .y()
            .saturating_add(left.rows())
            .min(right.y().saturating_add(right.rows()))
            .saturating_sub(1);
        if x == left.x().saturating_add(left.cols()) && y >= start && y <= end {
            let owner = if y <= left.y().saturating_add(left.rows() / 2) {
                left_index
            } else {
                right_index
            };
            return Some((owner, owner == active_pane_index));
        }
    }

    panes.sort_by_key(|(_, geometry)| (geometry.y(), geometry.x()));
    let (top_index, top) = panes[0];
    let (bottom_index, bottom) = panes[1];
    if top.y().saturating_add(top.rows()).saturating_add(1) == bottom.y() {
        let start = top.x().max(bottom.x());
        let end = top
            .x()
            .saturating_add(top.cols())
            .min(bottom.x().saturating_add(bottom.cols()))
            .saturating_sub(1);
        if y == top.y().saturating_add(top.rows()) && x >= start && x <= end {
            let owner = if x <= top.x().saturating_add(top.cols() / 2) {
                top_index
            } else {
                bottom_index
            };
            return Some((owner, owner == active_pane_index));
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BorderLayoutCell {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) glyph: char,
    pub(super) owner_pane_index: Option<u32>,
    pub(super) active: bool,
}
