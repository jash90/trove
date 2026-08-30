//! Pairing this device with a vault through the browser.
//!
//! The device generates its own keypair and sends only the public half, so the browser — and the
//! redirect that follows it — never carries anything secret. What comes back is a token, and even
//! that arrives sealed to this device's key rather than in the callback URL, because a URL is
//! logged by the browser, logged by the OS when it hands a custom scheme to an application, and
//! on macOS claimable by any application at all.
//!
//! Collection is a poll rather than a callback, which is why pairing finishes correctly whether
//! or not the redirect back into this application ever fires.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BigUint, RsaPrivateKey};

use crate::{KeyvaultError, decrypt_envelope, envelope::parse_envelope, parse_private_jwk};

/// A freshly generated device identity. The private half never leaves this machine.
pub struct DeviceKey {
    pub private_jwk: String,
    pub public_jwk: String,
}

/// What a vault says when a pairing opens.
#[derive(Clone, Debug)]
pub struct PairingStart {
    pub code: String,
    /// Shown to the person approving, so they can tell this pairing from someone else's.
    pub fingerprint: String,
}

/// Where a pairing has got to. `Pending` is the ordinary answer while the browser is still open.
#[derive(Clone, Debug, PartialEq)]
pub enum PairingOutcome {
    Pending,
    Approved {
        url: String,
        token: String,
    },
    Expired,
    NotFound,
    /// Someone already collected this pairing's result — it is spent, and not by us.
    AlreadyClaimed,
}

fn encode(value: &BigUint) -> String {
    URL_SAFE_NO_PAD.encode(value.to_bytes_be())
}

/// Generates this device's RSA-OAEP keypair.
///
/// 3072 bits to match what the vault's own generator mints; it costs a second or two, which is
/// why callers run it off the interface thread.
pub fn generate_device_key() -> Result<DeviceKey, KeyvaultError> {
    let mut rng = rsa::rand_core::OsRng;
    let key = RsaPrivateKey::new(&mut rng, 3072).map_err(|_| KeyvaultError::InvalidPrivateKey)?;
    let primes = key.primes();
    if primes.len() != 2 {
        return Err(KeyvaultError::InvalidPrivateKey);
    }
    let private_jwk = serde_json::json!({
        "kty": "RSA",
        "alg": "RSA-OAEP-256",
        "n": encode(key.n()),
        "e": encode(key.e()),
        "d": encode(key.d()),
        "p": encode(&primes[0]),
        "q": encode(&primes[1]),
    })
    .to_string();
    // Deliberately only the five public-safe fields: a public JWK that carried `d` would be the
    // one mistake this whole arrangement exists to make impossible, and the vault rejects it too.
    let public_jwk = serde_json::json!({
        "kty": "RSA",
        "alg": "RSA-OAEP-256",
        "n": encode(key.n()),
        "e": encode(key.e()),
    })
    .to_string();
    Ok(DeviceKey {
        private_jwk,
        public_jwk,
    })
}

/// The vault's client API, derived from the HTTP one the user typed.
///
/// The two hosts differ by a single label and the difference is invisible until a request fails,
/// so it is worth doing here rather than asking anyone to know it.
fn client_api(base_url: &str) -> Result<String, KeyvaultError> {
    crate::validate_base_url(base_url)?;
    let trimmed = base_url.trim().trim_end_matches('/');
    Ok(trimmed.replace(".convex.site", ".convex.cloud"))
}

async fn call(
    kind: &str,
    base_url: &str,
    path: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, KeyvaultError> {
    let endpoint = format!("{}/api/{kind}", client_api(base_url)?);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| KeyvaultError::Transport)?;
    // Serialized by hand rather than through reqwest's `json` feature: this crate's dependency
    // surface is small on purpose, and a content-type header is the whole of what that feature
    // would add here.
    let request = serde_json::json!({ "path": path, "args": args, "format": "json" }).to_string();
    let response = client
        .post(endpoint)
        .header("content-type", "application/json")
        .body(request)
        .send()
        .await
        .map_err(|_| KeyvaultError::Transport)?;
    if !response.status().is_success() {
        return Err(KeyvaultError::Transport);
    }
    let text = response
        .text()
        .await
        .map_err(|_| KeyvaultError::BadResponse)?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| KeyvaultError::BadResponse)?;
    // Convex reports a thrown mutation as a 200 with status "error", so the HTTP code alone is
    // not the answer.
    if body.get("status").and_then(|s| s.as_str()) != Some("success") {
        return Err(KeyvaultError::BadResponse);
    }
    Ok(body
        .get("value")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Asks the deployment where its web interface lives.
///
/// The API and the interface sit on different hosts and neither can be derived from the other, so
/// the alternative to asking is making someone type two addresses and know which is which. A
/// deployment that has not published one says so, and pairing stops with a reason rather than
/// opening a browser at a guess.
pub async fn page_url(base_url: &str) -> Result<String, KeyvaultError> {
    let value = call(
        "query",
        base_url,
        "config:getPairingUrl",
        serde_json::json!({}),
    )
    .await?;
    let url = value.as_str().ok_or(KeyvaultError::PairingPageUnknown)?;
    crate::validate_base_url(url)?;
    Ok(url.trim_end_matches('/').to_owned())
}

/// Opens a pairing and returns the code to put in the browser URL.
pub async fn start(
    base_url: &str,
    label: &str,
    public_jwk: &str,
) -> Result<PairingStart, KeyvaultError> {
    let value = call(
        "mutation",
        base_url,
        "pairing:createPairing",
        serde_json::json!({ "appPublicJwk": public_jwk, "label": label }),
    )
    .await?;
    let code = value
        .get("code")
        .and_then(|v| v.as_str())
        .ok_or(KeyvaultError::BadResponse)?;
    let fingerprint = value
        .get("fingerprint")
        .and_then(|v| v.as_str())
        .ok_or(KeyvaultError::BadResponse)?;
    Ok(PairingStart {
        code: code.to_owned(),
        fingerprint: fingerprint.to_owned(),
    })
}

/// Asks once whether the pairing has been approved, and opens the result if it has.
pub async fn claim(
    base_url: &str,
    code: &str,
    private_jwk: &str,
) -> Result<PairingOutcome, KeyvaultError> {
    let value = call(
        "mutation",
        base_url,
        "pairing:claimPairing",
        serde_json::json!({ "code": code }),
    )
    .await?;
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or(KeyvaultError::BadResponse)?;
    match status {
        "pending" => Ok(PairingOutcome::Pending),
        "expired" => Ok(PairingOutcome::Expired),
        "not_found" => Ok(PairingOutcome::NotFound),
        "claimed" => Ok(PairingOutcome::AlreadyClaimed),
        "approved" => {
            let payload = value
                .get("payload")
                .and_then(|v| v.as_str())
                .ok_or(KeyvaultError::BadResponse)?;
            open_payload(payload, private_jwk)
        }
        _ => Err(KeyvaultError::BadResponse),
    }
}

/// Opens the sealed payload. Its plaintext is a small JSON object, and it stops here.
fn open_payload(payload: &str, private_jwk: &str) -> Result<PairingOutcome, KeyvaultError> {
    let envelope = parse_envelope(payload)?;
    let key = parse_private_jwk(private_jwk)?;
    let plaintext = decrypt_envelope(&key, &envelope)?;
    // Every refusal below is PairingPayloadInvalid rather than BadResponse: the envelope opened,
    // so the network and the key are both fine and the fault is in what the vault's page put
    // inside. Saying that plainly is the difference between looking at a deployment's
    // configuration and looking at a connection.
    let parsed: serde_json::Value = serde_json::from_str(plaintext.as_str())
        .map_err(|_| KeyvaultError::PairingPayloadInvalid)?;
    let url = parsed
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or(KeyvaultError::PairingPayloadInvalid)?;
    let token = parsed
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or(KeyvaultError::PairingPayloadInvalid)?;
    // The vault's word for its own address is authoritative, but it is still a value from the
    // network heading for a config file, so it passes the same rules a typed one would.
    crate::validate_base_url(url)?;
    crate::validate_token(token)?;
    Ok(PairingOutcome::Approved {
        url: url.to_owned(),
        token: token.to_owned(),
    })
}
