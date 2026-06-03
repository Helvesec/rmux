use base64::Engine;
use hkdf::Hkdf;
use rmux_proto::RmuxError;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const SPECTATOR_TOKEN_INFO: &[u8] = b"rmux read token v1";

pub(super) fn random_share_id() -> Result<String, RmuxError> {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 5];
    getrandom::fill(&mut bytes).map_err(random_error)?;
    let value = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
    let mut out = String::with_capacity(8);
    for shift in (0..40).step_by(5).rev() {
        let index = ((value >> shift) & 0x1f) as usize;
        out.push(ALPHABET[index] as char);
    }
    Ok(out)
}

pub(super) fn random_pairing_code() -> Result<String, RmuxError> {
    loop {
        let mut bytes = [0u8; 3];
        getrandom::fill(&mut bytes).map_err(random_error)?;
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        if value < 16_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

pub(super) fn random_token() -> Result<String, RmuxError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(random_error)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn derive_spectator_token(operator_token: &str) -> Result<String, RmuxError> {
    let secret = decode_token_secret(operator_token)?;
    let hk = Hkdf::<Sha256>::new(None, &secret);
    let mut out = [0u8; 32];
    hk.expand(SPECTATOR_TOKEN_INFO, &mut out)
        .map_err(|_| RmuxError::Server("failed to derive web-share spectator token".to_owned()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SecretHash([u8; 32]);

impl SecretHash {
    pub(crate) fn from_secret(secret: &str) -> Self {
        let digest = Sha256::digest(secret.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    pub(super) const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    pub(crate) fn token_id(self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"rmux-token-id-v1");
        hasher.update(self.0);
        let digest = hasher.finalize();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&digest[..16])
    }
}

pub(super) fn valid_token_id_shape(token_id: &str) -> bool {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token_id)
        .is_ok_and(|bytes| bytes.len() == 16)
}

pub(super) fn secret_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn random_error(error: getrandom::Error) -> RmuxError {
    RmuxError::Server(format!("failed to create web-share secret: {error}"))
}

fn decode_token_secret(token: &str) -> Result<[u8; 32], RmuxError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| RmuxError::Server("invalid web-share operator token".to_owned()))?;
    let secret: [u8; 32] = bytes
        .try_into()
        .map_err(|_| RmuxError::Server("invalid web-share operator token".to_owned()))?;
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::{derive_spectator_token, random_token, SecretHash};

    #[test]
    fn token_derivation_matches_wire_stable_vector() {
        let operator = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
        let spectator = "f-dj7QKyPUJhAZabQ7IkQCRR1DoYQvIGf-OkgSGMuo4";

        assert_eq!(
            hex(&SecretHash::from_secret(operator).as_bytes()),
            "ea866a757e4c38babfa8127cbe9a409d3e1f93a00ff1488ff735fcf917afffd0",
            "PSK is SHA256 over the token string bytes, not decoded token bytes"
        );
        assert_eq!(
            SecretHash::from_secret(operator).token_id(),
            "VANRFV6FYQX1QTOi-BMVrQ"
        );
        assert_eq!(
            derive_spectator_token(operator).expect("spectator token"),
            spectator
        );
        assert_eq!(
            hex(&SecretHash::from_secret(spectator).as_bytes()),
            "107f3721f4b437db5c5e8107727565c2697b3af2a159fd6f35281b13dcd8b62a"
        );
        assert_eq!(
            SecretHash::from_secret(spectator).token_id(),
            "PhCBAZMZcF4zzFOwp0GBjA"
        );
    }

    #[test]
    fn spectator_token_derivation_is_stable_and_one_way_for_lookup() {
        let operator = random_token().expect("operator token");
        let spectator = derive_spectator_token(&operator).expect("spectator token");

        assert_eq!(
            spectator,
            derive_spectator_token(&operator).expect("same spectator token")
        );
        assert_ne!(operator, spectator);
        assert_ne!(
            SecretHash::from_secret(&operator).token_id(),
            SecretHash::from_secret(&spectator).token_id()
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
