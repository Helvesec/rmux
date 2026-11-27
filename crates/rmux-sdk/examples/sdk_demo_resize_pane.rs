//! Demonstrates `Pane::resize()`.
//!
//! The visible-state setup splits the pane first so the resize moves
//! a divider — a 50/50 split wouldn't look any different from the
//! Split-vertically scenario. After split + resize the left pane
//! occupies ~70 % of the width; the captured snapshot is 56 × 24, so
//! the docs renderer paints the divider at column 56 and the synth
//! right pane is the narrow remainder.

use rmux_sdk::{Result, SplitDirection};

mod sdk_demo_helpers;

#[tokio::main]
async fn main() -> Result<()> {
    let (_rmux, session) = sdk_demo_helpers::demo_session("resize").await?;
    // 2-pane layout so the resize is visible.
    let left = session.pane(0, 0);
    let _right = left.split(SplitDirection::Right).await?;

    // example:start
    use rmux_sdk::TerminalSizeSpec;
    let pane = session.pane(0, 0);
    pane.resize(TerminalSizeSpec::new(56, 24)).await?;
    // example:end

    sdk_demo_helpers::paint_idle_prompt(&session).await?;

    Ok(())
}
