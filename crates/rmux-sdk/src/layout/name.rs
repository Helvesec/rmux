use std::{error::Error, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// An SDK-owned named layout accepted by [`crate::Window::select_layout`].
///
/// Display and parsing use exact lowercase hyphenated names, such as `tiled`
/// and `main-vertical`. Parsing accepts no abbreviations or whitespace.
/// Serde retains the variant names (for example, `"Tiled"` in JSON) and the
/// original variant order for binary serializers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutName {
    /// The `main-vertical` layout.
    MainVertical,
    /// The `main-horizontal` layout with a top main pane.
    MainHorizontal,
    /// The `even-horizontal` left-to-right layout.
    EvenHorizontal,
    /// The `even-vertical` top-to-bottom layout.
    EvenVertical,
    /// The `tiled` grid layout.
    Tiled,
    /// The `main-horizontal-mirrored` layout.
    MainHorizontalMirrored,
    /// The `main-vertical-mirrored` layout.
    MainVerticalMirrored,
}

impl LayoutName {
    pub(crate) const fn to_proto(self) -> rmux_proto::LayoutName {
        match self {
            Self::MainVertical => rmux_proto::LayoutName::MainVertical,
            Self::MainHorizontal => rmux_proto::LayoutName::MainHorizontal,
            Self::EvenHorizontal => rmux_proto::LayoutName::EvenHorizontal,
            Self::EvenVertical => rmux_proto::LayoutName::EvenVertical,
            Self::Tiled => rmux_proto::LayoutName::Tiled,
            Self::MainHorizontalMirrored => rmux_proto::LayoutName::MainHorizontalMirrored,
            Self::MainVerticalMirrored => rmux_proto::LayoutName::MainVerticalMirrored,
        }
    }
}

impl fmt::Display for LayoutName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MainVertical => "main-vertical",
            Self::MainHorizontal => "main-horizontal",
            Self::EvenHorizontal => "even-horizontal",
            Self::EvenVertical => "even-vertical",
            Self::Tiled => "tiled",
            Self::MainHorizontalMirrored => "main-horizontal-mirrored",
            Self::MainVerticalMirrored => "main-vertical-mirrored",
        })
    }
}

impl FromStr for LayoutName {
    type Err = ParseLayoutNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "main-vertical" => Ok(Self::MainVertical),
            "main-horizontal" => Ok(Self::MainHorizontal),
            "even-horizontal" => Ok(Self::EvenHorizontal),
            "even-vertical" => Ok(Self::EvenVertical),
            "tiled" => Ok(Self::Tiled),
            "main-horizontal-mirrored" => Ok(Self::MainHorizontalMirrored),
            "main-vertical-mirrored" => Ok(Self::MainVerticalMirrored),
            _ => Err(ParseLayoutNameError {
                value: value.to_owned(),
            }),
        }
    }
}

/// An unrecognized named layout passed to [`LayoutName::from_str`].
///
/// This SDK-owned error retains the previous display diagnostic, including
/// its `server error:` prefix, although parsing happens locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLayoutNameError {
    value: String,
}

impl fmt::Display for ParseLayoutNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "server error: unknown layout: {}", self.value)
    }
}

impl Error for ParseLayoutNameError {}
