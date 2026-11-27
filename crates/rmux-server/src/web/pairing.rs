use rmux_proto::{CreateWebShareRequest, RmuxError};

use super::record::WebShareConnectRole;
use super::secrets::{random_pairing_code, secret_eq};

#[cfg(test)]
#[path = "pin_debug_logging_tests.rs"]
mod logging;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct WebSharePairingCodes {
    operator: Option<String>,
    spectator: Option<String>,
}

impl std::fmt::Debug for WebSharePairingCodes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSharePairingCodes")
            .field("operator", &self.operator.as_ref().map(|_| "[REDACTED]"))
            .field("spectator", &self.spectator.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl WebSharePairingCodes {
    pub(super) fn for_request(request: &CreateWebShareRequest) -> Result<Self, RmuxError> {
        validate_forced_pin(request.operator_pin.as_deref(), "--pin-operator")?;
        validate_forced_pin(request.spectator_pin.as_deref(), "--pin-spectator")?;

        if !request.require_pin {
            if request.operator_pin.is_some() || request.spectator_pin.is_some() {
                return Err(RmuxError::Server(
                    "web-share forced PINs cannot be used with --no-pin".to_owned(),
                ));
            }
            return Ok(Self::default());
        }

        let operator = role_pin(request.operator, request.operator_pin.as_deref(), None)?;
        let spectator = role_pin(
            request.spectator,
            request.spectator_pin.as_deref(),
            operator.as_deref(),
        )?;

        if operator.is_some() && operator == spectator {
            return Err(RmuxError::Server(
                "web-share operator and spectator PINs must differ".to_owned(),
            ));
        }

        Ok(Self {
            operator,
            spectator,
        })
    }

    pub(super) fn operator(&self) -> Option<&str> {
        self.operator.as_deref()
    }

    pub(super) fn spectator(&self) -> Option<&str> {
        self.spectator.as_deref()
    }

    pub(super) fn check(
        &self,
        pin: Option<&str>,
        role: WebShareConnectRole,
    ) -> Result<(), RmuxError> {
        let expected = match role {
            WebShareConnectRole::Operator => self.operator(),
            WebShareConnectRole::Spectator => self.spectator(),
        };
        let Some(expected) = expected else {
            return Ok(());
        };
        if pin.is_some_and(|provided| secret_eq(provided, expected)) {
            return Ok(());
        }
        let message = if pin.is_some() {
            "invalid web-share pairing code"
        } else {
            "missing web-share pairing code"
        };
        Err(RmuxError::Server(message.to_owned()))
    }
}

fn role_pin(
    enabled: bool,
    forced: Option<&str>,
    avoid: Option<&str>,
) -> Result<Option<String>, RmuxError> {
    if !enabled {
        if forced.is_some() {
            return Err(RmuxError::Server(
                "web-share forced PIN supplied for a disabled role".to_owned(),
            ));
        }
        return Ok(None);
    }
    if let Some(pin) = forced {
        return Ok(Some(pin.to_owned()));
    }
    loop {
        let generated = random_pairing_code()?;
        if avoid != Some(generated.as_str()) {
            return Ok(Some(generated));
        }
    }
}

fn validate_forced_pin(pin: Option<&str>, flag: &str) -> Result<(), RmuxError> {
    let Some(pin) = pin else {
        return Ok(());
    };
    if pin.len() == 6 && pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(());
    }
    Err(RmuxError::Server(format!(
        "web-share {flag} must be exactly 6 ASCII digits"
    )))
}

#[cfg(test)]
mod tests {
    use rmux_proto::{CreateWebShareRequest, WebShareScope};

    use super::WebSharePairingCodes;

    pub(super) fn request() -> CreateWebShareRequest {
        CreateWebShareRequest {
            scope: WebShareScope::Session("demo".parse().expect("session name")),
            public_base_url: None,
            tunnel_provider: None,
            frontend_url: None,
            ttl_seconds: None,
            expires_at_unix: None,
            max_spectators: None,
            max_operators: None,
            url_options: Default::default(),
            require_pin: true,
            operator_pin: None,
            spectator_pin: None,
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: true,
            kill_session_on_expire: false,
        }
    }

    #[test]
    fn debug_redacts_operator_and_spectator_pins() {
        use crate::web::connection_limit::ConnectionLimit;
        use crate::web::leases::LeaseBook;
        use crate::web::record::{WebShareConnectRole, WebShareRecord, WebShareTarget};
        use crate::web::secrets::SecretHash;

        let mut request = request();
        request.operator_pin = Some("193847".to_owned());
        request.spectator_pin = Some("826591".to_owned());
        let codes = WebSharePairingCodes::for_request(&request).expect("pairing codes");
        let record = WebShareRecord {
            allow_loopback_development_origins: false,
            endpoint_origin: "https://share.example".to_owned(),
            expires_at: None,
            frontend_origin: "https://share.example".to_owned(),
            frontend_url: "https://share.example".to_owned(),
            kill_session_on_expire: false,
            lease_book: LeaseBook::new(None, None),
            max_operators: None,
            max_spectators: None,
            operator_token_hash: Some(SecretHash::from_secret("operator-token-canary")),
            pairing_codes: codes.clone(),
            revoke_tx: tokio::sync::watch::channel(None).0,
            controls: true,
            share_id: "debug-pin-canary".to_owned(),
            target: WebShareTarget::session(
                "demo".parse().expect("session name"),
                rmux_proto::SessionId::new(1),
            ),
            terminal_palette: None,
            url_options: Default::default(),
            spectator_token_hash: Some(SecretHash::from_secret("spectator-token-canary")),
            _tunnel: None,
            operator: true,
            spectator: true,
        };
        let limit = ConnectionLimit::new(2);
        let operator = record
            .connect(
                Some("193847"),
                WebShareConnectRole::Operator,
                limit.try_acquire().unwrap(),
            )
            .expect("operator authenticates");
        let spectator = record
            .connect(
                Some("826591"),
                WebShareConnectRole::Spectator,
                limit.try_acquire().unwrap(),
            )
            .expect("spectator authenticates");
        assert_eq!(
            operator.operator_visible_spectator_pairing_code(),
            Some("826591")
        );
        assert_eq!(spectator.operator_visible_spectator_pairing_code(), None);
        let diagnostics = [
            format!("{codes:?}"),
            format!("{codes:#?}"),
            format!("{record:?}"),
            format!("{record:#?}"),
            format!("{operator:?}"),
            format!("{operator:#?}"),
            format!("{spectator:?}"),
            format!("{spectator:#?}"),
        ];
        for diagnostic in diagnostics {
            for pin in ["193847", "826591"] {
                assert!(
                    !diagnostic.contains(pin),
                    "Debug must redact both role PINs"
                );
            }
        }
    }

    #[test]
    fn generated_role_pins_are_distinct() {
        let codes = WebSharePairingCodes::for_request(&request()).expect("pairing codes");

        assert_ne!(codes.operator(), codes.spectator());
    }

    #[test]
    fn forced_role_pins_are_kept() {
        let mut request = request();
        request.operator_pin = Some("123456".to_owned());
        request.spectator_pin = Some("654321".to_owned());

        let codes = WebSharePairingCodes::for_request(&request).expect("pairing codes");

        assert_eq!(codes.operator(), Some("123456"));
        assert_eq!(codes.spectator(), Some("654321"));
    }

    #[test]
    fn matching_forced_role_pins_are_rejected() {
        let mut request = request();
        request.operator_pin = Some("123456".to_owned());
        request.spectator_pin = Some("123456".to_owned());

        assert!(WebSharePairingCodes::for_request(&request).is_err());
    }

    #[test]
    fn forced_pin_requires_six_digits() {
        let mut request = request();
        request.operator_pin = Some("abc123".to_owned());

        assert!(WebSharePairingCodes::for_request(&request).is_err());
    }
}
