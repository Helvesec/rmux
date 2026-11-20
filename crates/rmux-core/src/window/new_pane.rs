//! Direct insertion into the canonical floating stack, without a tiled split.

use super::Window;
use crate::{FloatingPanePlacement, FloatingPaneRect, FloatingPaneRequest, Pane, PaneId};
use rmux_proto::RmuxError;

/// The steps of the cascade, as the source walks them (`layout.c:1399-1417`).
const CASCADE_STEP_X: i32 = 4;
const CASCADE_STEP_Y: i32 = 2;

impl Window {
    /// The one refusal this placement has, whatever value was out of range.
    fn out_of_bounds() -> RmuxError {
        RmuxError::Server("floating size or position out of bounds".to_owned())
    }

    /// Where an omitted axis goes, and what the window remembers afterwards.
    ///
    /// Nothing placed yet starts at the step itself; otherwise the memory walks
    /// one step forward, and starts again when what it remembers has already
    /// passed the window's own edge. The test is on the remembered value, not
    /// on the one being computed, which is what the source does.
    fn cascade_axis(last: i32, edge: i32, step: i32) -> i32 {
        if last == 0 || last > edge {
            step
        } else {
            last + step
        }
    }

    pub(crate) fn insert_floating_pane(
        &mut self,
        pane_id: PaneId,
        request: FloatingPaneRequest,
        detached: bool,
    ) -> Result<u32, RmuxError> {
        let width = i32::from(self.size.cols);
        let height = i32::from(self.size.rows);
        let (requested_x, requested_y) = request.position();
        let (cols, rows) = request.size();
        // Everything the caller actually gave is read first, and the first
        // value out of range returns with nothing walked and nothing placed:
        // that is the order of the source, which expands each argument it was
        // given and returns on its `cause` (`layout.c:1374-1396`) before the
        // cascade block (1399). Measured on the pin in a fresh 80x24 window
        // (`fopen1/r2/oracle-r2.json`, cells a01 to a04): `-X 81`,
        // `-Y 25`, `-x 80` and `-y 24` are refused and the opening that follows
        // still takes the first step.
        if cols >= width || rows >= height {
            return Err(Self::out_of_bounds());
        }
        if requested_x.is_some_and(|x| x < -cols || x > width)
            || requested_y.is_some_and(|y| y < -rows || y > height)
        {
            return Err(Self::out_of_bounds());
        }
        // Each axis is resolved on its own: an explicit one neither consumes the
        // other nor moves its own memory, exactly as the source only assigns
        // inside the branch that had nothing. The memory takes its step before
        // the rectangle is checked, which is the order the source places in.
        let x = requested_x.unwrap_or_else(|| {
            let placed = Self::cascade_axis(self.last_new_pane_x, width, CASCADE_STEP_X);
            self.last_new_pane_x = placed;
            placed
        });
        let y = requested_y.unwrap_or_else(|| {
            let placed = Self::cascade_axis(self.last_new_pane_y, height, CASCADE_STEP_Y);
            self.last_new_pane_y = placed;
            placed
        });
        let rect = FloatingPaneRect::new(x, y, cols, rows)?;
        // What is left for a coordinate this window computed itself: the
        // divergence already declared, where the source places a float past the
        // window's own edge and this crate refuses to. The step is taken all the
        // same, which is what lets the cascade start again.
        if rect.x() < -rect.cols()
            || rect.x() > width
            || rect.y() < -rect.rows()
            || rect.y() > height
        {
            return Err(Self::out_of_bounds());
        }
        self.reconcile_borrowed_pane_owners();
        let active = self.active_pane().map(Pane::id);
        let last = self.last_pane.and_then(|index| self.pane_id(index));
        let index = self.allocate_pane_index();
        self.floating_stack
            .push_frontmost(FloatingPanePlacement::new(pane_id, rect))?;
        self.panes
            .push(Pane::new_with_id(pane_id, index, rect.pane_geometry()));
        self.commit_split_renumber(pane_id, active, last, detached);
        Ok(self
            .pane_index_for_id(pane_id)
            .expect("inserted floating pane"))
    }
}
