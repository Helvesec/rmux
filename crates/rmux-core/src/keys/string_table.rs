use super::{
    make_mouse_key, shift_type, KeyCode, KeyCodeType, MouseEventType, MouseLocation, KEYC_BSPACE,
    KEYC_BTAB, KEYC_CURSOR, KEYC_DC, KEYC_DOWN, KEYC_DRAGGING, KEYC_END, KEYC_F1, KEYC_F10,
    KEYC_F11, KEYC_F12, KEYC_F2, KEYC_F3, KEYC_F4, KEYC_F5, KEYC_F6, KEYC_F7, KEYC_F8, KEYC_F9,
    KEYC_HOME, KEYC_IC, KEYC_IMPLIED_META, KEYC_KEYPAD, KEYC_KP_EIGHT, KEYC_KP_ENTER, KEYC_KP_FIVE,
    KEYC_KP_FOUR, KEYC_KP_MINUS, KEYC_KP_NINE, KEYC_KP_ONE, KEYC_KP_PERIOD, KEYC_KP_PLUS,
    KEYC_KP_SEVEN, KEYC_KP_SIX, KEYC_KP_SLASH, KEYC_KP_STAR, KEYC_KP_THREE, KEYC_KP_TWO,
    KEYC_KP_ZERO, KEYC_LEFT, KEYC_MASK_KEY, KEYC_MASK_TYPE, KEYC_NPAGE, KEYC_NUSER, KEYC_PPAGE,
    KEYC_RIGHT, KEYC_UP, KEYC_USER,
};

#[derive(Clone, Copy)]
pub(super) struct KeyStringEntry {
    pub(super) string: &'static str,
    key: KeyCode,
}

const KEY_STRING_TABLE: &[KeyStringEntry] = &[
    KeyStringEntry {
        string: "Dragging",
        key: KEYC_DRAGGING,
    },
    KeyStringEntry {
        string: "F1",
        key: KEYC_F1 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F2",
        key: KEYC_F2 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F3",
        key: KEYC_F3 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F4",
        key: KEYC_F4 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F5",
        key: KEYC_F5 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F6",
        key: KEYC_F6 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F7",
        key: KEYC_F7 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F8",
        key: KEYC_F8 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F9",
        key: KEYC_F9 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F10",
        key: KEYC_F10 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F11",
        key: KEYC_F11 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "F12",
        key: KEYC_F12 | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "IC",
        key: KEYC_IC | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "Insert",
        key: KEYC_IC | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "DC",
        key: KEYC_DC | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "Delete",
        key: KEYC_DC | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "Home",
        key: KEYC_HOME | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "End",
        key: KEYC_END | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "NPage",
        key: KEYC_NPAGE | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "PageDown",
        key: KEYC_NPAGE | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "PgDn",
        key: KEYC_NPAGE | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "PPage",
        key: KEYC_PPAGE | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "PageUp",
        key: KEYC_PPAGE | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "PgUp",
        key: KEYC_PPAGE | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "BTab",
        key: KEYC_BTAB,
    },
    KeyStringEntry {
        string: "Space",
        key: b' ' as u64,
    },
    KeyStringEntry {
        string: "BSpace",
        key: KEYC_BSPACE,
    },
    KeyStringEntry {
        string: "[NUL]",
        key: 0,
    },
    KeyStringEntry {
        string: "[SOH]",
        key: 1,
    },
    KeyStringEntry {
        string: "[STX]",
        key: 2,
    },
    KeyStringEntry {
        string: "[ETX]",
        key: 3,
    },
    KeyStringEntry {
        string: "[EOT]",
        key: 4,
    },
    KeyStringEntry {
        string: "[ENQ]",
        key: 5,
    },
    KeyStringEntry {
        string: "[ASC]",
        key: 6,
    },
    KeyStringEntry {
        string: "[BEL]",
        key: 7,
    },
    KeyStringEntry {
        string: "[BS]",
        key: 8,
    },
    KeyStringEntry {
        string: "Tab",
        key: 9,
    },
    KeyStringEntry {
        string: "[LF]",
        key: 10,
    },
    KeyStringEntry {
        string: "[VT]",
        key: 11,
    },
    KeyStringEntry {
        string: "[FF]",
        key: 12,
    },
    KeyStringEntry {
        string: "Enter",
        key: 13,
    },
    KeyStringEntry {
        string: "[SO]",
        key: 14,
    },
    KeyStringEntry {
        string: "[SI]",
        key: 15,
    },
    KeyStringEntry {
        string: "[DLE]",
        key: 16,
    },
    KeyStringEntry {
        string: "[DC1]",
        key: 17,
    },
    KeyStringEntry {
        string: "[DC2]",
        key: 18,
    },
    KeyStringEntry {
        string: "[DC3]",
        key: 19,
    },
    KeyStringEntry {
        string: "[DC4]",
        key: 20,
    },
    KeyStringEntry {
        string: "[NAK]",
        key: 21,
    },
    KeyStringEntry {
        string: "[SYN]",
        key: 22,
    },
    KeyStringEntry {
        string: "[ETB]",
        key: 23,
    },
    KeyStringEntry {
        string: "[CAN]",
        key: 24,
    },
    KeyStringEntry {
        string: "[EM]",
        key: 25,
    },
    KeyStringEntry {
        string: "[SUB]",
        key: 26,
    },
    KeyStringEntry {
        string: "Escape",
        key: 27,
    },
    KeyStringEntry {
        string: "[FS]",
        key: 28,
    },
    KeyStringEntry {
        string: "[GS]",
        key: 29,
    },
    KeyStringEntry {
        string: "[RS]",
        key: 30,
    },
    KeyStringEntry {
        string: "[US]",
        key: 31,
    },
    KeyStringEntry {
        string: "Up",
        key: KEYC_UP | KEYC_CURSOR | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "Down",
        key: KEYC_DOWN | KEYC_CURSOR | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "Left",
        key: KEYC_LEFT | KEYC_CURSOR | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "Right",
        key: KEYC_RIGHT | KEYC_CURSOR | KEYC_IMPLIED_META,
    },
    KeyStringEntry {
        string: "KP/",
        key: KEYC_KP_SLASH | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP*",
        key: KEYC_KP_STAR | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP-",
        key: KEYC_KP_MINUS | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP7",
        key: KEYC_KP_SEVEN | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP8",
        key: KEYC_KP_EIGHT | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP9",
        key: KEYC_KP_NINE | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP+",
        key: KEYC_KP_PLUS | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP4",
        key: KEYC_KP_FOUR | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP5",
        key: KEYC_KP_FIVE | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP6",
        key: KEYC_KP_SIX | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP1",
        key: KEYC_KP_ONE | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP2",
        key: KEYC_KP_TWO | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP3",
        key: KEYC_KP_THREE | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KPEnter",
        key: KEYC_KP_ENTER | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP0",
        key: KEYC_KP_ZERO | KEYC_KEYPAD,
    },
    KeyStringEntry {
        string: "KP.",
        key: KEYC_KP_PERIOD | KEYC_KEYPAD,
    },
];

pub(super) fn key_string_search_table(string: &str) -> Option<KeyCode> {
    for entry in KEY_STRING_TABLE {
        if entry.string.eq_ignore_ascii_case(string) {
            return Some(entry.key);
        }
    }
    // `split_at_checked` keeps a multi-byte character straddling the prefix
    // length from being sliced; such a name cannot be a `User` key.
    if let Some(user) = string
        .split_at_checked(4)
        .and_then(|(head, user)| head.eq_ignore_ascii_case("User").then_some(user))
    {
        let user = user.parse::<u32>().ok()?;
        if user <= KEYC_NUSER {
            return Some(KEYC_USER + KeyCode::from(user));
        }
    }
    mouse_key_from_name(string)
}

pub(super) fn key_string_entry_for_key(key: KeyCode) -> Option<KeyStringEntry> {
    KEY_STRING_TABLE
        .iter()
        .copied()
        .find(|entry| key == (entry.key & KEYC_MASK_KEY))
}

const MOUSE_BUTTONS: &[u64] = &[1, 2, 3, 6, 7, 8, 9, 10, 11];
const MOUSE_LOCATIONS: &[(MouseLocation, &str)] = &[
    (MouseLocation::Pane, "Pane"),
    (MouseLocation::Status, "Status"),
    (MouseLocation::StatusLeft, "StatusLeft"),
    (MouseLocation::StatusRight, "StatusRight"),
    (MouseLocation::StatusDefault, "StatusDefault"),
    (MouseLocation::ScrollbarUp, "ScrollbarUp"),
    (MouseLocation::ScrollbarSlider, "ScrollbarSlider"),
    (MouseLocation::ScrollbarDown, "ScrollbarDown"),
    (MouseLocation::Border, "Border"),
    (MouseLocation::Control0, "Control0"),
    (MouseLocation::Control1, "Control1"),
    (MouseLocation::Control2, "Control2"),
    (MouseLocation::Control3, "Control3"),
    (MouseLocation::Control4, "Control4"),
    (MouseLocation::Control5, "Control5"),
    (MouseLocation::Control6, "Control6"),
    (MouseLocation::Control7, "Control7"),
    (MouseLocation::Control8, "Control8"),
    (MouseLocation::Control9, "Control9"),
];
const MOUSE_EVENTS: &[(MouseEventType, &str)] = &[
    (MouseEventType::MouseDown, "MouseDown"),
    (MouseEventType::MouseUp, "MouseUp"),
    (MouseEventType::MouseDrag, "MouseDrag"),
    (MouseEventType::MouseDragEnd, "MouseDragEnd"),
    (MouseEventType::WheelUp, "WheelUp"),
    (MouseEventType::WheelDown, "WheelDown"),
    (MouseEventType::SecondClick, "SecondClick"),
    (MouseEventType::DoubleClick, "DoubleClick"),
    (MouseEventType::TripleClick, "TripleClick"),
];

pub(super) fn decode_mouse_key(key: KeyCode) -> Option<(MouseEventType, u64, MouseLocation)> {
    let kind = match key & KEYC_MASK_TYPE {
        value if value == shift_type(KeyCodeType::MouseMove) => MouseEventType::MouseMove,
        value if value == shift_type(KeyCodeType::MouseDown) => MouseEventType::MouseDown,
        value if value == shift_type(KeyCodeType::MouseUp) => MouseEventType::MouseUp,
        value if value == shift_type(KeyCodeType::MouseDrag) => MouseEventType::MouseDrag,
        value if value == shift_type(KeyCodeType::MouseDragEnd) => MouseEventType::MouseDragEnd,
        value if value == shift_type(KeyCodeType::WheelDown) => MouseEventType::WheelDown,
        value if value == shift_type(KeyCodeType::WheelUp) => MouseEventType::WheelUp,
        value if value == shift_type(KeyCodeType::SecondClick) => MouseEventType::SecondClick,
        value if value == shift_type(KeyCodeType::DoubleClick) => MouseEventType::DoubleClick,
        value if value == shift_type(KeyCodeType::TripleClick) => MouseEventType::TripleClick,
        _ => return None,
    };
    let button = (key >> 8) & 0xff;
    let location = match key & 0xff {
        0 => MouseLocation::Pane,
        1 => MouseLocation::Status,
        2 => MouseLocation::StatusLeft,
        3 => MouseLocation::StatusRight,
        4 => MouseLocation::StatusDefault,
        5 => MouseLocation::Border,
        6 => MouseLocation::ScrollbarUp,
        7 => MouseLocation::ScrollbarSlider,
        8 => MouseLocation::ScrollbarDown,
        9 => MouseLocation::Control0,
        10 => MouseLocation::Control1,
        11 => MouseLocation::Control2,
        12 => MouseLocation::Control3,
        13 => MouseLocation::Control4,
        14 => MouseLocation::Control5,
        15 => MouseLocation::Control6,
        16 => MouseLocation::Control7,
        17 => MouseLocation::Control8,
        18 => MouseLocation::Control9,
        _ => return None,
    };
    Some((kind, button, location))
}

pub(super) fn mouse_key_name(key: KeyCode) -> Option<String> {
    let (kind, button, location) = decode_mouse_key(key)?;
    if kind == MouseEventType::MouseMove && button == 0 {
        return Some(format!("MouseMove{}", mouse_location_suffix(location)));
    }
    if matches!(kind, MouseEventType::WheelUp | MouseEventType::WheelDown) && button == 0 {
        let prefix = MOUSE_EVENTS
            .iter()
            .find(|(event, _)| *event == kind)
            .map(|(_, prefix)| *prefix)?;
        return Some(format!("{prefix}{}", mouse_location_suffix(location)));
    }
    let prefix = MOUSE_EVENTS
        .iter()
        .find(|(event, _)| *event == kind)
        .map(|(_, prefix)| *prefix)?;
    Some(format!(
        "{prefix}{button}{}",
        mouse_location_suffix(location)
    ))
}

fn mouse_key_from_name(string: &str) -> Option<KeyCode> {
    if let Some(location) = MOUSE_LOCATIONS.iter().find_map(|(location, suffix)| {
        string
            .strip_prefix("MouseMove")
            .filter(|rest| *rest == *suffix)
            .map(|_| *location)
    }) {
        return Some(make_mouse_key(MouseEventType::MouseMove, 0, location));
    }

    for event in [MouseEventType::WheelUp, MouseEventType::WheelDown] {
        let prefix = MOUSE_EVENTS
            .iter()
            .find(|(candidate, _)| *candidate == event)
            .map(|(_, prefix)| *prefix)
            .expect("wheel events must be listed");
        if let Some((location, _)) = MOUSE_LOCATIONS.iter().find(|(_, suffix)| {
            string
                .strip_prefix(prefix)
                .is_some_and(|rest| rest == *suffix)
        }) {
            return Some(make_mouse_key(event, 0, *location));
        }
    }

    for (event, prefix) in MOUSE_EVENTS {
        for button in MOUSE_BUTTONS {
            let button_string = button.to_string();
            let Some(rest) = string.strip_prefix(prefix) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix(&button_string) else {
                continue;
            };
            if let Some((location, _)) = MOUSE_LOCATIONS.iter().find(|(_, suffix)| rest == *suffix)
            {
                return Some(make_mouse_key(*event, *button, *location));
            }
        }
    }
    None
}

fn mouse_location_suffix(location: MouseLocation) -> &'static str {
    MOUSE_LOCATIONS
        .iter()
        .find(|(candidate, _)| *candidate == location)
        .map(|(_, suffix)| *suffix)
        .expect("all mouse locations must have suffixes")
}
