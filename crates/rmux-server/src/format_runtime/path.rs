pub(crate) fn pane_path_from_osc7(raw_path: &str) -> Option<String> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return None;
    }

    raw_path
        .strip_prefix("file://")
        .map(path_from_file_uri)
        .or_else(|| Some(raw_path.to_owned()))
        .filter(|path| !path.is_empty())
}

fn path_from_file_uri(uri_body: &str) -> String {
    let (host, path) = split_file_uri_body(uri_body);
    platform_path_from_file_uri(&percent_decode(host), &percent_decode(path))
}

fn split_file_uri_body(uri_body: &str) -> (&str, &str) {
    let uri_body = uri_body
        .split_once('#')
        .map_or(uri_body, |(head, _)| head)
        .split_once('?')
        .map_or(uri_body, |(head, _)| head);

    if uri_body.starts_with('/') {
        return ("", uri_body);
    }

    uri_body
        .split_once('/')
        .map_or((uri_body, ""), |(host, _)| (host, &uri_body[host.len()..]))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(windows)]
fn platform_path_from_file_uri(host: &str, path: &str) -> String {
    let path = path
        .strip_prefix('/')
        .filter(|path| has_drive_prefix(path))
        .unwrap_or(path);
    if !is_local_file_uri_host(host) && !has_drive_prefix(path) {
        return format!(
            "\\\\{}\\{}",
            host,
            path.trim_start_matches('/').replace('/', "\\")
        );
    }

    path.replace('/', "\\")
}

#[cfg(windows)]
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

#[cfg(windows)]
fn is_local_file_uri_host(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    std::env::var("COMPUTERNAME")
        .is_ok_and(|computer_name| host.eq_ignore_ascii_case(&computer_name))
}

#[cfg(not(windows))]
fn platform_path_from_file_uri(_host: &str, path: &str) -> String {
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::pane_path_from_osc7;

    #[test]
    fn preserves_non_uri_paths() {
        assert_eq!(
            pane_path_from_osc7("/tmp/plain").as_deref(),
            Some("/tmp/plain")
        );
    }

    #[test]
    fn rejects_empty_paths() {
        assert_eq!(pane_path_from_osc7(""), None);
        assert_eq!(pane_path_from_osc7("   "), None);
    }

    #[cfg(unix)]
    #[test]
    fn unix_file_uri_decodes_path_and_ignores_host() {
        assert_eq!(
            pane_path_from_osc7("file://host.example/tmp/space%20dir").as_deref(),
            Some("/tmp/space dir")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_uri_decodes_drive_path() {
        assert_eq!(
            pane_path_from_osc7("file:///C:/Users/RMUXUser%20Space").as_deref(),
            Some("C:\\Users\\RMUXUser Space")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_uri_decodes_unc_path() {
        assert_eq!(
            pane_path_from_osc7("file://server/share/RMUXUser%20Space").as_deref(),
            Some("\\\\server\\share\\RMUXUser Space")
        );
    }
}
