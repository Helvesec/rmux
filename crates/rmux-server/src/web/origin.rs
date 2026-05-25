use rmux_proto::RmuxError;
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrontendUrl {
    pub(crate) origin: String,
    pub(crate) url: String,
}

pub(crate) fn origin_matches(received: &str, expected: &str) -> bool {
    let Some(received) = normalize_origin(received) else {
        return false;
    };
    let Some(expected) = normalize_origin(expected) else {
        return false;
    };
    secret_eq(received.as_bytes(), expected.as_bytes())
}

pub(crate) fn origin_allowed(
    received: &str,
    expected: &str,
    allow_loopback_development: bool,
) -> bool {
    origin_matches(received, expected)
        || allow_loopback_development && is_loopback_development_origin(received)
}

pub(crate) fn validate_public_base_url(value: &str) -> Result<String, RmuxError> {
    let trimmed = value.trim();
    let Some(origin) = normalize_origin(trimmed) else {
        return Err(RmuxError::Server(
            "web-share public URL must be an ASCII origin without path, query, or fragment"
                .to_owned(),
        ));
    };
    let (scheme, rest) = origin
        .split_once("://")
        .expect("normalized origin must contain scheme separator");
    let host = rest.split_once(':').map(|(host, _)| host).unwrap_or(rest);
    if scheme == "http" && !is_loopback_host(host) {
        return Err(RmuxError::Server(
            "web-share public URL must use https:// outside localhost".to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn validate_frontend_url(value: &str) -> Result<FrontendUrl, RmuxError> {
    let trimmed = value.trim();
    let Some((origin, path)) = split_url_origin_and_path(trimmed) else {
        return Err(RmuxError::Server(
            "web-share frontend URL must be an ASCII http(s) URL without query or fragment"
                .to_owned(),
        ));
    };
    let Some(normalized_origin) = normalize_origin(origin) else {
        return Err(RmuxError::Server(
            "web-share frontend URL must use a valid ASCII origin".to_owned(),
        ));
    };
    let (scheme, rest) = normalized_origin
        .split_once("://")
        .expect("normalized origin must contain scheme separator");
    let host = rest.split_once(':').map(|(host, _)| host).unwrap_or(rest);
    if scheme == "http" && !is_loopback_host(host) {
        return Err(RmuxError::Server(
            "web-share frontend URL must use https:// outside localhost".to_owned(),
        ));
    }
    let url = match path {
        "" | "/" => origin.to_owned(),
        path => format!("{}{}", origin, path.trim_end_matches('/')),
    };
    Ok(FrontendUrl {
        origin: origin.to_owned(),
        url,
    })
}

fn split_url_origin_and_path(value: &str) -> Option<(&str, &str)> {
    if !value.is_ascii() || value.contains('?') || value.contains('#') {
        return None;
    }
    let (scheme, rest) = value.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let path_start = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..path_start];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let origin_end = scheme.len() + "://".len() + authority.len();
    Some((&value[..origin_end], &value[origin_end..]))
}

fn is_loopback_development_origin(value: &str) -> bool {
    let Some(origin) = normalize_origin(value) else {
        return false;
    };
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split_once(':').map(|(host, _)| host).unwrap_or(rest);
    is_loopback_host(host)
}

fn normalize_origin(value: &str) -> Option<String> {
    if !value.is_ascii() || value.contains('/') && !value.contains("://") {
        return None;
    }
    let lowered = value.trim().to_ascii_lowercase();
    let (scheme, authority) = lowered.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
        || authority.contains('@')
    {
        return None;
    }
    let (host, port) = parse_authority(authority, scheme)?;
    if host.starts_with("xn--") || host.contains(".xn--") || !valid_host(&host) {
        return None;
    }
    if scheme == "http" && !is_loopback_host(&host) {
        return None;
    }
    Some(format!("{scheme}://{host}:{port}"))
}

fn parse_authority(authority: &str, scheme: &str) -> Option<(String, u16)> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, raw_port))
            if !raw_port.is_empty() && raw_port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let port = raw_port.parse::<u16>().ok()?;
            (host, port)
        }
        Some(_) => return None,
        None => (authority, default_port(scheme)),
    };
    Some((host.to_owned(), port))
}

fn default_port(scheme: &str) -> u16 {
    match scheme {
        "http" => 80,
        "https" => 443,
        _ => unreachable!("scheme is validated before default_port"),
    }
}

fn valid_host(host: &str) -> bool {
    if is_loopback_host(host) {
        return true;
    }
    if host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(valid_dns_label)
}

fn valid_dns_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 {
        return false;
    }
    let bytes = label.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    let Some(last) = bytes.last() else {
        return false;
    };
    if !first.is_ascii_lowercase() || !last.is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost")
}

fn secret_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

#[cfg(test)]
mod tests {
    use super::{origin_allowed, origin_matches, validate_frontend_url, validate_public_base_url};

    #[test]
    fn origin_matrix_matches_security_contract() {
        let cases = [
            (
                "https://share.example.com",
                "https://share.example.com",
                true,
            ),
            (
                "https://SHARE.example.com",
                "https://share.example.com",
                true,
            ),
            (
                "https://share.example.com:443",
                "https://share.example.com",
                true,
            ),
            (
                "https://share.example.com/",
                "https://share.example.com",
                false,
            ),
            (
                "https://share.example.com/foo",
                "https://share.example.com",
                false,
            ),
            (
                "https://share.example.com?x=1",
                "https://share.example.com",
                false,
            ),
            (
                "http://share.example.com",
                "https://share.example.com",
                false,
            ),
            (
                "https://share.example.com.evil.com",
                "https://share.example.com",
                false,
            ),
            ("https://xn--n3h.com", "https://snow.example", false),
            (
                "https://user@share.example.com",
                "https://share.example.com",
                false,
            ),
            (
                "https://share..example.com",
                "https://share.example.com",
                false,
            ),
            ("http://localhost:9777", "http://localhost:9777", true),
            ("http://127.0.0.1:9777", "http://127.0.0.1:9777", true),
            ("http://192.168.1.5", "http://192.168.1.5", false),
        ];
        for (received, expected, accepted) in cases {
            assert_eq!(
                origin_matches(received, expected),
                accepted,
                "{received} against {expected}"
            );
        }
    }

    #[test]
    fn public_base_url_rejects_non_loopback_http() {
        assert!(validate_public_base_url("http://share.example.com").is_err());
        assert!(validate_public_base_url("http://127.0.0.1:9777").is_ok());
        assert!(validate_public_base_url("https://share.example.com").is_ok());
        assert!(validate_public_base_url("https://share.example.com/path").is_err());
    }

    #[test]
    fn frontend_url_accepts_paths_and_derives_origin() {
        let frontend = validate_frontend_url("https://share.example.com/share/")
            .expect("frontend URL with path");
        assert_eq!(frontend.origin, "https://share.example.com");
        assert_eq!(frontend.url, "https://share.example.com/share");
        assert!(validate_frontend_url("https://share.example.com/share?x=1").is_err());
        assert!(validate_frontend_url("http://share.example.com/share").is_err());
        assert!(validate_frontend_url("http://127.0.0.1:4321/share").is_ok());
    }

    #[test]
    fn local_mode_allows_loopback_development_origins_in_addition_to_frontend() {
        assert!(origin_allowed(
            "https://share.rmux.io",
            "https://share.rmux.io",
            true
        ));
        assert!(origin_allowed(
            "http://localhost:4321",
            "https://share.rmux.io",
            true
        ));
        assert!(origin_allowed(
            "http://127.0.0.1:5173",
            "https://share.rmux.io",
            true
        ));
        assert!(!origin_allowed(
            "http://localhost:4321",
            "https://share.rmux.io",
            false
        ));
        assert!(!origin_allowed(
            "https://localhost:4321",
            "https://share.rmux.io",
            true
        ));
    }
}
