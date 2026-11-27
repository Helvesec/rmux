//! Downstream web theme consumer with only the SDK as a dependency.

use rmux_sdk::{Pane, Session, WebShareBuilder, WebTerminalPalette, WebTerminalTheme};

/// Public theme setters and aliases remain usable in const functions.
pub const fn choose_theme(builder: WebShareBuilder<'_>) -> WebShareBuilder<'_> {
    builder
        .theme(WebTerminalTheme::User)
        .light_theme()
        .dark_theme()
        .user_theme()
        .terminal_theme(WebTerminalTheme::Dark)
}

/// The theme URL spelling remains available at compile time.
pub const USER_THEME: &str = WebTerminalTheme::User.as_url_value();

/// Palette construction retains all public fields and their types.
pub fn palette() -> WebTerminalPalette {
    WebTerminalPalette {
        foreground: "#fefefe".to_owned(),
        background: "#010101".to_owned(),
        cursor: "#ababab".to_owned(),
        ansi: std::array::from_fn(|index| format!("#{index:06x}")),
    }
}

/// Both existing share entry paths accept SDK-owned values.
pub async fn share(session: &Session, pane: &Pane) -> rmux_sdk::Result<()> {
    let _session_share = choose_theme(session.share())
        .terminal_palette(palette())
        .await?;
    let _pane_share = choose_theme(pane.share())
        .terminal_palette(palette())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::type_name;

    #[test]
    fn theme_is_owned_by_sdk() {
        let owner = type_name::<WebTerminalTheme>();
        assert!(owner.starts_with("rmux_sdk::"), "theme owner: {owner}");
    }

    #[test]
    fn palette_is_owned_by_sdk() {
        let owner = type_name::<WebTerminalPalette>();
        assert!(owner.starts_with("rmux_sdk::"), "palette owner: {owner}");
    }
}
