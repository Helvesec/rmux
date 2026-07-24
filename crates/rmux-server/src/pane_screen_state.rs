#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneScreenState {
    pub(crate) mode: u32,
    pub(crate) alternate_on: bool,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) cursor_position: (u32, u32),
    pub(crate) cursor_style: u32,
}

impl PaneScreenState {
    pub(crate) fn from_screen(screen: &rmux_core::Screen) -> Self {
        let mut mode = screen.mode();
        if mode & rmux_core::input::mode::MODE_KEYS_EXTENDED_2 != 0 {
            mode |= rmux_core::input::mode::MODE_KEYS_EXTENDED;
        }
        Self {
            mode,
            alternate_on: screen.is_alternate(),
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            cursor_position: screen.cursor_position(),
            cursor_style: screen.cursor_style(),
        }
    }
}
