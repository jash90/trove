use crate::{AgentEnvelope, KeyvaultError, SecretTransport};

/// One secret's metadata — everything the list may show, which is everything
/// the vault will say without opening anything.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SecretRef {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
}

/// The vault's slug grammar, enforced before any request: a slug that cannot
/// exist costs no round trip.
fn is_valid_slug(slug: &str) -> bool {
    let mut tail = slug.bytes();
    matches!(tail.next(), Some(b'a'..=b'z' | b'0'..=b'9'))
        && slug.len() <= 64
        && tail.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

#[derive(serde::Deserialize)]
struct ListResponse {
    secrets: Vec<SecretRef>,
}

#[derive(serde::Deserialize)]
struct SealedResponse {
    name: String,
    ciphertext: String,
}

/// The vault client: list metadata, fetch a sealed envelope.
///
/// Slugs are ASCII by grammar, so they need no percent-encoding on the wire.
pub struct KeyvaultClient<T> {
    transport: T,
}

impl<T: SecretTransport> KeyvaultClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Lists the agent-readable secrets inside the token's scope.
    pub async fn list(&self) -> Result<Vec<SecretRef>, KeyvaultError> {
        let response = self.transport.get("api/secrets").await?;
        check_status(response.status)?;
        let parsed: ListResponse =
            serde_json::from_str(&response.body).map_err(|_| KeyvaultError::BadResponse)?;
        Ok(parsed.secrets)
    }

    /// Fetches one secret as a sealed envelope, ready for
    /// [`crate::decrypt_envelope`].
    pub async fn fetch_sealed(&self, slug: &str) -> Result<(String, AgentEnvelope), KeyvaultError> {
        if !is_valid_slug(slug) {
            return Err(KeyvaultError::InvalidSlug);
        }
        let response = self.transport.get(&format!("api/secrets/{slug}")).await?;
        check_status(response.status)?;
        let parsed: SealedResponse =
            serde_json::from_str(&response.body).map_err(|_| KeyvaultError::BadResponse)?;
        let envelope = crate::envelope::parse_envelope(&parsed.ciphertext)?;
        Ok((parsed.name, envelope))
    }
}

/// Maps a vault denial to the code the interface can show. A 404 is also what
/// an out-of-scope slug returns: the vault does not reveal existence.
fn check_status(status: u16) -> Result<(), KeyvaultError> {
    match status {
        200..=299 => Ok(()),
        400 => Err(KeyvaultError::BadResponse),
        401 => Err(KeyvaultError::Unauthorized),
        403 => Err(KeyvaultError::AgentAccessDisabled),
        404 => Err(KeyvaultError::NotFound),
        429 => Err(KeyvaultError::RateLimited),
        _ => Err(KeyvaultError::BadResponse),
    }
}
