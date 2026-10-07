//! Interactive, browser-free Apple ID authentication. Never called by MCP tools.
//! Protocol arithmetic follows RFC 5054 with Apple's s2k password derivation
//! and proof encoding (as documented by fastlane/fastlane-sirp).
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use chrono::Utc;
use num_bigint::{BigUint, RandBigInt};
use rand::rngs::OsRng;
use reqwest::{
    Client, Method, StatusCode,
    cookie::{CookieStore, Jar},
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;
use zeroize::Zeroizing;

mod credentials;
mod trust;
use trust::TrustCookie;

const ASC: &str = "https://appstoreconnect.apple.com";
const ASC_HOST: &str = "appstoreconnect.apple.com";
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
    jar: Arc<Jar>,
    /// Every valid cookie observed or seeded this run, with scope metadata. ASC cookies are
    /// selected from it for the reviewer session; IDMSA-origin ones form the private login trust.
    cookies: BTreeMap<(String, String, String), Value>,
    seeded: HashSet<(String, String, String)>,
    widget: String,
    session: Option<String>,
    scnt: Option<String>,
    #[cfg(test)]
    test_origin: Option<String>,
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
        let jar = Arc::new(Jar::default());
        let client = Client::builder()
            .cookie_provider(jar.clone())
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(60))
            .user_agent("asc-mcp/0.2 AppleID CLI")
            .build()?;
        Ok(Self {
            client,
            jar,
            cookies: BTreeMap::new(),
            seeded: HashSet::new(),
            widget: String::new(),
            session: None,
            scnt: None,
            #[cfg(test)]
            test_origin: None,
        })
    }
    /// Authentication client seeded with preserved trust. Never used for widget discovery.
    fn with_trust(widget: String, seed: Vec<TrustCookie>) -> Result<Self> {
        let mut auth = Self::new()?;
        auth.widget = widget;
        for c in seed {
            let key = (c.domain.clone(), c.path.clone(), c.name.clone());
            auth.seeded.insert(key.clone());
            auth.cookies.insert(key, serde_json::to_value(&c)?);
        }
        Ok(auth)
    }
    /// IDMSA-origin cookies worth preserving across logins.
    fn trust_cookies(&self) -> Vec<TrustCookie> {
        self.cookies
            .values()
            .filter_map(|v| serde_json::from_value::<TrustCookie>(v.clone()).ok())
            .filter(|c| c.valid() && c.unexpired())
            .collect()
    }
    /// A single URL-selected Cookie header for IDMSA requests, or None to leave the jar alone.
    /// Preserved and freshly recorded cookies are selected; jar cookies are merged in so reqwest's own jar header
    /// (which an explicit Cookie header suppresses) is never duplicated.
    fn idmsa_cookie(&self, url: &Url) -> Option<HeaderValue> {
        let jar = self
            .jar
            .cookies(url)
            .and_then(|v| v.to_str().ok().map(str::to_owned));
        if url.host_str() != Some(trust::IDMSA_HOST) {
            return None;
        }
        let rendered = trust::cookie_header(&self.trust_cookies(), url, jar.as_deref());
        let mut value = match rendered {
            Some(v) => HeaderValue::from_str(&v).ok()?,
            None if jar.is_some() => HeaderValue::from_static(""),
            None => return None,
        };
        value.set_sensitive(true);
        Some(value)
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
        #[cfg(test)]
        let transport = match &self.test_origin {
            Some(origin) => Url::parse(&format!(
                "{}{}{}",
                origin,
                url.path(),
                url.query().map(|q| format!("?{q}")).unwrap_or_default()
            ))?,
            None => url.clone(),
        };
        #[cfg(not(test))]
        let transport = url.clone();
        let mut request = self
            .client
            .request(method, transport)
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
            if let Some(value) = self.idmsa_cookie(&url) {
                request = request.header(reqwest::header::COOKIE, value);
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
                #[cfg(test)]
                if self.test_origin.is_some() {
                    self.jar.add_cookie_str(raw, &url);
                }
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
        if c.name().starts_with("__Host-")
            && (c.domain().is_some() || c.path() != Some("/") || c.secure() != Some(true))
        {
            return;
        }
        if c.name().starts_with("__Secure-") && c.secure() != Some(true) {
            return;
        }
        let origin = source.host_str().unwrap_or_default().to_ascii_lowercase();
        let domain = c
            .domain()
            .unwrap_or(&origin)
            .trim_start_matches('.')
            .to_ascii_lowercase();
        let scoped = domain == "apple.com" || domain == origin;
        if !matches!(origin.as_str(), trust::IDMSA_HOST | ASC_HOST) || !scoped {
            return;
        }
        let default_path = source
            .path()
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/");
        let path = c
            .path()
            .filter(|p| p.starts_with('/'))
            .unwrap_or(default_path)
            .to_owned();
        let value = c.value_trimmed().to_owned();
        let deleting = value.is_empty() || c.max_age().is_some_and(|a| a.whole_seconds() <= 0);
        let stamp = c
            .max_age()
            .filter(|_| !deleting)
            .map(|a| Utc::now().timestamp().saturating_add(a.whole_seconds()))
            .or_else(|| c.expires_datetime().map(|d| d.unix_timestamp()));
        let expires = match stamp {
            Some(t) if !deleting => {
                let Some(d) = chrono::DateTime::from_timestamp(t, 0) else {
                    return;
                };
                Some(d.to_rfc3339())
            }
            _ => None,
        };
        let mut record = TrustCookie {
            name: c.name().to_owned(),
            value: if deleting { "deleted".into() } else { value },
            origin_host: origin,
            domain,
            host_only: c.domain().is_none(),
            path,
            secure: c.secure().unwrap_or(false),
            expires,
        };
        if !record.valid_response() {
            return;
        }
        let key = (
            record.domain.clone(),
            record.path.clone(),
            record.name.clone(),
        );
        self.seeded.remove(&key);
        if deleting || !record.unexpired() {
            self.cookies.remove(&key);
            return;
        }
        record.value = c.value_trimmed().to_owned();
        if let Ok(v) = serde_json::to_value(&record) {
            self.cookies.insert(key, v);
        }
    }
    /// Cookies for the reviewer session: applicable to ASC and not merely seeded trust.
    fn asc_cookies(&self) -> Vec<Value> {
        self.cookies
            .iter()
            .filter(|(key, value)| {
                !self.seeded.contains(*key)
                    && (key.0 == "apple.com"
                        || (key.0 == ASC_HOST && value["origin_host"] == ASC_HOST))
            })
            .map(|(_, v)| {
                json!({"name":v["name"],"value":v["value"],"domain":v["domain"],"path":v["path"],"expires":v["expires"]})
            })
            .collect()
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
/// Apple's Olympus identity must be the requested account. A provider alone proves nothing.
fn identity_matches(body: &Value, email: &str) -> bool {
    body["user"]["emailAddress"]
        .as_str()
        .is_some_and(|actual| actual.trim().to_lowercase() == email.trim().to_lowercase())
}
enum Preflight {
    Reused(u64),
    Fresh,
}
/// Check the existing effective session against the live origin. Fails closed on anything other
/// than confirmed 200 reuse or a definite 401 / identity mismatch / missing identity.
async fn preflight(
    origin: &str,
    email: &str,
    existing: &crate::reviewer::Reusable,
    trust_root: &Path,
    policy: ChallengePolicy,
) -> Result<Preflight> {
    let url = Url::parse(&format!("{origin}/olympus/v1/session"))
        .map_err(|_| anyhow::anyhow!("apple_login_invalid_endpoint"))?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut cookie = HeaderValue::from_str(existing.cookie_header())
        .map_err(|_| anyhow::anyhow!("apple_login_preflight_failed"))?;
    cookie.set_sensitive(true);
    let mut response = client
        .get(url.clone())
        .header("Accept", "application/json")
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("apple_login_preflight_failed"))?;
    match response.status().as_u16() {
        200 => (),
        401 => return Ok(Preflight::Fresh),
        _ => bail!("apple_login_preflight_failed"),
    }
    let headers = response.headers().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("apple_login_preflight_failed"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= 1024 * 1024,
            "apple_login_preflight_failed"
        );
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("apple_login_preflight_failed"))?;
    if !identity_matches(&body, email) {
        return Ok(Preflight::Fresh);
    }
    let provider = confirmed_provider(trust_root, email, &body, policy)?;
    existing
        .save_rotation(&headers, &url)
        .await
        .map_err(|_| anyhow::anyhow!("apple_login_session_save_failed"))?;
    Ok(Preflight::Reused(provider))
}
#[derive(Clone, Copy)]
enum ChallengePolicy {
    Interactive,
    InteractiveProviderOverride,
    Unattended,
}
impl ChallengePolicy {
    fn interactive(self) -> bool {
        !matches!(self, Self::Unattended)
    }
}
fn confirmed_provider(
    root: &Path,
    email: &str,
    body: &Value,
    policy: ChallengePolicy,
) -> Result<u64> {
    let provider = body["provider"]["providerId"].as_u64().filter(|id| *id > 0)
        .ok_or_else(|| anyhow::anyhow!("apple_login_provider_required: confirm an App Store Connect provider with native reviewer-login"))?;
    let expected = credentials::pinned_provider(root, email)?;
    if matches!(policy, ChallengePolicy::Unattended) && expected.is_none() {
        bail!(
            "apple_login_provider_required: run native reviewer-login once to establish the provider"
        );
    }
    if expected.is_some_and(|id| id != provider)
        && !matches!(policy, ChallengePolicy::InteractiveProviderOverride)
    {
        bail!(
            "apple_login_provider_changed: confirm the intended provider with reviewer-login --accept-provider-change"
        );
    }
    Ok(provider)
}
async fn accept_completion(
    auth: &mut AuthClient,
    status: StatusCode,
    policy: ChallengePolicy,
    challenge: impl std::ops::AsyncFnOnce(&mut AuthClient) -> Result<()>,
) -> Result<()> {
    match status.as_u16() {
        200 => Ok(()),
        409 if policy.interactive() => challenge(auth).await,
        409 => bail!(
            "apple_login_2fa_required: run native reviewer-login to complete Apple verification"
        ),
        401 | 403 => bail!("apple_login_credentials_rejected"),
        412 => bail!(
            "apple_login_account_action_required: acknowledge Apple's account requirements, then retry"
        ),
        code => bail!("apple_login_http_{code}"),
    }
}
fn finalize_confirmed(
    auth: &AuthClient,
    session: &Reply,
    email: &str,
    trust_root: &Path,
    source: &Path,
    policy: ChallengePolicy,
) -> Result<PathBuf> {
    finalize_with_publication(
        auth,
        session,
        email,
        trust_root,
        source,
        policy,
        crate::reviewer::publish_login_session,
    )
}
fn finalize_with_publication(
    auth: &AuthClient,
    session: &Reply,
    email: &str,
    trust_root: &Path,
    source: &Path,
    policy: ChallengePolicy,
    publish: impl FnOnce(&Path, &Value) -> Result<()>,
) -> Result<PathBuf> {
    session.success()?;
    let body = session.json()?;
    ensure!(
        identity_matches(&body, email),
        "apple_login_session_not_confirmed: use the primary Apple Account email"
    );
    let provider = confirmed_provider(trust_root, email, &body, policy)?;
    // Validate identity, provider and snapshots before writing; only confirmed authentication replaces them.
    let data = crate::reviewer::login_session_data(json!(auth.asc_cookies()))?;
    trust::save_to(trust_root, email, &auth.trust_cookies())?;
    credentials::remember_identity(trust_root, email, Some(provider))?;
    publish(source, &data)?;
    Ok(source.to_owned())
}
fn save_password_after_confirmation(
    confirmed: Result<PathBuf>,
    email: &str,
    password: &str,
    store: &impl credentials::SecretStore,
) -> Result<PathBuf> {
    let source = confirmed?;
    credentials::save_password(store, email, password)?;
    Ok(source)
}
/// Resolve the explicit primary account or its last confirmed private host-only value.
pub fn resolve_account(explicit: Option<&str>) -> Result<Option<String>> {
    credentials::resolve_from(&crate::reviewer::trust_root()?, explicit)
}

/// Generates or reuses a reviewer session with remembered account and native Keychain support.
pub async fn login(email: &str) -> Result<PathBuf> {
    login_with_options(email, false).await
}

pub async fn login_with_options(email: &str, fresh: bool) -> Result<PathBuf> {
    login_with_provider_override(email, fresh, false).await
}

pub async fn login_with_provider_override(
    email: &str,
    fresh: bool,
    allow_provider_change: bool,
) -> Result<PathBuf> {
    let policy = if allow_provider_change {
        ChallengePolicy::InteractiveProviderOverride
    } else {
        ChallengePolicy::Interactive
    };
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "apple_login_interactive_terminal_required"
    );
    let email = credentials::normalize_account(email)?;
    if !fresh && let Some(path) = reuse_confirmed(&email, policy).await? {
        return Ok(path);
    }
    let store = credentials::NativeStore;
    let password = if fresh {
        None
    } else {
        credentials::load_password(&store, &email, credentials::Access::Interactive)?
    };
    let password = match password {
        Some(password) => password,
        None => read_secret("Apple ID password: ")?,
    };
    login_with_password(&email, &password, policy, &store).await
}

/// Host-only automatic renewal. No terminal, password prompt or verification challenge is used.
pub async fn login_unattended(email: &str) -> Result<PathBuf> {
    let email = credentials::normalize_account(email)?;
    ensure!(
        credentials::pinned_provider(&crate::reviewer::trust_root()?, &email)?.is_some(),
        "apple_login_provider_required: run native reviewer-login once to establish the provider"
    );
    if let Some(path) = reuse_confirmed(&email, ChallengePolicy::Unattended).await? {
        return Ok(path);
    }
    let store = credentials::NativeStore;
    let password = credentials::load_password(&store, &email, credentials::Access::Unattended)?
        .ok_or_else(|| anyhow::anyhow!("apple_login_secret_unavailable"))?;
    login_with_password(&email, &password, ChallengePolicy::Unattended, &store).await
}

async fn reuse_confirmed(email: &str, policy: ChallengePolicy) -> Result<Option<PathBuf>> {
    let trust_root = crate::reviewer::trust_root()?;
    if let Some(existing) = crate::reviewer::Reusable::load().await?
        && let Preflight::Reused(provider) =
            preflight(ASC, email, &existing, &trust_root, policy).await?
    {
        credentials::remember_identity(&trust_root, email, Some(provider))?;
        return Ok(Some(existing.source_path().to_owned()));
    }
    Ok(None)
}

async fn login_with_password(
    email: &str,
    password: &str,
    policy: ChallengePolicy,
    store: &impl credentials::SecretStore,
) -> Result<PathBuf> {
    ensure!(
        !password.is_empty() && password.len() <= 4096,
        "apple_login_password_required"
    );
    let trust_root = crate::reviewer::trust_root()?;
    let seed = trust::load_from(&trust_root, email)?;
    // Widget discovery uses its own empty client; trust is never sent to /logout.
    let mut discovery = AuthClient::new()?;
    discovery.widget_key().await?;
    let widget = std::mem::take(&mut discovery.widget);
    drop(discovery);
    let mut auth = AuthClient::with_trust(widget, seed)?;
    let source = crate::reviewer::session_source()?;
    authenticate_and_finalize(
        &mut auth,
        email,
        password,
        policy,
        store,
        |auth, session, policy| {
            finalize_confirmed(auth, session, email, &trust_root, &source, policy)
        },
    )
    .await
}

async fn authenticate_and_finalize(
    auth: &mut AuthClient,
    email: &str,
    password: &str,
    policy: ChallengePolicy,
    store: &impl credentials::SecretStore,
    confirm: impl FnOnce(&AuthClient, &Reply, ChallengePolicy) -> Result<PathBuf>,
) -> Result<PathBuf> {
    let session = authenticate_with_password(auth, email, password, policy).await?;
    let confirmed = confirm(auth, &session, policy);
    if policy.interactive() {
        save_password_after_confirmation(confirmed, email, password, store)
    } else {
        confirmed
    }
}

fn auth_reply_success(reply: &Reply) -> Result<()> {
    match reply.status.as_u16() {
        401 | 403 => bail!(
            "apple_login_credentials_rejected: run native reviewer-login --fresh to update the password"
        ),
        409 => bail!(
            "apple_login_2fa_required: run native reviewer-login to complete Apple verification"
        ),
        412 => bail!(
            "apple_login_account_action_required: acknowledge Apple's account requirements, then retry"
        ),
        _ => reply.success(),
    }
}

/// The shared SRP flow receives a borrowed zeroized caller-owned password and explicit policy.
async fn authenticate_with_password(
    auth: &mut AuthClient,
    email: &str,
    password: &str,
    policy: ChallengePolicy,
) -> Result<Reply> {
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
    auth_reply_success(&response)?;
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
        password,
        &salt,
        iterations,
        challenge["protocol"].as_str().unwrap_or_default(),
    )?;
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
    accept_completion(auth, response.status, policy, async |a| {
        a.two_factor().await
    })
    .await?;
    if response.status.is_success() {
        response.success()?;
    }
    let session = auth
        .request(
            Method::GET,
            &format!("{ASC}/olympus/v1/session"),
            None,
            None,
        )
        .await?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn finalize_fixture(
        auth: &AuthClient,
        session: &Reply,
        email: &str,
        trust_root: &Path,
        source: &Path,
    ) -> Result<PathBuf> {
        finalize_with_publication(
            auth,
            session,
            email,
            trust_root,
            source,
            ChallengePolicy::Interactive,
            |path, value| crate::reviewer::atomic_json(path, value).map(|_| ()),
        )
    }
    fn finalize_fixture_password(
        auth: &AuthClient,
        session: &Reply,
        email: &str,
        trust_root: &Path,
        source: &Path,
        password: &str,
        store: &impl credentials::SecretStore,
    ) -> Result<PathBuf> {
        save_password_after_confirmation(
            finalize_fixture(auth, session, email, trust_root, source),
            email,
            password,
            store,
        )
    }
    #[tokio::test]
    async fn recovery_unattended_409_never_starts_a_challenge() {
        let mut auth = AuthClient::new().unwrap();
        let challenges = std::sync::atomic::AtomicUsize::new(0);
        let error = accept_completion(
            &mut auth,
            StatusCode::CONFLICT,
            ChallengePolicy::Unattended,
            async |_| {
                challenges.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                bail!("challenge_was_started")
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().starts_with("apple_login_2fa_required"));
        assert_eq!(challenges.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[test]
    fn recovery_confirmed_identity_is_remembered_outside_session_cookie() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("private-trust");
        let source = d.path().join("configured-session.cookie");
        let mut auth = AuthClient::new().unwrap();
        auth.record_cookie(
            "myacinfo=synthetic-token; Domain=apple.com; Path=/; Secure",
            &Url::parse(ASC).unwrap(),
        );
        let reply = Reply {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: json!({"user":{"emailAddress":"A@X.Y"},"provider":{"providerId":123}})
                .to_string()
                .into_bytes(),
        };
        finalize_fixture(&auth, &reply, "a@x.y", &root, &source).unwrap();
        let account: Value = serde_json::from_slice(
            &fs::read(root.join("account.json"))
                .expect("successful login must remember the account"),
        )
        .unwrap();
        assert_eq!(account["account"], "a@x.y");
        assert!(!fs::read_to_string(&source).unwrap().contains("a@x.y"));
    }
    struct RecordingStore(std::cell::RefCell<Vec<(String, Zeroizing<String>)>>);
    impl credentials::SecretStore for RecordingStore {
        fn supported(&self) -> bool {
            true
        }
        fn get(&self, _: &str, _: credentials::Access) -> Result<Option<Zeroizing<String>>> {
            Ok(None)
        }
        fn set(&self, account: &str, password: &str) -> Result<()> {
            self.0
                .borrow_mut()
                .push((account.into(), Zeroizing::new(password.into())));
            Ok(())
        }
    }
    #[test]
    fn recovery_password_is_saved_only_after_matching_final_confirmation() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("trust");
        let source = d.path().join("configured.cookie");
        let mut auth = AuthClient::new().unwrap();
        auth.record_cookie(
            "myacinfo=fresh; Domain=apple.com; Path=/; Secure",
            &Url::parse(ASC).unwrap(),
        );
        let store = RecordingStore(std::cell::RefCell::new(Vec::new()));
        let mut reply = Reply {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: json!({"user":{"emailAddress":"another@x.y"},"provider":{"providerId":123}})
                .to_string()
                .into_bytes(),
        };
        assert!(
            finalize_fixture_password(
                &auth,
                &reply,
                "a@x.y",
                &root,
                &source,
                "secret-sentinel",
                &store
            )
            .is_err()
        );
        assert!(store.0.borrow().is_empty());
        assert!(!source.exists());
        assert!(!root.join("account.json").exists());
        reply.body = json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}})
            .to_string()
            .into_bytes();
        finalize_fixture_password(
            &auth,
            &reply,
            "a@x.y",
            &root,
            &source,
            "secret-sentinel",
            &store,
        )
        .unwrap();
        let saved = store.0.borrow();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, "a@x.y");
        assert_eq!(&*saved[0].1, "secret-sentinel");
        for path in [
            &source,
            &root.join("account.json"),
            &root.join("trust.json"),
        ] {
            assert!(
                !fs::read_to_string(path)
                    .unwrap()
                    .contains("secret-sentinel")
            );
        }
    }
    #[tokio::test]
    async fn recovery_shared_srp_is_headless_and_never_sends_plaintext_password() {
        use httpmock::prelude::{GET, MockServer, POST};
        for (status, expected) in [
            (200, None),
            (409, Some("apple_login_2fa_required")),
            (401, Some("apple_login_credentials_rejected")),
            (403, Some("apple_login_credentials_rejected")),
            (412, Some("apple_login_account_action_required")),
        ] {
            let server = MockServer::start();
            let init = server.mock(|when, then| {
                when.method(POST).path("/appleauth/auth/signin/init")
                    .body_excludes("secret-sentinel");
                then.json_body(json!({"salt":"c2FsdA==","b":"Ag==","iteration":1,"protocol":"s2k","c":"challenge"}));
            });
            let cash = server.mock(|when, then| {
                when.method(GET).path("/appleauth/auth/signin");
                then.status(200);
            });
            let complete = server.mock(|when, then| {
                when.method(POST)
                    .path("/appleauth/auth/signin/complete")
                    .body_excludes("secret-sentinel")
                    .body_includes("\"accountName\":\"a@x.y\"");
                then.status(status).json_body(json!({}));
            });
            let challenge = server.mock(|when, then| {
                when.method(GET).path("/appleauth/auth");
                then.status(200);
            });
            let session = server.mock(|when, then| {
                when.method(GET).path("/olympus/v1/session");
                then.header(
                    "set-cookie",
                    "myacinfo=fresh; Domain=apple.com; Path=/; Secure",
                )
                .json_body(json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}}));
            });
            let mut auth = AuthClient::with_trust("widget".into(), Vec::new()).unwrap();
            auth.test_origin = Some(server.base_url());
            let result = authenticate_with_password(
                &mut auth,
                "a@x.y",
                "secret-sentinel",
                ChallengePolicy::Unattended,
            )
            .await;
            if let Some(expected) = expected {
                let error = result
                    .err()
                    .expect("user action must stop authentication")
                    .to_string();
                assert!(error.starts_with(expected), "{error}");
                assert!(!error.contains("secret-sentinel"));
                session.assert_calls(0);
            } else {
                let reply = result.unwrap();
                let dir = tempfile::tempdir().unwrap();
                finalize_fixture(
                    &auth,
                    &reply,
                    "a@x.y",
                    &dir.path().join("trust"),
                    &dir.path().join("configured.cookie"),
                )
                .unwrap();
                assert_eq!(auth.asc_cookies()[0]["value"], "fresh");
                session.assert_calls(1);
            }
            init.assert_calls(1);
            cash.assert_calls(1);
            complete.assert_calls(1);
            challenge.assert_calls(0);
        }
    }
    #[tokio::test]
    async fn recovery_rejected_init_stops_before_srp_complete_or_challenge() {
        use httpmock::prelude::{GET, MockServer, POST};
        for (status, expected) in [
            (401, "apple_login_credentials_rejected"),
            (403, "apple_login_credentials_rejected"),
            (409, "apple_login_2fa_required"),
            (412, "apple_login_account_action_required"),
        ] {
            let server = MockServer::start();
            let init = server.mock(|when, then| {
                when.method(POST).path("/appleauth/auth/signin/init");
                then.status(status).body("secret-sentinel");
            });
            let later = server.mock(|when, then| {
                when.path("/appleauth/auth/signin/complete");
                then.status(200);
            });
            let challenge = server.mock(|when, then| {
                when.method(GET).path("/appleauth/auth");
                then.status(200);
            });
            let mut auth = AuthClient::with_trust("widget".into(), Vec::new()).unwrap();
            auth.test_origin = Some(server.base_url());
            let error = authenticate_with_password(
                &mut auth,
                "a@x.y",
                "secret-sentinel",
                ChallengePolicy::Unattended,
            )
            .await
            .err()
            .unwrap()
            .to_string();
            assert!(error.starts_with(expected), "{error}");
            assert!(!error.contains("secret-sentinel"));
            init.assert_calls(1);
            later.assert_calls(0);
            challenge.assert_calls(0);
        }
    }
    #[test]
    fn recovery_unattended_provider_change_preserves_every_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trust");
        let source = dir.path().join("configured.cookie");
        trust::save_to(&root, "a@x.y", &[fixture_trust()]).unwrap();
        crate::reviewer::atomic_json(
            &root.join("account.json"),
            &json!({"version":1,"account":"a@x.y","provider_id":123}),
        )
        .unwrap();
        crate::reviewer::atomic_json(&source,&json!({"cookies":[{"name":"myacinfo","value":"previous","domain":"apple.com","path":"/"}]})).unwrap();
        let before_account = fs::read(root.join("account.json")).unwrap();
        let before_trust = fs::read(root.join("trust.json")).unwrap();
        let before_source = fs::read(&source).unwrap();
        let mut auth = AuthClient::new().unwrap();
        auth.record_cookie(
            "myacinfo=new; Domain=apple.com; Path=/; Secure",
            &Url::parse(ASC).unwrap(),
        );
        let store = RecordingStore(std::cell::RefCell::new(Vec::new()));
        for provider in [json!({"providerId":999}), json!({})] {
            let reply = Reply {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: json!({"user":{"emailAddress":"a@x.y"},"provider":provider})
                    .to_string()
                    .into_bytes(),
            };
            let result = finalize_with_publication(
                &auth,
                &reply,
                "a@x.y",
                &root,
                &source,
                ChallengePolicy::Unattended,
                |p, v| crate::reviewer::atomic_json(p, v).map(|_| ()),
            );
            let error =
                save_password_after_confirmation(result, "a@x.y", "secret-sentinel", &store)
                    .unwrap_err()
                    .to_string();
            assert!(error.starts_with("apple_login_provider_"), "{error}");
            assert_eq!(fs::read(root.join("account.json")).unwrap(), before_account);
            assert_eq!(fs::read(root.join("trust.json")).unwrap(), before_trust);
            assert_eq!(fs::read(&source).unwrap(), before_source);
            assert!(store.0.borrow().is_empty());
        }
    }
    #[tokio::test]
    async fn recovery_unattended_preflight_provider_change_does_not_rotate_cookies() {
        use httpmock::prelude::{GET, MockServer};
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let reusable = existing(dir.path()).await;
        let root = dir.path().join("trust");
        crate::reviewer::atomic_json(
            &root.join("account.json"),
            &json!({"version":1,"account":"a@x.y","provider_id":123}),
        )
        .unwrap();
        server.mock(|when, then| {
            when.method(GET).path("/olympus/v1/session");
            then.header(
                "set-cookie",
                "myacinfo=wrong-provider; Domain=apple.com; Path=/",
            )
            .json_body(json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":999}}));
        });
        let error = preflight(
            &server.base_url(),
            "a@x.y",
            &reusable,
            &root,
            ChallengePolicy::Unattended,
        )
        .await
        .err()
        .expect("provider drift must require a human")
        .to_string();
        assert!(error.starts_with("apple_login_provider_changed"), "{error}");
        assert!(!dir.path().join("session.state.json").exists());
        assert_eq!(reusable.cookie_header(), "myacinfo=old");
    }
    #[test]
    fn recovery_provider_pin_requires_enrollment_and_explicit_human_override() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trust");
        let body = json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}});
        assert!(
            confirmed_provider(&root, "a@x.y", &body, ChallengePolicy::Unattended)
                .unwrap_err()
                .to_string()
                .starts_with("apple_login_provider_required")
        );
        assert_eq!(
            confirmed_provider(&root, "a@x.y", &body, ChallengePolicy::Interactive).unwrap(),
            123
        );
        credentials::remember_identity(&root, "a@x.y", Some(123)).unwrap();
        let body = json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":456}});
        assert!(
            confirmed_provider(&root, "a@x.y", &body, ChallengePolicy::Interactive)
                .unwrap_err()
                .to_string()
                .starts_with("apple_login_provider_changed")
        );
        assert!(confirmed_provider(&root, "a@x.y", &body, ChallengePolicy::Unattended).is_err());
        assert_eq!(
            confirmed_provider(
                &root,
                "a@x.y",
                &body,
                ChallengePolicy::InteractiveProviderOverride
            )
            .unwrap(),
            456
        );
        assert_eq!(
            credentials::pinned_provider(&root, "other@x.y").unwrap(),
            None
        );
        for provider in [
            json!({"providerId":0}),
            json!({"providerId":-1}),
            json!({"providerId":"123"}),
            json!({}),
        ] {
            let body = json!({"provider":provider});
            assert!(
                confirmed_provider(
                    &root,
                    "a@x.y",
                    &body,
                    ChallengePolicy::InteractiveProviderOverride
                )
                .is_err()
            );
        }
    }
    #[tokio::test]
    async fn recovery_unattended_success_never_writes_the_secret_store() {
        use httpmock::prelude::{GET, MockServer, POST};
        let server = MockServer::start();
        server.mock(|when,then| { when.method(POST).path("/appleauth/auth/signin/init").body_excludes("secret-sentinel"); then.json_body(json!({"salt":"c2FsdA==","b":"Ag==","iteration":1,"protocol":"s2k","c":"challenge"})); });
        server.mock(|when, then| {
            when.method(GET).path("/appleauth/auth/signin");
            then.status(200);
        });
        server.mock(|when, then| {
            when.method(POST)
                .path("/appleauth/auth/signin/complete")
                .body_excludes("secret-sentinel");
            then.status(200).json_body(json!({}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/olympus/v1/session");
            then.header(
                "set-cookie",
                "myacinfo=renewed; Domain=apple.com; Path=/; Secure",
            )
            .json_body(json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}}));
        });
        let challenge = server.mock(|when, then| {
            when.path("/appleauth/auth/verify");
            then.status(200);
        });
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trust");
        let source = dir.path().join("configured.cookie");
        credentials::remember_identity(&root, "a@x.y", Some(123)).unwrap();
        let store = RecordingStore(std::cell::RefCell::new(Vec::new()));
        let mut auth = AuthClient::with_trust("widget".into(), Vec::new()).unwrap();
        auth.test_origin = Some(server.base_url());
        let result = authenticate_and_finalize(
            &mut auth,
            "a@x.y",
            "secret-sentinel",
            ChallengePolicy::Unattended,
            &store,
            |auth, session, policy| {
                finalize_with_publication(auth, session, "a@x.y", &root, &source, policy, |p, v| {
                    crate::reviewer::atomic_json(p, v).map(|_| ())
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(result, source);
        assert!(fs::read_to_string(&source).unwrap().contains("renewed"));
        assert!(
            store.0.borrow().is_empty(),
            "an unattended renewal must never invoke SecretStore::set"
        );
        challenge.assert_calls(0);
    }
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
    fn trusted_auth_cookie_survives_recording_for_next_login() {
        let mut auth = AuthClient::new().unwrap();
        let idmsa = Url::parse(AUTH).unwrap();
        auth.record_cookie(
            "DES-fixture=synthetic-trust; Path=/appleauth; Secure; HttpOnly",
            &idmsa,
        );
        assert!(
            auth.cookies.values().any(|c| c["name"] == "DES-fixture"),
            "the trusted login cookie must survive client exit for reuse on the next login"
        );
    }
    #[test]
    fn trust_round_trip_binding_and_permissions() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("login-trust");
        let mut auth = AuthClient::new().unwrap();
        let idmsa = Url::parse(AUTH).unwrap();
        auth.record_cookie("DES-x=trust; Path=/appleauth; Secure; Max-Age=3600", &idmsa);
        auth.record_cookie("broad=b; Domain=apple.com; Path=/; Secure", &idmsa);
        let trust = auth.trust_cookies();
        assert_eq!(trust.len(), 2);
        let path = trust::save_to(&root, " User@Example.com ", &trust).unwrap();
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("example.com"));
        assert_eq!(trust::load_from(&root, "user@example.com").unwrap(), trust);
        // Another account sees nothing and cannot reuse it; a missing store is normal.
        assert!(
            trust::load_from(&root, "other@example.com")
                .unwrap()
                .is_empty()
        );
        assert!(
            trust::load_from(&dir.path().join("none"), "a@b.c")
                .unwrap()
                .is_empty()
        );
        #[cfg(unix)]
        {
            // Insecure file or directory, and symlinks, fail closed.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(trust::load_from(&root, "user@example.com").is_err());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(trust::load_from(&root, "user@example.com").is_err());
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            assert!(trust::load_from(&link, "user@example.com").is_err());
        }
        fs::write(&path, b"{not json").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(trust::load_from(&root, "user@example.com").is_err());
    }
    #[test]
    fn expired_trust_is_dropped_and_account_switch_replaces_after_confirm() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut c = TrustCookie {
            name: "DES-a".into(),
            value: "v".into(),
            origin_host: trust::IDMSA_HOST.into(),
            domain: trust::IDMSA_HOST.into(),
            host_only: true,
            path: "/appleauth".into(),
            secure: true,
            expires: Some("2000-01-01T00:00:00+00:00".into()),
        };
        trust::save_to(dir.path(), "a@x.y", &[c.clone()]).unwrap();
        assert!(trust::load_from(dir.path(), "a@x.y").unwrap().is_empty());
        c.expires = None;
        trust::save_to(dir.path(), "b@x.y", &[c.clone()]).unwrap();
        assert!(trust::load_from(dir.path(), "a@x.y").unwrap().is_empty());
        assert_eq!(trust::load_from(dir.path(), "b@x.y").unwrap().len(), 1);
    }
    #[test]
    fn seeded_trust_header_is_scoped_quoted_and_single() {
        let seed = vec![TrustCookie {
            name: "DES-a".into(),
            value: "tok".into(),
            origin_host: trust::IDMSA_HOST.into(),
            domain: trust::IDMSA_HOST.into(),
            host_only: true,
            path: "/appleauth".into(),
            secure: true,
            expires: None,
        }];
        let auth = AuthClient::with_trust("w".into(), seed).unwrap();
        let signin = Url::parse(&format!("{AUTH}/signin/init")).unwrap();
        let value = auth.idmsa_cookie(&signin).unwrap();
        assert_eq!(value.to_str().unwrap(), "DES-a=\"tok\"");
        assert!(value.is_sensitive());
        // Never sent to ASC nor outside the cookie's path.
        assert!(
            auth.idmsa_cookie(&Url::parse(&format!("{ASC}/olympus/v1/session")).unwrap())
                .is_none()
        );
        assert!(
            auth.idmsa_cookie(&Url::parse("https://idmsa.apple.com/other").unwrap())
                .is_none()
        );
        // Seeded trust is not part of the reviewer's ASC session.
        assert!(auth.asc_cookies().is_empty());
        assert_eq!(auth.trust_cookies().len(), 1);
    }
    #[test]
    fn live_jar_des_rotation_is_also_quoted() {
        let u = Url::parse(&format!("{AUTH}/signin/complete")).unwrap();
        let h = trust::cookie_header(&[], &u, Some("DES-a=rotated; session=alive")).unwrap();
        assert!(h.contains("DES-a=\"rotated\""), "{h}");
        assert!(h.contains("session=alive"));
    }
    #[test]
    fn auth_default_path_and_cookie_prefix_constraints() {
        let mut a = AuthClient::new().unwrap();
        let u = Url::parse(&format!("{AUTH}/signin/init")).unwrap();
        a.record_cookie("DES-default=trusted; Secure", &u);
        assert_eq!(a.trust_cookies()[0].path, "/appleauth/auth/signin");
        let sibling = Url::parse(&format!("{AUTH}/verify/trusteddevice")).unwrap();
        assert!(trust::cookie_header(&a.trust_cookies(), &sibling, None).is_none());
        a.record_cookie("__Secure-invalid=x; Path=/", &u);
        a.record_cookie("__Host-invalid=x; Domain=apple.com; Path=/; Secure", &u);
        assert!(
            !a.cookies
                .values()
                .any(|v| v["name"] == "__Secure-invalid" || v["name"] == "__Host-invalid")
        );
        a.record_cookie(
            "__Host-missing=x; Secure",
            &Url::parse("https://idmsa.apple.com/").unwrap(),
        );
        assert!(!a.cookies.values().any(|v| v["name"] == "__Host-missing"));
        a.record_cookie("DES-default=gone; Secure; Max-Age=0", &u);
        assert!(!a.cookies.values().any(|v| v["name"] == "DES-default"));
    }
    #[test]
    fn unsafe_trust_values_and_oversized_store_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("trust");
        let c = TrustCookie {
            name: "DES-a".into(),
            value: "safe".into(),
            origin_host: trust::IDMSA_HOST.into(),
            domain: trust::IDMSA_HOST.into(),
            host_only: true,
            path: "/".into(),
            secure: true,
            expires: None,
        };
        for value in ["unsafe\"quote", "unsafe\\slash", "unsafe,comma"] {
            let mut bad = c.clone();
            bad.value = value.into();
            assert!(!bad.valid());
        }
        let mut prefix = c.clone();
        prefix.name = "__Secure-bad".into();
        prefix.secure = false;
        assert!(!prefix.valid());
        prefix = c.clone();
        prefix.name = "__Host-bad".into();
        prefix.host_only = false;
        assert!(!prefix.valid());
        let many: Vec<_> = (0..150)
            .map(|i| {
                let mut v = c.clone();
                v.name = format!("cookie-{i}");
                v.value = "a".repeat(4096);
                v
            })
            .collect();
        assert!(trust::save_to(&root, "a@x.y", &many).is_err());
        assert!(!trust::path_in(&root).exists());
    }
    fn fixture_trust() -> TrustCookie {
        TrustCookie {
            name: "DES-a".into(),
            value: "cached".into(),
            origin_host: trust::IDMSA_HOST.into(),
            domain: trust::IDMSA_HOST.into(),
            host_only: true,
            path: "/appleauth".into(),
            secure: true,
            expires: None,
        }
    }
    #[tokio::test]
    async fn mocked_trusted_completion_only_requests_challenge_on_409() {
        use httpmock::prelude::{GET, MockServer, POST};
        for (status, challenges) in [(200, 0), (409, 1)] {
            let server = MockServer::start();
            let complete = server.mock(|when, then| {
                when.method(POST)
                    .path("/appleauth/auth/signin/complete")
                    .header("cookie", "DES-a=\"cached\"");
                then.status(status)
                    .header("set-cookie", "DES-a=rotated; Path=/appleauth; Secure")
                    .json_body(json!({}));
            });
            let challenge = server.mock(|when, then| {
                when.method(GET)
                    .path("/appleauth/auth/verify")
                    .header("cookie", "DES-a=\"rotated\"");
                then.status(200);
            });
            let session = server.mock(|when, then| {
                when.method(GET).path("/olympus/v1/session");
                then.status(200)
                    .header(
                        "set-cookie",
                        "myacinfo=active; Domain=apple.com; Path=/; Secure",
                    )
                    .json_body(
                        json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}}),
                    );
            });
            let mut auth = AuthClient::with_trust("fixture".into(), vec![fixture_trust()]).unwrap();
            auth.test_origin = Some(server.base_url());
            let r = auth
                .request(
                    Method::POST,
                    &format!("{AUTH}/signin/complete"),
                    Some(&json!({})),
                    None,
                )
                .await
                .unwrap();
            accept_completion(
                &mut auth,
                r.status,
                ChallengePolicy::Interactive,
                async |a| {
                    a.request(Method::GET, &format!("{AUTH}/verify"), None, None)
                        .await?
                        .success()
                },
            )
            .await
            .unwrap();
            let r = auth
                .request(
                    Method::GET,
                    &format!("{ASC}/olympus/v1/session"),
                    None,
                    None,
                )
                .await
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let store = dir.path().join("login-trust");
            let source = dir.path().join("session.cookie");
            finalize_fixture(&auth, &r, "a@x.y", &store, &source).unwrap();
            assert_eq!(
                trust::load_from(&store, "a@x.y").unwrap()[0].value,
                "rotated"
            );
            assert!(!std::fs::read_to_string(source).unwrap().contains("DES-a"));
            complete.assert_calls(1);
            challenge.assert_calls(challenges);
            session.assert_calls(1);
        }
    }
    #[tokio::test]
    async fn discovery_never_receives_cached_cookies() {
        use httpmock::prelude::MockServer;
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method("HEAD").path("/logout").is_true(|r| {
                !r.headers()
                    .iter()
                    .any(|(k, _)| k.as_str().eq_ignore_ascii_case("cookie"))
            });
            then.status(302).header(
                "location",
                "https://idmsa.apple.com/appleauth/auth/signout?widgetKey=fixture",
            );
        });
        let mut discovery = AuthClient::new().unwrap();
        discovery.test_origin = Some(server.base_url());
        discovery.widget_key().await.unwrap();
        let auth = AuthClient::with_trust(discovery.widget, vec![fixture_trust()]).unwrap();
        assert!(
            auth.idmsa_cookie(&Url::parse(&format!("{AUTH}/signin/init")).unwrap())
                .is_some()
        );
        m.assert_calls(1);
    }
    #[test]
    fn failed_final_confirmation_preserves_both_stores_and_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("login-trust");
        let source = dir.path().join("session.cookie");
        let overlay = dir.path().join("session.state.json");
        trust::save_to(&store, "a@x.y", &[fixture_trust()]).unwrap();
        crate::reviewer::atomic_json(&source,&json!({"cookies":[{"name":"myacinfo","value":"previous","domain":"apple.com","path":"/"}]})).unwrap();
        crate::reviewer::atomic_json(&overlay, &json!({"generation":"previous"})).unwrap();
        let before_source = std::fs::read(&source).unwrap();
        let before_trust = std::fs::read(trust::path_in(&store)).unwrap();
        let before_overlay = std::fs::read(&overlay).unwrap();
        let mut auth = AuthClient::with_trust("fixture".into(), vec![fixture_trust()]).unwrap();
        auth.record_cookie(
            "myacinfo=new; Domain=apple.com; Path=/; Secure",
            &Url::parse(ASC).unwrap(),
        );
        for (status, body) in [
            (
                StatusCode::OK,
                json!({"user":{"emailAddress":"other@x.y"},"provider":{"providerId":123}})
                    .to_string(),
            ),
            (StatusCode::OK, json!({"provider":{}}).to_string()),
            (StatusCode::UNAUTHORIZED, "{}".into()),
            (StatusCode::OK, "invalid-json".into()),
        ] {
            let r = Reply {
                status,
                headers: HeaderMap::new(),
                body: body.into_bytes(),
            };
            assert!(finalize_fixture(&auth, &r, "a@x.y", &store, &source).is_err());
            assert_eq!(std::fs::read(&source).unwrap(), before_source);
            assert_eq!(std::fs::read(trust::path_in(&store)).unwrap(), before_trust);
            assert_eq!(std::fs::read(&overlay).unwrap(), before_overlay);
        }
        auth.cookies.clear();
        let r = Reply {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}})
                .to_string()
                .into_bytes(),
        };
        assert!(finalize_fixture(&auth, &r, "a@x.y", &store, &source).is_err());
        assert_eq!(std::fs::read(trust::path_in(&store)).unwrap(), before_trust);
    }
    #[tokio::test]
    async fn configured_source_preflight_uses_overlay_and_ignores_stale_generation() {
        use httpmock::prelude::{GET, MockServer};
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let source = dir.path().join("configured.cookie");
        let data = crate::reviewer::atomic_json(
            &source,
            &json!({"cookies":[{"name":"myacinfo","value":"old","domain":"apple.com","path":"/"}]}),
        )
        .unwrap();
        crate::reviewer::atomic_json(&state.join("session.state.json"),&json!({"generation":hex::encode(Sha256::digest(&data)),"cookies":[{"name":"myacinfo","value":"rotated","domain":"apple.com","path":"/"}]})).unwrap();
        let reusable = crate::reviewer::Reusable::from_paths(source.clone(), state.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reusable.source_path(), source);
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(GET)
                .path("/olympus/v1/session")
                .header("cookie", "myacinfo=rotated");
            then.json_body(json!({"user":{"emailAddress":"a@x.y"},"provider":{"providerId":123}}));
        });
        assert!(matches!(
            preflight(
                &server.base_url(),
                "a@x.y",
                &reusable,
                reusable.source_path().parent().unwrap(),
                ChallengePolicy::Interactive
            )
            .await
            .unwrap(),
            Preflight::Reused(_)
        ));
        m.assert_calls(1);
        crate::reviewer::atomic_json(&source,&json!({"cookies":[{"name":"myacinfo","value":"new-generation","domain":"apple.com","path":"/"}]})).unwrap();
        assert_eq!(
            crate::reviewer::Reusable::from_paths(source, state)
                .await
                .unwrap()
                .unwrap()
                .cookie_header(),
            "myacinfo=new-generation"
        );
    }
    #[test]
    fn same_name_cookies_at_different_paths_are_not_lost() {
        let mut root = fixture_trust();
        root.name = "session".into();
        root.path = "/".into();
        root.value = "root".into();
        let mut scoped = root.clone();
        scoped.path = "/appleauth".into();
        scoped.value = "scoped".into();
        assert_eq!(
            trust::cookie_header(
                &[root, scoped],
                &Url::parse(AUTH).unwrap(),
                Some("session=scoped")
            )
            .unwrap(),
            "session=scoped; session=root"
        );
    }
    #[test]
    fn rejected_prefix_jar_value_cannot_override_valid_seed() {
        let mut c = fixture_trust();
        c.name = "__Host-a".into();
        c.path = "/".into();
        assert_eq!(
            trust::cookie_header(&[c], &Url::parse(AUTH).unwrap(), Some("__Host-a=bad")).unwrap(),
            "__Host-a=cached"
        );
    }
    #[test]
    fn identity_requires_matching_email_not_provider() {
        assert!(identity_matches(
            &json!({"user":{"emailAddress":" A@X.Y "},"provider":{"providerId":123}}),
            "a@x.y"
        ));
        assert!(!identity_matches(
            &json!({"user":{"emailAddress":"b@x.y"},"provider":{"providerId":123}}),
            "a@x.y"
        ));
        assert!(!identity_matches(
            &json!({"provider":{"name":"p"}}),
            "a@x.y"
        ));
        assert!(!identity_matches(&json!({"user":{}}), "a@x.y"));
    }
    async fn existing(dir: &std::path::Path) -> crate::reviewer::Reusable {
        crate::reviewer::atomic_json(
            &dir.join("session.cookie"),
            &json!({"cookies":[{"name":"myacinfo","value":"old","domain":"apple.com","path":"/"}]}),
        )
        .unwrap();
        crate::reviewer::Reusable::from_paths(dir.join("session.cookie"), dir.to_owned())
            .await
            .unwrap()
            .unwrap()
    }
    #[tokio::test]
    async fn preflight_reuses_matching_identity_and_saves_rotation() {
        use httpmock::prelude::{GET, MockServer};
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let reusable = existing(dir.path()).await;
        let m = server.mock(|when, then| {
            when.method(GET)
                .path("/olympus/v1/session")
                .header("cookie", "myacinfo=old");
            then.header("set-cookie", "myacinfo=rotated; Domain=apple.com; Path=/")
                .json_body(json!({"user":{"emailAddress":"A@x.y"},"provider":{"providerId":123}}));
        });
        assert!(matches!(
            preflight(
                &server.base_url(),
                "a@x.y",
                &reusable,
                reusable.source_path().parent().unwrap(),
                ChallengePolicy::Interactive
            )
            .await
            .unwrap(),
            Preflight::Reused(_)
        ));
        m.assert();
        let reloaded = crate::reviewer::Reusable::from_paths(
            dir.path().join("session.cookie"),
            dir.path().to_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reloaded.cookie_header(), "myacinfo=rotated");
    }
    #[tokio::test]
    async fn preflight_fresh_on_401_or_mismatch_and_closed_on_other_failures() {
        use httpmock::prelude::{GET, MockServer};
        for (status, body, fresh) in [
            (401, json!({}), Some(true)),
            (
                200,
                json!({"user":{"emailAddress":"other@x.y"},"provider":{"providerId":123}}),
                Some(true),
            ),
            (200, json!({"provider":{"name":"p"}}), Some(true)),
            (403, json!({}), None),
            (500, json!({}), None),
        ] {
            let server = MockServer::start();
            let dir = tempfile::tempdir().unwrap();
            let reusable = existing(dir.path()).await;
            server.mock(|when, then| {
                when.method(GET).path("/olympus/v1/session");
                then.status(status).json_body(body.clone());
            });
            let result = preflight(
                &server.base_url(),
                "a@x.y",
                &reusable,
                reusable.source_path().parent().unwrap(),
                ChallengePolicy::Interactive,
            )
            .await;
            match fresh {
                Some(_) => assert!(matches!(result.unwrap(), Preflight::Fresh), "{status}"),
                None => assert!(result.is_err(), "{status}"),
            }
            // Nothing was written for a non-reused session.
            assert!(!dir.path().join("session.state.json").exists());
        }
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let reusable = existing(dir.path()).await;
        server.mock(|when, then| {
            when.method(GET);
            then.status(200).body("not json");
        });
        assert!(
            preflight(
                &server.base_url(),
                "a@x.y",
                &reusable,
                reusable.source_path().parent().unwrap(),
                ChallengePolicy::Interactive
            )
            .await
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
        // IDMSA-only cookies are retained as login trust but never enter the ASC session.
        assert_eq!(auth.asc_cookies().len(), 1);
        assert!(auth.cookies.values().any(|c| c["name"] == "auth"));
        assert!(!auth.cookies.values().any(|c| c["name"] == "bad"));
        auth.record_cookie("session=replaced; Domain=.apple.com; Path=/", &asc);
        assert_eq!(auth.asc_cookies()[0]["value"], "replaced");
        auth.record_cookie("session=gone; Domain=.apple.com; Path=/; Max-Age=0", &asc);
        assert!(auth.asc_cookies().is_empty());
    }
}
