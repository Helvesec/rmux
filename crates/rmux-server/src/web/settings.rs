use rmux_proto::{RmuxError, WebShareListener};

use super::origin::{validate_frontend_url, FrontendUrl};

const DEFAULT_FRONTEND_ORIGIN: &str = "https://share.rmux.io";
const DEFAULT_FRONTEND_URL: &str = "https://share.rmux.io";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 9777;

#[derive(Debug, Clone)]
pub(crate) struct WebShareSettings {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) port_explicit: bool,
    pub(super) frontend_origin: String,
    pub(super) frontend_url: String,
}

impl Default for WebShareSettings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            port_explicit: false,
            frontend_origin: DEFAULT_FRONTEND_ORIGIN.to_owned(),
            frontend_url: DEFAULT_FRONTEND_URL.to_owned(),
        }
    }
}

impl WebShareSettings {
    #[cfg(test)]
    pub(crate) fn from_options(
        port: u16,
        frontend_origin: Option<String>,
    ) -> Result<Self, RmuxError> {
        Self::from_options_with_port_explicit(port, frontend_origin, true)
    }

    pub(crate) fn from_options_with_port_explicit(
        port: u16,
        frontend_origin: Option<String>,
        port_explicit: bool,
    ) -> Result<Self, RmuxError> {
        if port == 0 {
            return Err(RmuxError::Server(
                "web-share listener port must be between 1 and 65535".to_owned(),
            ));
        }
        let frontend = match frontend_origin {
            Some(value) => validate_frontend_url(&value)?,
            None => FrontendUrl {
                origin: DEFAULT_FRONTEND_ORIGIN.to_owned(),
                url: DEFAULT_FRONTEND_URL.to_owned(),
            },
        };
        Ok(Self {
            host: DEFAULT_HOST.to_owned(),
            port,
            port_explicit,
            frontend_origin: frontend.origin,
            frontend_url: frontend.url,
        })
    }

    pub(crate) fn listener(&self) -> WebShareListener {
        WebShareListener {
            host: self.host.clone(),
            port: self.port,
            frontend_origin: self.frontend_origin.clone(),
        }
    }

    pub(crate) fn local_endpoint_origin(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub(crate) const fn allows_automatic_port_fallback(&self) -> bool {
        !self.port_explicit
    }

    pub(crate) fn set_port(&mut self, port: u16) {
        self.port = port;
    }
}
