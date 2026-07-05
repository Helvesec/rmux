use rmux_core::PaneId;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusRangeType {
    None,
    Left,
    Right,
    Pane(PaneId),
    Window(u32),
    Session(u32),
    User,
    Control(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusRange {
    pub(crate) line: u16,
    pub(crate) x: std::ops::RangeInclusive<u16>,
    pub(crate) kind: StatusRangeType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusLineLayout {
    pub(crate) ranges: Vec<StatusRange>,
}
