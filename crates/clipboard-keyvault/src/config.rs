use crate::KeyvaultError;

/// Most bytes a bearer token may carry. Real ones are `kv_` plus 43
/// base64url characters; the bound exists so a pasted blob cannot become one.
pub const MAX_TOKEN_BYTES: usize = 512;

/// Where the vault lives, how this device proves itself to it, and the private
/// half of the keypair the vault seals secrets to.
///
/// Deliberately without `Debug`: the token and the private key are the two
/// things a stray debug log must never print.
pub struct KeyvaultConfig {
    pub base_url: String,
    pub token: String,
    pub private_jwk: String,
}

impl KeyvaultConfig {
    /// The URL must be https — except on loopback, where a local Convex
    /// deployment is plain http and there is no wire for anyone else to read.
    ///
    /// It must also sit at the host root: a path prefix would be silently
    /// discarded when requests are joined, and a vault behind a reverse-proxy
    /// prefix would fail every request with nothing naming the mistake.
    pub fn validate(&self) -> Result<(), KeyvaultError> {
        let url = url::Url::parse(self.base_url.trim()).map_err(|_| KeyvaultError::InvalidUrl)?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]")
        );
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            _ => return Err(KeyvaultError::InvalidUrl),
        }
        if url.path() != "/" {
            return Err(KeyvaultError::InvalidUrl);
        }

        let body = self
            .token
            .strip_prefix("kv_")
            .ok_or(KeyvaultError::InvalidToken)?;
        let token_is_bounded_base64url = !body.is_empty()
            && self.token.len() <= MAX_TOKEN_BYTES
            && body
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !token_is_bounded_base64url {
            return Err(KeyvaultError::InvalidToken);
        }

        crate::decrypt::validate_private_jwk(&self.private_jwk)
    }
}
