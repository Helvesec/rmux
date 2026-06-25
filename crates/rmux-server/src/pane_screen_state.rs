#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneScreenState {
    pub(crate) mode: u32,
    pub(crate) alternate_on: bool,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) cursor_position: (u32, u32),
    pub(crate) cursor_style: u32,
}
