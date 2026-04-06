use rmux_proto::{PaneTarget, SessionName};

/// Stable internal pane identity used for server-side ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(u32);

impl PaneId {
    /// Creates a stable pane identity.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw stable pane identity.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// A pane rectangle within terminal coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGeometry {
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
}

impl PaneGeometry {
    /// Creates a pane rectangle at the given position and size.
    #[must_use]
    pub const fn new(x: u16, y: u16, cols: u16, rows: u16) -> Self {
        Self { x, y, cols, rows }
    }

    /// Returns the left-most column of the pane.
    #[must_use]
    pub const fn x(&self) -> u16 {
        self.x
    }

    /// Returns the top-most row of the pane.
    #[must_use]
    pub const fn y(&self) -> u16 {
        self.y
    }

    /// Returns the pane width in columns.
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }

    /// Returns the pane height in rows.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }
}

/// Pure in-memory pane state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    id: PaneId,
    index: u32,
    geometry: PaneGeometry,
    active_point: u64,
}

impl Pane {
    /// Returns the pane's stable internal identity.
    #[must_use]
    pub const fn id(&self) -> PaneId {
        self.id
    }

    /// Returns the stable pane index used by detached targets.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Returns the pane's current geometry.
    #[must_use]
    pub const fn geometry(&self) -> PaneGeometry {
        self.geometry
    }

    pub(crate) const fn active_point(&self) -> u64 {
        self.active_point
    }

    /// Builds an exact pane target for this pane in the given session.
    #[must_use]
    pub fn target(&self, session_name: &SessionName) -> PaneTarget {
        PaneTarget::new(session_name.clone(), self.index)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn new(index: u32, geometry: PaneGeometry) -> Self {
        Self::new_with_id(PaneId::new(index), index, geometry)
    }

    pub(crate) const fn new_with_id(id: PaneId, index: u32, geometry: PaneGeometry) -> Self {
        Self {
            id,
            index,
            geometry,
            active_point: 0,
        }
    }

    pub(crate) fn set_geometry(&mut self, geometry: PaneGeometry) {
        self.geometry = geometry;
    }

    pub(crate) fn set_index(&mut self, index: u32) {
        self.index = index;
    }

    pub(crate) fn set_active_point(&mut self, active_point: u64) {
        self.active_point = active_point;
    }
}

#[cfg(test)]
mod tests {
    use super::{Pane, PaneGeometry, PaneId};
    use rmux_proto::SessionName;

    #[test]
    fn geometry_accessors_match_constructor_values() {
        let geometry = PaneGeometry::new(4, 7, 80, 24);

        assert_eq!(geometry.x(), 4);
        assert_eq!(geometry.y(), 7);
        assert_eq!(geometry.cols(), 80);
        assert_eq!(geometry.rows(), 24);
    }

    #[test]
    fn pane_target_uses_session_name_and_index() {
        let pane = Pane::new(3, PaneGeometry::new(0, 0, 10, 5));
        let session_name = SessionName::new("alpha").expect("valid session name");

        assert_eq!(pane.target(&session_name).to_string(), "alpha:0.3");
    }

    #[test]
    fn pane_id_is_stable_and_independent_from_display_index() {
        let pane = Pane::new_with_id(PaneId::new(9), 3, PaneGeometry::new(0, 0, 10, 5));

        assert_eq!(pane.id(), PaneId::new(9));
        assert_eq!(pane.index(), 3);
    }

    #[test]
    fn set_geometry_replaces_the_existing_rectangle() {
        let mut pane = Pane::new(0, PaneGeometry::new(0, 0, 10, 5));
        let replacement = PaneGeometry::new(12, 1, 34, 50);

        pane.set_geometry(replacement);

        assert_eq!(pane.geometry(), replacement);
    }
}
