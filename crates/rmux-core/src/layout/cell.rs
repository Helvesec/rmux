use std::fmt::Write as _;

use crate::PaneId;

use super::{LayoutDirection, PANE_BORDER_CELLS, PANE_MINIMUM};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutGeometry {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) x: u32,
    pub(super) y: u32,
}

impl LayoutGeometry {
    #[must_use]
    pub(super) const fn new(width: u32, height: u32, x: u32, y: u32) -> Self {
        Self {
            width,
            height,
            x,
            y,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LayoutKind {
    Pane(PaneId),
    Split(LayoutDirection, Vec<LayoutCell>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LayoutCell {
    pub(super) geometry: LayoutGeometry,
    pub(super) kind: LayoutKind,
}

impl LayoutCell {
    #[must_use]
    pub(super) const fn pane(id: PaneId, geometry: LayoutGeometry) -> Self {
        Self {
            geometry,
            kind: LayoutKind::Pane(id),
        }
    }

    #[must_use]
    pub(super) fn split(
        direction: LayoutDirection,
        geometry: LayoutGeometry,
        children: Vec<Self>,
    ) -> Self {
        Self {
            geometry,
            kind: LayoutKind::Split(direction, children),
        }
    }

    #[must_use]
    pub(super) fn leaf_count(&self) -> usize {
        match &self.kind {
            LayoutKind::Pane(_) => 1,
            LayoutKind::Split(_, children) => children.iter().map(Self::leaf_count).sum(),
        }
    }

    #[must_use]
    pub(super) fn minimum_size(&self) -> (u32, u32) {
        let LayoutKind::Split(direction, children) = &self.kind else {
            return (PANE_MINIMUM, PANE_MINIMUM);
        };

        let border_count = u32::try_from(children.len().saturating_sub(1)).unwrap_or(u32::MAX);
        let borders = PANE_BORDER_CELLS.saturating_mul(border_count);
        let mut summed_axis = borders;
        let mut widest = PANE_MINIMUM;
        let mut tallest = PANE_MINIMUM;

        for child in children {
            let (width, height) = child.minimum_size();
            match direction {
                LayoutDirection::LeftRight => {
                    summed_axis = summed_axis.saturating_add(width);
                    tallest = tallest.max(height);
                }
                LayoutDirection::TopBottom => {
                    summed_axis = summed_axis.saturating_add(height);
                    widest = widest.max(width);
                }
            }
        }

        match direction {
            LayoutDirection::LeftRight => (summed_axis.max(PANE_MINIMUM), tallest),
            LayoutDirection::TopBottom => (widest, summed_axis.max(PANE_MINIMUM)),
        }
    }

    pub(super) fn visit_leaves(&self, visit: &mut impl FnMut(PaneId, LayoutGeometry)) {
        match &self.kind {
            LayoutKind::Pane(id) => visit(*id, self.geometry),
            LayoutKind::Split(_, children) => {
                for child in children {
                    child.visit_leaves(visit);
                }
            }
        }
    }

    pub(super) fn map_ids(&mut self, map: &mut impl FnMut(PaneId) -> PaneId) {
        match &mut self.kind {
            LayoutKind::Pane(id) => *id = map(*id),
            LayoutKind::Split(_, children) => {
                for child in children {
                    child.map_ids(map);
                }
            }
        }
    }

    pub(super) fn leaf_path(&self, id: PaneId) -> Option<Vec<usize>> {
        match &self.kind {
            LayoutKind::Pane(leaf_id) => (*leaf_id == id).then(Vec::new),
            LayoutKind::Split(_, children) => {
                children.iter().enumerate().find_map(|(index, child)| {
                    let mut path = child.leaf_path(id)?;
                    path.insert(0, index);
                    Some(path)
                })
            }
        }
    }

    #[must_use]
    pub(super) fn dump_body(&self) -> String {
        let mut body = format!(
            "{}x{},{},{}",
            self.geometry.width, self.geometry.height, self.geometry.x, self.geometry.y
        );
        match &self.kind {
            LayoutKind::Pane(id) => {
                let _ = write!(&mut body, ",{}", id.as_u32());
            }
            LayoutKind::Split(direction, children) => {
                let (open, close) = match direction {
                    LayoutDirection::LeftRight => ('{', '}'),
                    LayoutDirection::TopBottom => ('[', ']'),
                };
                body.push(open);
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        body.push(',');
                    }
                    body.push_str(&child.dump_body());
                }
                body.push(close);
            }
        }
        body
    }

    #[must_use]
    pub(super) fn import_dimensions_are_safe(&self) -> bool {
        let limit = i32::MAX as u32;
        if self.geometry.width > limit || self.geometry.height > limit {
            return false;
        }
        let LayoutKind::Split(direction, children) = &self.kind else {
            return true;
        };
        if children.is_empty() {
            return false;
        }
        let mut total = 0_u32;
        for (index, child) in children.iter().enumerate() {
            if !child.import_dimensions_are_safe() {
                return false;
            }
            let size = match direction {
                LayoutDirection::LeftRight => child.geometry.width,
                LayoutDirection::TopBottom => child.geometry.height,
            };
            let border = if index == 0 { 0 } else { PANE_BORDER_CELLS };
            let Some(next) = total
                .checked_add(border)
                .and_then(|sum| sum.checked_add(size))
            else {
                return false;
            };
            if next > limit {
                return false;
            }
            total = next;
        }
        true
    }

    #[must_use]
    pub(super) fn check(&self) -> bool {
        match &self.kind {
            LayoutKind::Pane(_) => true,
            LayoutKind::Split(direction, children) => {
                if children.is_empty() {
                    return false;
                }

                let mut total = 0_u32;
                for child in children {
                    if !child.check() {
                        return false;
                    }
                    match direction {
                        LayoutDirection::LeftRight => {
                            if child.geometry.height != self.geometry.height {
                                return false;
                            }
                            total = total.saturating_add(child.geometry.width + PANE_BORDER_CELLS);
                        }
                        LayoutDirection::TopBottom => {
                            if child.geometry.width != self.geometry.width {
                                return false;
                            }
                            total = total.saturating_add(child.geometry.height + PANE_BORDER_CELLS);
                        }
                    }
                }

                match direction {
                    LayoutDirection::LeftRight => {
                        total.saturating_sub(PANE_BORDER_CELLS) == self.geometry.width
                    }
                    LayoutDirection::TopBottom => {
                        total.saturating_sub(PANE_BORDER_CELLS) == self.geometry.height
                    }
                }
            }
        }
    }

    pub(super) fn fix_offsets(&mut self, x: u32, y: u32) {
        self.geometry.x = x;
        self.geometry.y = y;
        match &mut self.kind {
            LayoutKind::Pane(_) => {}
            LayoutKind::Split(direction, children) => {
                let mut cursor = 0_u32;
                for child in children {
                    match direction {
                        LayoutDirection::LeftRight => {
                            child.fix_offsets(x + cursor, y);
                            cursor = cursor
                                .saturating_add(child.geometry.width)
                                .saturating_add(PANE_BORDER_CELLS);
                        }
                        LayoutDirection::TopBottom => {
                            child.fix_offsets(x, y + cursor);
                            cursor = cursor
                                .saturating_add(child.geometry.height)
                                .saturating_add(PANE_BORDER_CELLS);
                        }
                    }
                }
            }
        }
    }

    #[must_use]
    pub(super) fn resize_check(&self, direction: LayoutDirection) -> u32 {
        match &self.kind {
            LayoutKind::Pane(_) => {
                let available = match direction {
                    LayoutDirection::LeftRight => self.geometry.width,
                    LayoutDirection::TopBottom => self.geometry.height,
                };
                available.saturating_sub(PANE_MINIMUM)
            }
            LayoutKind::Split(split_direction, children) => {
                if *split_direction == direction {
                    children
                        .iter()
                        .map(|child| child.resize_check(direction))
                        .sum()
                } else {
                    children
                        .iter()
                        .map(|child| child.resize_check(direction))
                        .min()
                        .unwrap_or(0)
                }
            }
        }
    }

    pub(super) fn resize_adjust(&mut self, direction: LayoutDirection, change: i32) {
        if change == 0 {
            return;
        }

        match direction {
            LayoutDirection::LeftRight => {
                self.geometry.width = self.geometry.width.saturating_add_signed(change);
            }
            LayoutDirection::TopBottom => {
                self.geometry.height = self.geometry.height.saturating_add_signed(change);
            }
        }

        let LayoutKind::Split(split_direction, children) = &mut self.kind else {
            return;
        };

        if *split_direction != direction {
            for child in children {
                child.resize_adjust(direction, change);
            }
            return;
        }

        let mut remaining = change;
        while remaining != 0 {
            let mut progressed = false;
            for child in children.iter_mut() {
                if remaining == 0 {
                    break;
                }
                if remaining > 0 {
                    child.resize_adjust(direction, 1);
                    remaining -= 1;
                    progressed = true;
                } else if child.resize_check(direction) > 0 {
                    child.resize_adjust(direction, -1);
                    remaining += 1;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    pub(super) fn spread(&mut self) -> bool {
        let LayoutKind::Split(direction, children) = &mut self.kind else {
            return false;
        };
        if children.len() <= 1 {
            return false;
        }

        let size = match direction {
            LayoutDirection::LeftRight => self.geometry.width,
            LayoutDirection::TopBottom => self.geometry.height,
        };
        if size < (children.len() as u32).saturating_sub(1) {
            return false;
        }

        let each =
            size.saturating_sub((children.len() as u32).saturating_sub(1)) / children.len() as u32;
        if each == 0 {
            return false;
        }

        let mut remainder = size
            .saturating_sub((children.len() as u32) * (each + PANE_BORDER_CELLS))
            .saturating_add(PANE_BORDER_CELLS);
        let mut changed = false;

        for child in children {
            let mut target = each;
            if remainder > 0 {
                target += 1;
                remainder -= 1;
            }
            let current = match direction {
                LayoutDirection::LeftRight => child.geometry.width,
                LayoutDirection::TopBottom => child.geometry.height,
            };
            let delta = target as i32 - current as i32;
            if delta != 0 {
                child.resize_adjust(*direction, delta);
                changed = true;
            }
        }

        changed
    }

    pub(super) fn fix_root_size_if_needed(&mut self) -> bool {
        let LayoutKind::Split(direction, children) = &self.kind else {
            return true;
        };
        let Some(first) = children.first() else {
            return false;
        };
        let mut total = 0_u32;
        for (index, child) in children.iter().enumerate() {
            let size = match direction {
                LayoutDirection::LeftRight => child.geometry.width,
                LayoutDirection::TopBottom => child.geometry.height,
            };
            let border = if index == 0 { 0 } else { PANE_BORDER_CELLS };
            let Some(next) = total
                .checked_add(border)
                .and_then(|sum| sum.checked_add(size))
            else {
                return false;
            };
            total = next;
        }
        let (width, height) = match direction {
            LayoutDirection::LeftRight => (total, first.geometry.height),
            LayoutDirection::TopBottom => (first.geometry.width, total),
        };
        self.geometry.width = width;
        self.geometry.height = height;
        true
    }
}
