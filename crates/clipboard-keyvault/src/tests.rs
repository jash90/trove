use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rsa::rand_core::{OsRng, RngCore};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BigUint, Oaep, RsaPrivateKey};
use serde_json::json;
use sha2::Sha256;

use crate::{
    KeyvaultClient, KeyvaultConfig, KeyvaultError, SecretRef, SecretResponse, SecretTransport,
    decrypt_envelope, envelope, parse_private_jwk,
};

const BASE: &str = "https://trustworthy-eagle-783.convex.site";
const TOKEN: &str = "kv_AbCdEf0123456789-_";

/// A transport that answers from a script: no network in this crate's tests.
struct CannedTransport {
    responses: Vec<Result<SecretResponse, KeyvaultError>>,
    requested: std::sync::Mutex<Vec<String>>,
}

impl CannedTransport {
    fn with(responses: Vec<Result<SecretResponse, KeyvaultError>>) -> Self {
        Self {
            responses,
            requested: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn json(status: u16, body: &str) -> Result<SecretResponse, KeyvaultError> {
        Ok(SecretResponse {
            status,
            body: body.to_owned(),
        })
    }
}

impl SecretTransport for CannedTransport {
    #[allow(clippy::manual_async_fn)]
    fn get(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<SecretResponse, KeyvaultError>> + Send {
        async move {
            let mut requested = self.requested.lock().unwrap();
            let index = requested.len();
            requested.push(path.to_owned());
            self.responses[index].clone()
        }
    }
}

fn config(base_url: &str, token: &str, private_jwk: &str) -> KeyvaultConfig {
    KeyvaultConfig {
        base_url: base_url.to_owned(),
        token: token.to_owned(),
        private_jwk: private_jwk.to_owned(),
    }
}

/// Shape-correct components: valid base64url in the five fields the decryptor
/// reads. Not a real key — the round-trip tests generate one of those, since
/// only round-tripping proves the format agreement.
fn well_formed_jwk() -> String {
    let field = |bytes: &[u8]| URL_SAFE_NO_PAD.encode(bytes);
    json!({
        "kty": "RSA",
        "n": field(&[0x0a; 96]),
        "e": field(&[1, 0, 1]),
        "d": field(&[0x0b; 96]),
        "p": field(&[0x0c; 48]),
        "q": field(&[0x0d; 48]),
    })
    .to_string()
}

#[test]
fn https_loopback_and_well_formed_credentials_validate() {
    let good = config(BASE, TOKEN, &well_formed_jwk());
    assert_eq!(good.validate(), Ok(()));

    // A local Convex deployment serves its HTTP API over plain http on
    // loopback; there is no wire there for anyone else to read.
    let local = config("http://127.0.0.1:3211", TOKEN, &well_formed_jwk());
    assert_eq!(local.validate(), Ok(()));
    let named_local = config("http://localhost:3211", TOKEN, &well_formed_jwk());
    assert_eq!(named_local.validate(), Ok(()));
}

#[test]
fn plain_http_off_loopback_and_broken_urls_are_refused() {
    for base in [
        "http://vault.example.com",
        "vault.example.com",
        "https://vault.example.com:not-a-port/",
        "ftp://vault.example.com",
    ] {
        let bad = config(base, TOKEN, &well_formed_jwk());
        assert_eq!(bad.validate(), Err(KeyvaultError::InvalidUrl), "{base}");
    }
}

#[test]
fn tokens_must_be_bounded_kv_prefixed_base64url() {
    for token in [
        "bearer-token",
        "kv_",
        "kv_sp@ce",
        &format!("kv_{}", "a".repeat(600)),
    ] {
        let bad = config(BASE, token, &well_formed_jwk());
        assert_eq!(bad.validate(), Err(KeyvaultError::InvalidToken), "{token}");
    }
}

#[test]
fn the_private_key_must_be_five_component_base64url_json() {
    for jwk in [
        "not json",
        r#"{"kty":"RSA"}"#,
        r#"{"n":"!!!","e":"AQAB","d":"AQ","p":"AQ","q":"AQ"}"#,
    ] {
        let bad = config(BASE, TOKEN, jwk);
        assert_eq!(
            bad.validate(),
            Err(KeyvaultError::InvalidPrivateKey),
            "{jwk}"
        );
    }
}

/// Seals exactly the way the vault's browser does: a one-shot AES-256-GCM key
/// wrapped with RSA-OAEP-SHA256, tag appended to the ciphertext.
fn sealed_envelope(key: &RsaPrivateKey, plaintext: &str) -> String {
    let mut rng = OsRng;
    let mut aes_key = [0u8; 32];
    rng.fill_bytes(&mut aes_key);
    let mut iv = [0u8; 12];
    rng.fill_bytes(&mut iv);
    let wrapped = key
        .to_public_key()
        .encrypt(&mut rng, Oaep::new::<Sha256>(), &aes_key)
        .unwrap();
    use aes_gcm::aead::{Aead, KeyInit};
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&aes_key).unwrap();
    let ciphertext = cipher.encrypt((&iv).into(), plaintext.as_bytes()).unwrap();
    json!({
        "v": 1,
        "encKey": STANDARD.encode(wrapped),
        "iv": STANDARD.encode(iv),
        "ct": STANDARD.encode(ciphertext),
    })
    .to_string()
}

fn jwk_for(key: &RsaPrivateKey) -> String {
    let encode = |value: &BigUint| URL_SAFE_NO_PAD.encode(value.to_bytes_be());
    json!({
        "kty": "RSA",
        "n": encode(key.n()),
        "e": encode(key.e()),
        "d": encode(key.d()),
        "p": encode(&key.primes()[0]),
        "q": encode(&key.primes()[1]),
    })
    .to_string()
}

#[test]
fn round_trips_a_browser_sealed_envelope() {
    let mut rng = OsRng;
    let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let envelope =
        envelope::parse_envelope(&sealed_envelope(&key, "tajna wartość klucza")).unwrap();
    let opened = parse_private_jwk(&jwk_for(&key)).unwrap();
    let plaintext = decrypt_envelope(&opened, &envelope).unwrap();
    assert_eq!(plaintext.as_str(), "tajna wartość klucza");
}

#[test]
fn a_sealed_envelope_refuses_the_wrong_private_key() {
    let mut rng = OsRng;
    let holder = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let stranger = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let envelope = envelope::parse_envelope(&sealed_envelope(&holder, "value")).unwrap();
    let wrong = parse_private_jwk(&jwk_for(&stranger)).unwrap();
    assert!(matches!(
        decrypt_envelope(&wrong, &envelope),
        Err(KeyvaultError::DecryptFailed)
    ));
}

#[test]
fn envelopes_carry_a_version_and_a_size_bound() {
    let mut rng = OsRng;
    let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let valid = sealed_envelope(&key, "value");

    let versioned = valid.replace("\"v\":1", "\"v\":2");
    assert!(matches!(
        envelope::parse_envelope(&versioned),
        Err(KeyvaultError::EnvelopeUnsupportedVersion)
    ));

    let oversized = format!("{}{}", " ".repeat(crate::MAX_ENVELOPE_BYTES), valid);
    assert!(matches!(
        envelope::parse_envelope(&oversized),
        Err(KeyvaultError::EnvelopeTooLarge)
    ));

    for malformed in [
        r#"{"v":1,"encKey":"!!","iv":"AQIDBAUGBwgJCgsM","ct":"AQ"}"#,
        r#"{"v":1,"encKey":"AQID","iv":"AQ","ct":"AQID"}"#,
        r#"{"v":1,"encKey":"AQID"}"#,
    ] {
        assert!(matches!(
            envelope::parse_envelope(malformed),
            Err(KeyvaultError::EnvelopeInvalid)
        ));
    }
}

#[tokio::test]
async fn the_client_lists_metadata_and_fetches_sealed_envelopes() {
    let transport = CannedTransport::with(vec![
        CannedTransport::json(
            200,
            r#"{"secrets":[{"slug":"openai","name":"OpenAI","category":"ai"},
                            {"slug":"github","name":"GitHub"}]}"#,
        ),
        CannedTransport::json(
            200,
            r#"{"slug":"openai","name":"OpenAI","ciphertext":"{\"v\":1,\"encKey\":\"AQIDBAUG\",\"iv\":\"AQIDBAUGBwgJCgsM\",\"ct\":\"AQIDBAUG\"}"}"#,
        ),
    ]);
    let client = KeyvaultClient::new(transport);

    let secrets = client.list().await.unwrap();
    assert_eq!(
        secrets,
        vec![
            SecretRef {
                slug: "openai".to_owned(),
                name: "OpenAI".to_owned(),
                category: Some("ai".to_owned()),
            },
            SecretRef {
                slug: "github".to_owned(),
                name: "GitHub".to_owned(),
                category: None,
            },
        ]
    );

    let (name, _envelope) = client.fetch_sealed("openai").await.unwrap();
    assert_eq!(name, "OpenAI");
}

#[tokio::test]
async fn denials_map_to_stable_codes() {
    for (status, expected) in [
        (401u16, KeyvaultError::Unauthorized),
        (403, KeyvaultError::AgentAccessDisabled),
        (404, KeyvaultError::NotFound),
        (429, KeyvaultError::RateLimited),
        (500, KeyvaultError::BadResponse),
    ] {
        let transport = CannedTransport::with(vec![CannedTransport::json(status, "{}")]);
        let client = KeyvaultClient::new(transport);
        assert_eq!(client.list().await, Err(expected), "status {status}");
    }
}

#[tokio::test]
async fn a_slug_that_cannot_exist_costs_no_round_trip() {
    let transport = CannedTransport::with(Vec::new());
    let client = KeyvaultClient::new(transport);

    for slug in ["Bad", "with space", "-leading", &"a".repeat(65), ""] {
        assert!(
            matches!(
                client.fetch_sealed(slug).await,
                Err(KeyvaultError::InvalidSlug)
            ),
            "{slug}"
        );
    }
}
