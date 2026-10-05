//! Interactive, browser-free Apple ID authentication. Never called by MCP tools.
//! Protocol arithmetic follows RFC 5054 with Apple's s2k password derivation
//! and proof encoding (as documented by fastlane/fastlane-sirp).
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chrono::Utc;
use num_bigint::{BigUint, RandBigInt};
use rand::rngs::OsRng;
use reqwest::{Client, Method, StatusCode, cookie::Jar, header::HeaderMap};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;
use zeroize::Zeroizing;

const ASC: &str = "https://appstoreconnect.apple.com";
const AUTH: &str = "https://idmsa.apple.com/appleauth/auth";
const N_HEX: &str = concat!(
    "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050",
    "A37329CBB4A099ED8193E0757767A13DD52312AB4B03310DCD7F48A9DA04FD50",
    "E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE82918A9962F0B93B8",
    "55F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773B",
    "CA97B43A23FB801676BD207A436C6481F1D2B9078717461A5B9D32E688F87748",
    "544523B524B0D57D5EA77A2775D2ECFA032CFBDBF52FB3786160279004E57AE6",
    "AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8E9DBFBB6",
    "94B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F9E4AFF73"
);
fn hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().to_vec()
}
fn pad(n: &BigUint) -> Vec<u8> {
    let bytes = n.to_bytes_be();
    let mut result = vec![0; 256 - bytes.len()];
    result.extend(bytes);
    result
}
fn modulus() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).unwrap()
}
struct Srp {
    a: BigUint,
    public: BigUint,
}
impl Srp {
    fn new() -> Self {
        Self::with_secret(OsRng.gen_biguint(2048))
    }
    fn with_secret(a: BigUint) -> Self {
        let public = BigUint::from(2u8).modpow(&a, &modulus());
        Self { a, public }
    }
    fn proofs(
        &self,
        email: &str,
        password: &[u8],
        salt: &[u8],
        server: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let n = modulus();
        let g = BigUint::from(2u8);
        let b = BigUint::from_bytes_be(server);
        ensure!(
            !salt.is_empty()
                && salt.len() <= 1024
                && !server.is_empty()
                && server.len() <= 256
                && b < n
                && &b % &n != BigUint::from(0u8),
            "apple_login_invalid_srp_challenge"
        );
        let k = BigUint::from_bytes_be(&hash(&[&pad(&n), &pad(&g)]));
        let u = BigUint::from_bytes_be(&hash(&[&pad(&self.public), &pad(&b)]));
        ensure!(u != BigUint::from(0u8), "apple_login_invalid_srp_challenge");
        let inner = Zeroizing::new(hash(&[b":", password]));
        let x = BigUint::from_bytes_be(&hash(&[salt, &inner]));
        let kv = (&k * g.modpow(&x, &n)) % &n;
        let base = (&b + &n - kv) % &n;
        let s = base.modpow(&(&self.a + &u * &x), &n);
        // fastlane-sirp hashes the minimal, even-byte representation of S/A/B.
        let key = Zeroizing::new(hash(&[&s.to_bytes_be()]));
        let hn = BigUint::from_bytes_be(&hash(&[&pad(&n)]));
        let hg = BigUint::from_bytes_be(&hash(&[&pad(&g)]));
        let xor = (hn ^ hg).to_bytes_be();
        let a_bytes = self.public.to_bytes_be();
        let m1 = hash(&[
            &xor,
            &hash(&[email.as_bytes()]),
            salt,
            &a_bytes,
            server,
            &key,
        ]);
        let m2 = BigUint::from_bytes_be(&hash(&[&a_bytes, &m1, &key])).to_bytes_be();
        Ok((m1, m2))
    }
}
fn derive_password(
    password: &str,
    salt: &[u8],
    iterations: u32,
    protocol: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        (1..=1_000_000).contains(&iterations),
        "apple_login_invalid_iteration_count"
    );
    let digest = Zeroizing::new(hash(&[password.as_bytes()]));
    let input = match protocol {
        "s2k" => Zeroizing::new(digest.to_vec()),
        "s2k_fo" => Zeroizing::new(hex::encode(&*digest).into_bytes()),
        _ => bail!("apple_login_unsupported_srp_protocol"),
    };
    let mut output = Zeroizing::new(vec![0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(&input, salt, iterations, &mut output);
    Ok(output)
}
fn hashcash(bits: u32, challenge: &str, date: &str) -> Result<String> {
    ensure!(
        bits <= 24 && challenge.len() <= 1024 && !challenge.contains(['\r', '\n', ':']),
        "apple_login_invalid_hashcash"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    for counter in 0..100_000_000u32 {
        if counter % 1024 == 0 && Instant::now() >= deadline {
            break;
        }
        let candidate = format!("1:{bits}:{date}:{challenge}::{counter}");
        let digest = sha1::Sha1::digest(candidate.as_bytes());
        let zeros: u32 = digest.iter().take_while(|b| **b == 0).count() as u32 * 8;
        let leading = zeros
            + digest
                .get((zeros / 8) as usize)
                .map_or(0, |b| b.leading_zeros());
        if leading >= bits {
            return Ok(candidate);
        }
    }
    bail!("apple_login_hashcash_work_limit")
}

struct AuthClient {
    client: Client,
    cookies: BTreeMap<(String, String, String), Value>,
    widget: String,
    session: Option<String>,
    scnt: Option<String>,
}
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}
impl Reply {
    fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body)
            .map_err(|_| anyhow::anyhow!("apple_login_unexpected_response"))
    }
    fn success(&self) -> Result<()> {
        ensure!(
            self.status.is_success(),
            "apple_login_http_{}",
            self.status.as_u16()
        );
        if let Ok(body) = serde_json::from_slice::<Value>(&self.body) {
            let errors = [
                "serviceErrors",
                "service_errors",
                "validationErrors",
                "errors",
            ];
            ensure!(
                body["hasError"].as_bool() != Some(true)
                    && !errors.iter().any(|key| body[*key]
                        .as_array()
                        .is_some_and(|values| !values.is_empty())),
                "apple_login_authentication_rejected"
            );
        }
        Ok(())
    }
}
fn allowed_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some("idmsa.apple.com" | "appstoreconnect.apple.com")
        )
}
impl AuthClient {
    fn new() -> Result<Self> {
        let client = Client::builder()
            .cookie_provider(Arc::new(Jar::default()))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(60))
            .user_agent("asc-mcp/0.2 AppleID CLI")
            .build()?;
        Ok(Self {
            client,
            cookies: BTreeMap::new(),
            widget: String::new(),
            session: None,
            scnt: None,
        })
    }
    async fn request(
        &mut self,
        method: Method,
        endpoint: &str,
        body: Option<&Value>,
        hc: Option<&str>,
    ) -> Result<Reply> {
        let url =
            Url::parse(endpoint).map_err(|_| anyhow::anyhow!("apple_login_invalid_endpoint"))?;
        ensure!(allowed_url(&url), "apple_login_untrusted_endpoint");
        let mut request = self
            .client
            .request(method, url.clone())
            .header("Accept", "application/json");
        if url.host_str() == Some("idmsa.apple.com") {
            request = request
                .header("X-Apple-Widget-Key", &self.widget)
                .header("X-Requested-With", "XMLHttpRequest");
            if let Some(value) = &self.session {
                request = request.header("X-Apple-Id-Session-Id", value);
            }
            if let Some(value) = &self.scnt {
                request = request.header("scnt", value);
            }
            if let Some(value) = hc {
                request = request.header("X-Apple-HC", value);
            }
        }
        if let Some(value) = body {
            request = request.json(value);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("apple_login_network_failed"))?;
        let headers = response.headers().clone();
        let status = response.status();
        for (key, dest) in [
            ("scnt", &mut self.scnt),
            ("x-apple-id-session-id", &mut self.session),
        ] {
            if url.host_str() == Some("idmsa.apple.com")
                && let Some(value) = headers.get(key).and_then(|h| h.to_str().ok())
            {
                *dest = Some(value.to_owned());
            }
        }
        for raw in headers.get_all("set-cookie") {
            if let Ok(raw) = raw.to_str() {
                self.record_cookie(raw, &url);
            }
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("apple_login_network_failed"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 1024 * 1024,
                "apple_login_response_too_large"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Reply {
            status,
            headers,
            body: bytes,
        })
    }
    fn record_cookie(&mut self, raw: &str, source: &Url) {
        let Ok(c) = cookie::Cookie::parse(raw.to_owned()) else {
            return;
        };
        let domain = c
            .domain()
            .unwrap_or(source.host_str().unwrap_or_default())
            .trim_start_matches('.');
        // Persist only cookies that actually apply to ASC. IDMSA-only cookies stay in the live Jar.
        if !matches!(domain, "apple.com" | "appstoreconnect.apple.com") {
            return;
        }
        if domain == "appstoreconnect.apple.com" && source.host_str() != Some(domain) {
            return;
        }
        let path = c.path().unwrap_or("/");
        let key = (domain.to_owned(), path.to_owned(), c.name().to_owned());
        let expires = c
            .max_age()
            .map(|age| Utc::now().timestamp().saturating_add(age.whole_seconds()))
            .or_else(|| c.expires_datetime().map(|d| d.unix_timestamp()));
        if expires.is_some_and(|e| e <= Utc::now().timestamp()) || c.value().is_empty() {
            self.cookies.remove(&key);
            return;
        }
        let expires = expires
            .and_then(|e| chrono::DateTime::from_timestamp(e, 0))
            .map(|d| d.to_rfc3339());
        self.cookies.insert(key, json!({"name":c.name(),"value":c.value(),"domain":domain,"path":path,"expires":expires}));
    }
    async fn widget_key(&mut self) -> Result<()> {
        // A fresh, empty cookie jar: HEAD /logout discovers the public widget key without logging anyone out.
        let response = self
            .request(Method::HEAD, &format!("{ASC}/logout"), None, None)
            .await?;
        if let Some(location) = response
            .headers
            .get("location")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Url::parse(v).ok())
            && allowed_url(&location)
            && location.host_str() == Some("idmsa.apple.com")
            && let Some((_, key)) = location.query_pairs().find(|(k, _)| k == "widgetKey")
        {
            self.widget = key.into_owned();
        }
        if self.widget.is_empty() {
            let response = self
                .request(
                    Method::GET,
                    &format!("{ASC}/olympus/v1/app/config?hostname=itunesconnect.apple.com"),
                    None,
                    None,
                )
                .await?;
            response.success()?;
            self.widget = response
                .json()?
                .get("authServiceKey")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        ensure!(
            !self.widget.is_empty() && self.widget.len() <= 4096,
            "apple_login_missing_widget_key"
        );
        Ok(())
    }
    async fn two_factor(&mut self) -> Result<()> {
        ensure!(
            self.session.is_some() && self.scnt.is_some(),
            "apple_login_missing_2fa_headers"
        );
        let response = self.request(Method::GET, AUTH, None, None).await?;
        response.success()?;
        let options = response.json()?;
        if let Some(devices) = options["trustedDevices"].as_array() {
            ensure!(
                !devices.is_empty() && devices.len() <= 100,
                "apple_login_no_trusted_device"
            );
            for (i, device) in devices.iter().enumerate() {
                eprintln!(
                    "{}: {}",
                    i + 1,
                    device["name"].as_str().unwrap_or("trusted device")
                );
            }
            let index: usize = read_line("Select trusted device: ")?
                .parse()
                .map_err(|_| anyhow::anyhow!("apple_login_invalid_device_choice"))?;
            let device = devices
                .get(index.wrapping_sub(1))
                .ok_or_else(|| anyhow::anyhow!("apple_login_invalid_device_choice"))?;
            let id = device
                .get("id")
                .filter(|v| v.is_number() || v.is_string())
                .ok_or_else(|| anyhow::anyhow!("apple_login_invalid_device_id"))?;
            let id_path = id
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| id.to_string());
            ensure!(
                !id_path.is_empty()
                    && id_path.len() <= 128
                    && id_path
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "apple_login_invalid_device_id"
            );
            self.request(
                Method::PUT,
                &format!("{AUTH}/verify/device/{id_path}/securitycode"),
                None,
                None,
            )
            .await?
            .success()?;
            let code = read_secret("Enter the four digit verification code: ")?;
            validate_code(&code, 4)?;
            let body = json!({"phoneNumber":{"id":id},"securityCode":{"code":&*code},"mode":"sms"});
            self.request(
                Method::POST,
                &format!("{AUTH}/verify/phone/securitycode"),
                Some(&body),
                None,
            )
            .await?
            .success()?;
            self.request(Method::GET, &format!("{AUTH}/2sv/trust"), None, None)
                .await?
                .success()?;
            return Ok(());
        }
        ensure!(
            !options["securityCode"]["securityCodeLocked"]
                .as_bool()
                .unwrap_or(false),
            "apple_login_2fa_locked"
        );
        let length = options["securityCode"]["length"].as_u64().unwrap_or(6) as usize;
        ensure!((4..=8).contains(&length), "apple_login_invalid_code_length");
        let mut code = if options["noTrustedDevices"].as_bool() == Some(true) {
            Zeroizing::new("sms".to_owned())
        } else {
            read_secret("Enter the verification code from your trusted device (or type sms): ")?
        };
        let (kind, body) =
            if code.as_str() == "sms" || options["noTrustedDevices"].as_bool() == Some(true) {
                let phones = options["trustedPhoneNumbers"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("apple_login_no_trusted_phone"))?;
                ensure!(
                    !phones.is_empty() && phones.len() <= 100,
                    "apple_login_no_trusted_phone"
                );
                for (i, phone) in phones.iter().enumerate() {
                    eprintln!(
                        "{}: {}",
                        i + 1,
                        phone["numberWithDialCode"]
                            .as_str()
                            .unwrap_or("trusted phone")
                    );
                }
                let index: usize = read_line("Select phone number: ")?
                    .parse()
                    .map_err(|_| anyhow::anyhow!("apple_login_invalid_phone_choice"))?;
                let phone = phones
                    .get(index.wrapping_sub(1))
                    .ok_or_else(|| anyhow::anyhow!("apple_login_invalid_phone_choice"))?;
                let id = phone
                    .get("id")
                    .filter(|v| v.is_number())
                    .ok_or_else(|| anyhow::anyhow!("apple_login_invalid_phone_id"))?;
                let mode = match phone["pushMode"].as_str() {
                    Some("voice") => "voice",
                    _ => "sms",
                };
                let body = json!({"phoneNumber":{"id":id},"mode":mode});
                if !(phones.len() == 1 && options["noTrustedDevices"].as_bool() == Some(true)) {
                    self.request(
                        Method::PUT,
                        &format!("{AUTH}/verify/phone"),
                        Some(&body),
                        None,
                    )
                    .await?
                    .success()?;
                }
                code = read_secret("Enter the verification code: ")?;
                (
                    "phone",
                    json!({"phoneNumber":{"id":id},"mode":mode,"securityCode":{"code":&*code}}),
                )
            } else {
                ("trusteddevice", json!({"securityCode":{"code":&*code}}))
            };
        validate_code(&code, length)?;
        self.request(
            Method::POST,
            &format!("{AUTH}/verify/{kind}/securitycode"),
            Some(&body),
            None,
        )
        .await?
        .success()?;
        self.request(Method::GET, &format!("{AUTH}/2sv/trust"), None, None)
            .await?
            .success()?;
        Ok(())
    }
}
fn validate_code(code: &str, length: usize) -> Result<()> {
    ensure!(
        (4..=8).contains(&length)
            && code.len() == length
            && code.bytes().all(|b| b.is_ascii_digit()),
        "apple_login_invalid_verification_code"
    );
    Ok(())
}
fn read_secret(prompt: &str) -> Result<Zeroizing<String>> {
    let input = Zeroizing::new(
        rpassword::prompt_password(prompt)
            .map_err(|_| anyhow::anyhow!("apple_login_secret_prompt_failed"))?,
    );
    ensure!(input.len() <= 4096, "apple_login_input_too_long");
    Ok(input)
}
fn read_line(prompt: &str) -> Result<Zeroizing<String>> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut input = Zeroizing::new(String::new());
    io::stdin().read_line(&mut input)?;
    ensure!(input.len() <= 4096, "apple_login_input_too_long");
    Ok(Zeroizing::new(input.trim().to_owned()))
}
/// Generates a private reviewer session. Requires a human terminal and never reads cached secrets.
pub async fn login(email: &str) -> Result<PathBuf> {
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "apple_login_interactive_terminal_required"
    );
    ensure!(
        !email.trim().is_empty() && email.len() <= 320 && !email.chars().any(char::is_control),
        "apple_login_invalid_email"
    );
    let mut auth = AuthClient::new()?;
    auth.widget_key().await?;
    let password = Zeroizing::new(
        rpassword::prompt_password("Apple ID password: ")
            .map_err(|_| anyhow::anyhow!("apple_login_password_prompt_failed"))?,
    );
    ensure!(!password.is_empty(), "apple_login_password_required");
    let srp = Srp::new();
    let start = json!({"a":B64.encode(srp.public.to_bytes_be()),"accountName":email,"protocols":["s2k","s2k_fo"]});
    let response = auth
        .request(
            Method::POST,
            &format!("{AUTH}/signin/init"),
            Some(&start),
            None,
        )
        .await?;
    response.success()?;
    let challenge = response.json()?;
    let salt = B64
        .decode(challenge["salt"].as_str().unwrap_or_default())
        .map_err(|_| anyhow::anyhow!("apple_login_invalid_salt"))?;
    let server = B64
        .decode(challenge["b"].as_str().unwrap_or_default())
        .map_err(|_| anyhow::anyhow!("apple_login_invalid_srp_challenge"))?;
    let iterations = challenge["iteration"]
        .as_u64()
        .and_then(|i| u32::try_from(i).ok())
        .ok_or_else(|| anyhow::anyhow!("apple_login_invalid_iteration_count"))?;
    let encrypted = derive_password(
        &password,
        &salt,
        iterations,
        challenge["protocol"].as_str().unwrap_or_default(),
    )?;
    drop(password);
    let (m1, m2) = srp.proofs(email, &encrypted, &salt, &server)?;
    drop(encrypted);
    let c = challenge["c"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("apple_login_missing_challenge"))?;
    let cash_response = auth
        .request(
            Method::GET,
            &format!(
                "{AUTH}/signin?widgetKey={}",
                percent_encoding::utf8_percent_encode(
                    &auth.widget,
                    percent_encoding::NON_ALPHANUMERIC
                )
            ),
            None,
            None,
        )
        .await?;
    let hc = match (
        cash_response.headers.get("x-apple-hc-bits"),
        cash_response.headers.get("x-apple-hc-challenge"),
    ) {
        (Some(bits), Some(resource)) => {
            let bits = bits
                .to_str()
                .ok()
                .and_then(|b| b.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("apple_login_invalid_hashcash"))?;
            let resource = resource
                .to_str()
                .map_err(|_| anyhow::anyhow!("apple_login_invalid_hashcash"))?;
            Some(hashcash(
                bits,
                resource,
                &Utc::now().format("%Y%m%d%H%M%S").to_string(),
            )?)
        }
        (None, None) => None,
        _ => bail!("apple_login_invalid_hashcash"),
    };
    let complete = json!({"accountName":email,"c":c,"m1":B64.encode(m1),"m2":B64.encode(m2),"rememberMe":false});
    let response = auth
        .request(
            Method::POST,
            &format!("{AUTH}/signin/complete?isRememberMeEnabled=false"),
            Some(&complete),
            hc.as_deref(),
        )
        .await?;
    match response.status.as_u16() {
        200 => (),
        409 => auth.two_factor().await?,
        401 | 403 => bail!("apple_login_credentials_rejected"),
        412 => bail!(
            "apple_login_account_action_required: acknowledge Apple's terms or account requirements on Apple's website, then retry"
        ),
        code => bail!("apple_login_http_{code}"),
    }
    let session = auth
        .request(
            Method::GET,
            &format!("{ASC}/olympus/v1/session"),
            None,
            None,
        )
        .await?;
    session.success()?;
    let session_body = session.json()?;
    ensure!(
        session_body.get("user").is_some_and(Value::is_object)
            || session_body.get("provider").is_some_and(Value::is_object),
        "apple_login_session_not_confirmed"
    );
    crate::reviewer::save_login_session(
        email,
        json!(auth.cookies.into_values().collect::<Vec<_>>()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apple_password_derivation_fixtures_and_protocol_bounds() {
        let salt = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(
            hex::encode(&*derive_password("fixture-password", &salt, 1000, "s2k").unwrap()),
            "38993851e6adbee1a62166222d3d98d7bc3b31850c4e41d12cc1cbc909459120"
        );
        assert_eq!(
            hex::encode(&*derive_password("fixture-password", &salt, 1000, "s2k_fo").unwrap()),
            "de7da795f769747c2045fa4ed67ca1c053e1d1156dc365d1c88b32efa59c8ddf"
        );
        assert!(derive_password("p", &salt, 0, "s2k").is_err());
        assert!(derive_password("p", &salt, 1_000_001, "s2k").is_err());
        assert!(derive_password("p", &salt, 1, "unknown").is_err());
    }
    // Independently computed with Python hashlib PBKDF2 and server-side SRP S=(A*v^u)^b.
    // Fixture inputs are synthetic. Encoding matches fastlane-sirp 1.1.0's 2048/SHA256 group.
    #[test]
    fn apple_srp_transcript_fixture() {
        let srp = Srp::with_secret(BigUint::parse_bytes("1f".repeat(32).as_bytes(), 16).unwrap());
        let salt = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        let password = derive_password("fixture-password", &salt, 1000, "s2k").unwrap();
        let server = hex::decode(concat!(
            "92bc25f5a886c54f4b8feab86a4b4dc67c0b7970c05c86ba40b2fdb565d92b29",
            "5cce11d6286b836c1fabce4d64a3a304f2c50b9a7f92be793540c53269d57412",
            "f9ff660f1f571a5500a9e98a5ca8fd74bd667bcfb29ed4829705174eeb0aeb01",
            "1be9fc74829d96338327e614a8e1ef0d6f162cb3cdce06db810255f1ba579687",
            "2f40eccbcf35c3a2c36421e66054bd8a885a3d7b8dcfb7359b9e5644b365e53",
            "f335ea0f17b5b5ef86efe77a928083d524db38575e12ed38aee3317cbb9ae46f",
            "9b174cfb3a345c37f28f0be59268a540a97f9eb934a8f111c81cf16b14fecff7",
            "d09b7290a838f16623bcd8ec74053b324fc19de8bc2e362a7c15d1941014b0e08"
        ))
        .unwrap();
        let (m1, m2) = srp
            .proofs("fixture@example.invalid", &password, &salt, &server)
            .unwrap();
        assert_eq!(
            hex::encode(m1),
            "cccf5f92c09cdd86734dbf52d0d681f12f11d53b990b982b478007b7f463363c"
        );
        assert_eq!(
            hex::encode(m2),
            "d5a56a8b94ca7f955750f1740858daf5b3a73b2ca8ed5fa86a7260c9bc23bdd8"
        );
        for invalid in [vec![0], modulus().to_bytes_be(), vec![1; 257]] {
            assert!(
                srp.proofs("fixture@example.invalid", &password, &salt, &invalid)
                    .is_err()
            );
        }
        assert!(
            srp.proofs("fixture@example.invalid", &password, &[], &server)
                .is_err()
        );
    }
    #[test]
    fn bounded_hashcash_matches_apple_wire_format() {
        let stamp = hashcash(12, "fixture-challenge", "20261005123456").unwrap();
        assert!(stamp.starts_with("1:12:20261005123456:fixture-challenge::"));
        let hash = sha1::Sha1::digest(stamp.as_bytes());
        assert_eq!(hash[0], 0);
        assert!(hash[1] < 16);
        assert!(hashcash(25, "x", "20261005123456").is_err());
        assert!(hashcash(1, "header\r\ninjection", "20261005123456").is_err());
    }
    #[test]
    fn origin_and_code_validation() {
        for endpoint in [
            "https://idmsa.apple.com/appleauth/auth",
            "https://appstoreconnect.apple.com/olympus/v1/session",
        ] {
            assert!(allowed_url(&Url::parse(endpoint).unwrap()));
        }
        for endpoint in [
            "http://idmsa.apple.com/",
            "https://idmsa.apple.com.evil.invalid/",
            "https://user@idmsa.apple.com/",
            "https://idmsa.apple.com:444/",
            "https://evil.invalid/",
        ] {
            assert!(!allowed_url(&Url::parse(endpoint).unwrap()));
        }
        assert!(validate_code("123456", 6).is_ok());
        assert!(validate_code("1234", 4).is_ok());
        for code in ["12345", "12345a", "１２３４５６", "123456\n"] {
            assert!(validate_code(code, 6).is_err());
        }
    }
    #[test]
    fn two_factor_response_errors_are_rejected_without_echoing_body() {
        for body in [
            json!({"service_errors":[{"code":"-21669","message":"synthetic-secret"}]}),
            json!({"hasError":true}),
            json!({"validationErrors":[{"code":"bad"}]}),
        ] {
            let reply = Reply {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
            };
            let message = reply.success().unwrap_err().to_string();
            assert_eq!(message, "apple_login_authentication_rejected");
            assert!(!message.contains("synthetic-secret"));
        }
        for status in [StatusCode::OK, StatusCode::NO_CONTENT] {
            assert!(
                Reply {
                    status,
                    headers: HeaderMap::new(),
                    body: vec![]
                }
                .success()
                .is_ok()
            );
        }
        assert!(
            Reply {
                status: StatusCode::UNAUTHORIZED,
                headers: HeaderMap::new(),
                body: vec![]
            }
            .success()
            .is_err()
        );
    }
    #[test]
    fn cookie_scope_replacement_and_deletion() {
        let mut auth = AuthClient::new().unwrap();
        let idmsa = Url::parse(AUTH).unwrap();
        let asc = Url::parse(ASC).unwrap();
        auth.record_cookie(
            "session=synthetic; Domain=.apple.com; Path=/; Secure; HttpOnly",
            &idmsa,
        );
        auth.record_cookie("auth=synthetic; Path=/", &idmsa);
        auth.record_cookie(
            "bad=synthetic; Domain=appstoreconnect.apple.com; Path=/",
            &idmsa,
        );
        assert_eq!(auth.cookies.len(), 1);
        auth.record_cookie("session=replaced; Domain=.apple.com; Path=/", &asc);
        assert_eq!(auth.cookies.values().next().unwrap()["value"], "replaced");
        auth.record_cookie("session=gone; Domain=.apple.com; Path=/; Max-Age=0", &asc);
        assert!(auth.cookies.is_empty());
    }
}
