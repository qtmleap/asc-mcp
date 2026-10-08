//! Public App Store Connect HTTP and ES256 JWT client. Reviewer cookies never
//! enter this client; asset uploads use a separate unauthenticated request.
use crate::api_error::{ApiError, MAX_ERROR_BODY, sensitive_values};
use anyhow::{Result, anyhow, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Method, Url};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_RESPONSE: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub struct ApiClient {
    pub http: reqwest::Client,
    origin: Url,
    auth: Arc<Auth>,
}
enum Auth {
    Key {
        issuer: String,
        key_id: String,
        key: EncodingKey,
        cached: Mutex<Option<(String, u64)>>,
    },
    Offline,
}
#[derive(Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
    aud: &'static str,
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
impl ApiClient {
    pub fn from_env() -> Result<Option<Self>> {
        let issuer = env::var("ASC_ISSUER_ID").unwrap_or_default();
        let key_id = env::var("ASC_KEY_ID").unwrap_or_default();
        let path = env::var("ASC_PRIVATE_KEY_PATH").unwrap_or_default();
        if issuer.is_empty() && key_id.is_empty() && path.is_empty() {
            return Ok(None);
        }
        if issuer.is_empty() || key_id.is_empty() || path.is_empty() {
            bail!("ASC_ISSUER_ID, ASC_KEY_ID and ASC_PRIVATE_KEY_PATH must be configured together")
        }
        Self::with_key(
            &issuer,
            &key_id,
            &fs::read(path).map_err(|_| anyhow!("unable to read API private key"))?,
        )
        .map(Some)
    }
    pub fn with_key(issuer: &str, key_id: &str, pem: &[u8]) -> Result<Self> {
        let key =
            EncodingKey::from_ec_pem(pem).map_err(|_| anyhow!("invalid ES256 API private key"))?;
        Ok(Self {
            http: Self::http()?,
            origin: Url::parse("https://api.appstoreconnect.apple.com")?,
            auth: Arc::new(Auth::Key {
                issuer: issuer.into(),
                key_id: key_id.into(),
                key,
                cached: Mutex::new(None),
            }),
        })
    }
    /// Explicit offline endpoint constructor for local compatibility tests.
    /// Neither the CLI nor MCP arguments expose an endpoint override.
    pub fn with_endpoint(endpoint: &str) -> Result<Self> {
        let origin = Url::parse(endpoint)?;
        if !matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "::1")) {
            bail!("test endpoint must be loopback")
        }
        Ok(Self {
            http: Self::http()?,
            origin,
            auth: Arc::new(Auth::Offline),
        })
    }
    fn http() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(60))
            .build()?)
    }
    pub fn token(&self) -> Result<String> {
        match &*self.auth {
            Auth::Offline => Ok("offline-test-token".into()),
            Auth::Key {
                issuer,
                key_id,
                key,
                cached,
            } => {
                let time = now()?;
                let mut state = cached
                    .lock()
                    .map_err(|_| anyhow!("JWT cache unavailable"))?;
                if let Some((token, exp)) = &*state
                    && *exp > time + 120
                {
                    return Ok(token.clone());
                }
                let exp = time + 900;
                let mut header = Header::new(Algorithm::ES256);
                header.kid = Some(key_id.clone());
                let token = jsonwebtoken::encode(
                    &header,
                    &Claims {
                        iss: issuer,
                        iat: time,
                        exp,
                        aud: "appstoreconnect-v1",
                    },
                    key,
                )
                .map_err(|_| anyhow!("unable to sign ES256 JWT"))?;
                *state = Some((token.clone(), exp));
                Ok(token)
            }
        }
    }
    pub async fn raw_request(
        &self,
        method: &str,
        path: &str,
        query: Vec<(String, String)>,
        body: Option<Value>,
    ) -> Result<ApiResponse> {
        let url = if path.starts_with("https://") || path.starts_with("http://") {
            let parsed = Url::parse(path).map_err(|_| anyhow!("invalid pagination cursor"))?;
            if parsed.origin() != self.origin.origin()
                || parsed.username() != ""
                || parsed.password().is_some()
                || parsed.fragment().is_some()
                || !parsed.path().starts_with("/v1/") && !parsed.path().starts_with("/v2/")
            {
                bail!("pagination cursor must target this App Store Connect API origin")
            }
            parsed
        } else {
            if !path.starts_with('/')
                || path.starts_with("//")
                || path.split('/').any(|s| s == ".." || s == ".")
            {
                bail!("invalid API path")
            }
            self.origin.join(path)?
        };
        let method =
            Method::from_bytes(method.as_bytes()).map_err(|_| anyhow!("invalid HTTP method"))?;
        let attempts = if method == Method::GET { 3 } else { 1 };
        let mut secrets = body.as_ref().map(sensitive_values).unwrap_or_default();
        let token = self.token()?;
        secrets.push(token.clone());
        for attempt in 0..attempts {
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token)
                .header("Accept", "application/json")
                .header("User-Agent", "asc-mcp-rust/0.2.0")
                .query(&query);
            if let Some(value) = &body {
                request = request.json(value)
            }
            let response = request.send().await;
            let mut response = match response {
                Ok(r) => r,
                Err(_) => {
                    if attempt + 1 < attempts {
                        tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                        continue;
                    }
                    bail!(
                        "App Store Connect transport failed; no automatic write retry was performed"
                    )
                }
            };
            let status = response.status().as_u16();
            if (status == 429 || status >= 500) && attempt + 1 < attempts {
                tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                continue;
            }
            if !(200..300).contains(&status) {
                let media_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if !(media_type == "application/json" || media_type.ends_with("+json"))
                    || response
                        .content_length()
                        .is_some_and(|n| n > MAX_ERROR_BODY as u64)
                {
                    return Err(ApiError::status_only(status).into());
                }
                let mut bytes = Vec::new();
                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) if chunk.len() <= MAX_ERROR_BODY - bytes.len() => {
                            bytes.extend_from_slice(&chunk)
                        }
                        Ok(None) => break,
                        _ => return Err(ApiError::status_only(status).into()),
                    }
                }
                return Err(ApiError::from_body(status, &bytes, &secrets).into());
            }
            let headers = response
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
                .collect();
            if response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE as u64)
            {
                bail!("API response exceeds size limit")
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow!("unable to read API response"))?
            {
                if chunk.len() > MAX_RESPONSE - bytes.len() {
                    bail!("API response exceeds size limit")
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(ApiResponse {
                status,
                headers,
                body: bytes,
            });
        }
        bail!("API request failed")
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        query: Vec<(String, String)>,
        body: Option<Value>,
    ) -> Result<Value> {
        let response = self.raw_request(method, path, query, body).await?;
        if response.body.is_empty() {
            return Ok(json!({"status":"success"}));
        }
        serde_json::from_slice(&response.body).map_err(|_| anyhow!("API response is not JSON"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use p256::{SecretKey, pkcs8::EncodePrivateKey};
    use rand_core::OsRng;
    #[test]
    fn jwt_has_bounded_lifetime_and_is_cached() {
        let key = SecretKey::random(&mut OsRng)
            .to_pkcs8_pem(Default::default())
            .unwrap();
        let client = ApiClient::with_key("test-issuer", "test-key", key.as_bytes()).unwrap();
        let first = client.token().unwrap();
        assert_eq!(first, client.token().unwrap());
        use base64::Engine;
        let payload: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(first.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            payload["exp"].as_u64().unwrap() - payload["iat"].as_u64().unwrap(),
            900
        );
        assert_eq!(payload["aud"], "appstoreconnect-v1");
    }
    #[tokio::test]
    async fn rejects_cross_origin_cursor_without_network() {
        let c = ApiClient::with_endpoint("http://127.0.0.1:1").unwrap();
        assert!(
            c.request("GET", "https://example.com/v1/apps", vec![], None)
                .await
                .unwrap_err()
                .to_string()
                .contains("origin")
        );
    }
}
