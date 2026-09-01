use std::future::Future;

use crate::{KeyvaultConfig, KeyvaultError};

/// Most body bytes one vault answer may carry. The vault speaks in slugs and
/// envelopes measured in kilobytes; anything bigger is not an answer.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// One vault answer: a status and an already-bounded body.
#[derive(Clone)]
pub struct SecretResponse {
    pub status: u16,
    pub body: String,
}

/// How the client reaches the vault.
///
/// A trait so tests can answer from a script instead of the network — the
/// network is the one dependency this crate refuses to need in its tests.
pub trait SecretTransport: Send + Sync {
    fn get(&self, path: &str)
    -> impl Future<Output = Result<SecretResponse, KeyvaultError>> + Send;
}

/// The real transport: reqwest against the configured base URL, with the
/// bearer token and a source header so the vault's access log can tell this
/// application apart from the others that read it.
pub struct ReqwestSecretTransport {
    client: reqwest::Client,
    base: url::Url,
    token: String,
}

impl ReqwestSecretTransport {
    pub fn new(config: &KeyvaultConfig) -> Result<Self, KeyvaultError> {
        config.validate()?;
        let base =
            url::Url::parse(config.base_url.trim()).map_err(|_| KeyvaultError::InvalidUrl)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("clipboard-history/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| KeyvaultError::Transport)?;
        Ok(Self {
            client,
            base,
            token: config.token.clone(),
        })
    }
}

impl SecretTransport for ReqwestSecretTransport {
    // Not `async fn`: the trait promises a Send future, which RPITIT states
    // explicitly and AFIT cannot.
    #[allow(clippy::manual_async_fn)]
    fn get(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<SecretResponse, KeyvaultError>> + Send {
        let path = path.to_owned();
        let token = self.token.clone();
        let client = self.client.clone();
        let base = self.base.clone();
        async move {
            // An absolute path replaces whatever path the base URL carries, so
            // a base pasted with or without a trailing slash behaves the same.
            let url = base
                .join(&format!("/{path}"))
                .map_err(|_| KeyvaultError::InvalidUrl)?;
            let mut response = client
                .get(url)
                .bearer_auth(token)
                .header("X-KeyVault-Source", "clipboard-history")
                .send()
                .await
                .map_err(|_| KeyvaultError::Transport)?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| KeyvaultError::Transport)?
            {
                if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err(KeyvaultError::BadResponse);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(SecretResponse {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            })
        }
    }
}
