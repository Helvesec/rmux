//! Downstream named-layout migration fixture with only the SDK as a dependency.

use std::error::Error;

use rmux_sdk::{LayoutName, Window};

/// The SDK import and window operation retain their pre-cutoff spelling.
pub async fn select_named_layout(window: &Window, name: &str) -> Result<(), Box<dyn Error>> {
    let layout: LayoutName = name.parse()?;
    window.select_layout(layout).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::type_name;
    use std::str::FromStr;

    #[test]
    fn layout_is_owned_by_sdk() {
        let owner = type_name::<LayoutName>();
        assert!(owner.starts_with("rmux_sdk::"), "layout owner: {owner}");
    }

    #[test]
    fn parse_error_is_owned_by_sdk() {
        let owner = type_name::<<LayoutName as FromStr>::Err>();
        assert!(
            owner.starts_with("rmux_sdk::"),
            "parse error owner: {owner}"
        );
    }
}
