//! Web-share authentication: token pre-checks, bounded authentication waits
//! and the backoff-throttled connect path.

use std::net::IpAddr;

use rmux_proto::RmuxError;
use tokio::time::sleep;
use tracing::info;

use super::super::auth_wait_limit::AuthWaitPermit;
use super::super::backoff::{AttemptOutcome, AttemptReservation};
use super::super::record::WebShareAccess;
use super::super::secrets::{valid_token_id_shape, SecretHash};
use super::WebShareRegistry;

const UNKNOWN_TOKEN_BACKOFF_KEY: &str = "token_id:<unknown>";

#[derive(Debug)]
pub(crate) struct WebShareAuthWaitPermit {
    backoff_key: String,
    _permit: AuthWaitPermit,
    token_id: String,
}

impl WebShareRegistry {
    #[cfg(test)]
    pub(crate) async fn connect(
        &self,
        token: &str,
        pin: Option<&str>,
    ) -> Result<WebShareAccess, RmuxError> {
        let token_id = SecretHash::from_secret(token).token_id();
        self.connect_token_id(&token_id, pin).await
    }

    #[cfg(test)]
    pub(crate) async fn connect_from_peer(
        &self,
        token: &str,
        pin: Option<&str>,
        peer: IpAddr,
    ) -> Result<WebShareAccess, RmuxError> {
        let token_id = SecretHash::from_secret(token).token_id();
        self.connect_token_id_from_peer(&token_id, pin, peer).await
    }

    #[cfg(test)]
    pub(crate) fn known_token_origin_allowed(&self, token: &str, origin: &str) -> Option<bool> {
        let token_id = SecretHash::from_secret(token).token_id();
        self.pre_auth_token(&token_id, origin)
            .map(|(_, allowed)| allowed)
    }

    #[cfg(test)]
    pub(crate) async fn connect_token_id(
        &self,
        token_id: &str,
        pin: Option<&str>,
    ) -> Result<WebShareAccess, RmuxError> {
        let auth_wait = self.reserve_authentication_wait(token_id, None)?;
        self.connect_token_id_with_wait(token_id, pin, &auth_wait)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn connect_token_id_from_peer(
        &self,
        token_id: &str,
        pin: Option<&str>,
        peer: IpAddr,
    ) -> Result<WebShareAccess, RmuxError> {
        let auth_wait = self.reserve_authentication_wait(token_id, Some(peer))?;
        self.connect_token_id_with_wait(token_id, pin, &auth_wait)
            .await
    }

    /// Settles `failures` wrong-PIN attempts for `token_id` through the real
    /// reservation and backoff path, without sleeping their delays.
    #[cfg(test)]
    pub(crate) fn record_authentication_failures_for_test(&self, token_id: &str, failures: u32) {
        let auth_wait = self
            .reserve_authentication_wait(token_id, None)
            .expect("test token reserves an authentication wait");
        for _ in 0..failures {
            match self.backoff.reserve_attempt(&auth_wait.backoff_key) {
                AttemptReservation::Wait { guard, .. } => guard.settle(AttemptOutcome::AuthFailure),
                AttemptReservation::Locked => panic!("test failures must stay below the lock"),
            }
        }
    }

    pub(crate) fn reserve_authentication_wait(
        &self,
        token_id: &str,
        peer: Option<IpAddr>,
    ) -> Result<WebShareAuthWaitPermit, RmuxError> {
        if !valid_token_id_shape(token_id) {
            return Err(RmuxError::Server("invalid web-share token id".to_owned()));
        }
        let lookup = {
            let mut inner = self
                .inner
                .lock()
                .expect("web-share registry mutex must not be poisoned");
            inner.prune_expired();
            inner.capability_by_token_id(token_id)
        };
        let backoff_key = lookup
            .as_ref()
            .map(|capability| {
                format!(
                    "{}:{}",
                    capability.share_id,
                    capability.role.backoff_label()
                )
            })
            .unwrap_or_else(|| UNKNOWN_TOKEN_BACKOFF_KEY.to_owned());
        // Once the encrypted pre-ready handshake releases its small socket
        // queue, keep authentication tasks bounded independently by daemon,
        // role/share key, and network peer. A single leaked share link must not
        // consume established-client capacity while its PIN attempt sleeps.
        let permit = self
            .auth_wait_limit
            .try_acquire(&backoff_key, peer)
            .ok_or_else(|| {
                RmuxError::Server("web-share authentication queue limit reached".to_owned())
            })?;
        Ok(WebShareAuthWaitPermit {
            backoff_key,
            _permit: permit,
            token_id: token_id.to_owned(),
        })
    }

    pub(crate) async fn connect_token_id_with_wait(
        &self,
        token_id: &str,
        pin: Option<&str>,
        auth_wait: &WebShareAuthWaitPermit,
    ) -> Result<WebShareAccess, RmuxError> {
        if auth_wait.token_id != token_id {
            return Err(RmuxError::Server(
                "web-share authentication reservation mismatch".to_owned(),
            ));
        }
        let backoff_key = &auth_wait.backoff_key;
        // Reserve the attempt up front so concurrent guesses are throttled by the
        // in-flight count, not just by past recorded failures. The guard makes
        // this cancellation-safe: if an async timeout or task abort drops the
        // future before an explicit result, the attempt settles as `Other`.
        let (wait, attempt) = match self.backoff.reserve_attempt(backoff_key) {
            AttemptReservation::Locked => {
                // Failure budget exhausted: fail closed while the temporary
                // lock is active. Return the SAME uniform error as a missing
                // share so no new oracle is exposed, and do not settle (the
                // locked path reserves no attempt).
                info!(share_id = %backoff_key, "web_share_auth_locked");
                return Err(RmuxError::Server(
                    "web-share does not exist or has expired".to_owned(),
                ));
            }
            AttemptReservation::Wait { delay, guard } => (delay, guard),
        };
        if !wait.is_zero() {
            sleep(wait).await;
        }

        let result = match self.connection_limit.try_acquire() {
            Some(connection_permit) => {
                let mut inner = self
                    .inner
                    .lock()
                    .expect("web-share registry mutex must not be poisoned");
                inner.prune_expired();
                match inner.capability_by_token_id(token_id) {
                    Some(capability) => match inner.records.get(&capability.share_id) {
                        Some(record) => record.connect(pin, capability.role, connection_permit),
                        None => Err(RmuxError::Server(
                            "web-share does not exist or has expired".to_owned(),
                        )),
                    },
                    None => Err(RmuxError::Server(
                        "web-share does not exist or has expired".to_owned(),
                    )),
                }
            }
            None => Err(RmuxError::Server(
                "web-share connection limit reached".to_owned(),
            )),
        };

        match result {
            Ok(access) => {
                attempt.settle(AttemptOutcome::Success);
                info!(share_id = %access.share_id(), role = ?access.connect_role(), "web_share_access_granted");
                Ok(access)
            }
            Err(error) => {
                let outcome = if is_auth_failure_for_backoff(&error) {
                    AttemptOutcome::AuthFailure
                } else {
                    AttemptOutcome::Other
                };
                attempt.settle(outcome);
                if matches!(outcome, AttemptOutcome::AuthFailure) {
                    info!(share_id = %backoff_key, "web_share_auth_backoff");
                }
                Err(error)
            }
        }
    }

    pub(crate) fn pre_auth_token(
        &self,
        token_id: &str,
        origin: &str,
    ) -> Option<(SecretHash, bool)> {
        if !valid_token_id_shape(token_id) {
            return None;
        }
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let capability = inner.capability_by_token_id(token_id)?;
        let record = inner.records.get(&capability.share_id)?;
        Some((capability.secret_hash, record.origin_allowed(origin)))
    }
}

fn is_auth_failure_for_backoff(error: &RmuxError) -> bool {
    let message = error.to_string();
    message.contains("invalid web-share pairing code")
        || message.contains("does not exist or has expired")
}
