const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(super) fn render_open_close(
    open: &Option<String>,
    close: &Option<String>,
    value: &str,
) -> String {
    let (Some(open), Some(close)) = (open.as_deref(), close.as_deref()) else {
        return String::new();
    };
    let mut rendered = String::with_capacity(open.len() + close.len() + value.len());
    rendered.push_str(open);
    rendered.push_str(&sanitize_osc_payload(value));
    rendered.push_str(close);
    rendered
}

pub(super) fn sanitize_osc_payload(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

pub(super) fn render_string_template(template: &str, arg: &str) -> String {
    template.replace("%p1%s", &sanitize_osc_payload(arg))
}

pub(super) fn render_string_string_template(template: &str, first: &str, second: &str) -> String {
    template
        .replace("%p1%s", &sanitize_osc_payload(first))
        .replace("%p2%s", &sanitize_osc_payload(second))
}

pub(super) fn render_int_template(template: &str, value: u32) -> String {
    template.replace("%p1%d", &value.to_string())
}

pub(super) fn render_sync_template(template: &str, mode: u32) -> String {
    template
        .replace("%?%p1%{1}%-%tl%eh%;", if mode == 1 { "h" } else { "l" })
        .replace("%p1%d", &mode.to_string())
}

pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut index = 0;

    while index + 3 <= bytes.len() {
        let chunk = &bytes[index..index + 3];
        let value = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        encoded.push(BASE64_ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        encoded.push(BASE64_ALPHABET[((value >> 12) & 0x3f) as usize] as char);
        encoded.push(BASE64_ALPHABET[((value >> 6) & 0x3f) as usize] as char);
        encoded.push(BASE64_ALPHABET[(value & 0x3f) as usize] as char);
        index += 3;
    }

    match bytes.len() - index {
        0 => {}
        1 => {
            let value = u32::from(bytes[index]) << 16;
            encoded.push(BASE64_ALPHABET[((value >> 18) & 0x3f) as usize] as char);
            encoded.push(BASE64_ALPHABET[((value >> 12) & 0x3f) as usize] as char);
            encoded.push('=');
            encoded.push('=');
        }
        2 => {
            let value = (u32::from(bytes[index]) << 16) | (u32::from(bytes[index + 1]) << 8);
            encoded.push(BASE64_ALPHABET[((value >> 18) & 0x3f) as usize] as char);
            encoded.push(BASE64_ALPHABET[((value >> 12) & 0x3f) as usize] as char);
            encoded.push(BASE64_ALPHABET[((value >> 6) & 0x3f) as usize] as char);
            encoded.push('=');
        }
        _ => unreachable!("remainder must be smaller than three"),
    }

    encoded
}

pub(super) fn sync_toggle(
    bytes: &mut Vec<u8>,
    previous: Option<&String>,
    current: Option<&String>,
    disable: Option<&String>,
) {
    match (previous, current) {
        (Some(_), None) => {
            if let Some(disable) = disable {
                bytes.extend_from_slice(disable.as_bytes());
            }
        }
        (None, Some(current)) => bytes.extend_from_slice(current.as_bytes()),
        (Some(previous), Some(current)) if previous != current => {
            if let Some(disable) = disable {
                bytes.extend_from_slice(disable.as_bytes());
            }
            bytes.extend_from_slice(current.as_bytes());
        }
        _ => {}
    }
}
