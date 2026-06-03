use rmux_proto::{CreateWebShareRequest, RmuxError};

use super::record::WebShareConnectRole;
use super::secrets::{random_pairing_code, secret_eq};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WebSharePairingCodes {
    operator: Option<String>,
    spectator: Option<String>,
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

    fn request() -> CreateWebShareRequest {
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
