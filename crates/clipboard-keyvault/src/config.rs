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

/// The URL must be https — except on loopback, where a local Convex
/// deployment is plain http and there is no wire for anyone else to read.
///
/// It must also sit at the host root: a path prefix would be silently
/// discarded when requests are joined, and a vault behind a reverse-proxy
/// prefix would fail every request with nothing naming the mistake.
///
/// Standalone so a settings form can check one field the user just typed
/// without inventing the other two to check it against.
pub fn validate_base_url(base_url: &str) -> Result<(), KeyvaultError> {
    let url = url::Url::parse(base_url.trim()).map_err(|_| KeyvaultError::InvalidUrl)?;
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
    Ok(())
}

/// A token is `kv_` and a bounded run of base64url. Standalone for the same
/// reason [`validate_base_url`] is.
pub fn validate_token(token: &str) -> Result<(), KeyvaultError> {
    let body = token
        .strip_prefix("kv_")
        .ok_or(KeyvaultError::InvalidToken)?;
    let bounded_base64url = !body.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if !bounded_base64url {
        return Err(KeyvaultError::InvalidToken);
    }
    Ok(())
}

impl KeyvaultConfig {
    /// All three parts, each by its own rule.
    pub fn validate(&self) -> Result<(), KeyvaultError> {
        validate_base_url(&self.base_url)?;
        validate_token(&self.token)?;
        crate::decrypt::validate_private_jwk(&self.private_jwk)
    }
}
