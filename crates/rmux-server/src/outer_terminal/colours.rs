pub(super) use rmux_core::colour_to_rgb;
use rmux_core::Colour;

pub(super) fn colour_to_rgb_string(colour: Colour) -> Option<String> {
    let (red, green, blue) = colour_to_rgb(colour)?;
    Some(format!("rgb:{red:02x}/{green:02x}/{blue:02x}"))
}
