//! Signed segment downloads. The signed URL comes only from Apple's API
//! response, is checked against an allowlist, and is fetched by a client that
//! carries no credentials, cookies, proxy, redirects or retries. No error here
//! ever contains the URL or any part of it.
use flate2::read::MultiGzDecoder;
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};
use url::{Host, Url};

pub fn client() -> Result<reqwest::Client, &'static str> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "unable to build the download client")
}
fn apple_host(host: &str) -> bool {
    host == "apple.com" || host.ends_with(".apple.com")
}
/// `asp-<bucket>.s3.<region>.amazonaws.com` (also `.s3.amazonaws.com` and
/// `.s3-<region>.`): one virtual-host bucket, never the whole amazonaws.com.
fn s3_virtual_host(host: &str) -> bool {
    let Some(rest) = host.strip_suffix(".amazonaws.com") else {
        return false;
    };
    let Some((bucket, service)) = rest.split_once(".s3") else {
        return false;
    };
    let region = |r: &str| {
        !r.is_empty()
            && r.len() <= 30
            && r.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    };
    bucket.starts_with("asp-")
        && bucket.len() <= 63
        && !bucket.ends_with('-')
        && bucket
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && (service.is_empty()
            || service
                .strip_prefix('.')
                .or_else(|| service.strip_prefix('-'))
                .is_some_and(region))
}
/// Accept production HTTPS (port 443, no userinfo/fragment, domain host on the
/// allowlist) or, for clients built by `ApiClient::with_endpoint`, exactly the
/// offline loopback origin.
pub fn validate_url(raw: &str, offline: Option<&Url>) -> Result<Url, &'static str> {
    const REJECTED: &str = "segment download URL is not an allowed Apple download location";
    let url = Url::parse(raw).map_err(|_| REJECTED)?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(REJECTED);
    }
    if offline.is_some_and(|o| url.origin() == o.origin()) {
        return Ok(url);
    }
    if url.scheme() != "https" || url.port().is_some() {
        return Err(REJECTED);
    }
    match url.host() {
        Some(Host::Domain(host))
            if !host.ends_with('.') && (apple_host(host) || s3_virtual_host(host)) =>
        {
            Ok(url)
        }
        _ => Err(REJECTED),
    }
}
/// Stream the body, requiring exactly `size` bytes.
pub async fn fetch(http: &reqwest::Client, url: Url, size: u64) -> Result<Vec<u8>, &'static str> {
    const FAILED: &str = "segment download failed";
    let mut response = http
        .get(url)
        .header("User-Agent", "asc-mcp-rust/0.2.0")
        .send()
        .await
        .map_err(|_| FAILED)?;
    if response.status().as_u16() != 200 {
        return Err("segment download was rejected");
    }
    const MISMATCH: &str = "segment length does not match its sizeInBytes metadata";
    if response.content_length().is_some_and(|n| n != size) {
        return Err(MISMATCH);
    }
    let mut body = Vec::with_capacity(size.min(1 << 20) as usize);
    while let Some(chunk) = response.chunk().await.map_err(|_| FAILED)? {
        if body.len() as u64 + chunk.len() as u64 > size {
            return Err(MISMATCH);
        }
        body.extend_from_slice(&chunk);
    }
    if body.len() as u64 != size {
        return Err(MISMATCH);
    }
    Ok(body)
}
/// Hex checksum of the compressed bytes: MD5 (32 digits, Apple's) or SHA-256
/// (64 digits). Any other form fails rather than being ignored.
pub fn verify_checksum(bytes: &[u8], expected: &str) -> Result<(), &'static str> {
    let expected = expected.trim().to_ascii_lowercase();
    let actual = match expected.len() {
        32 => format!("{:x}", md5::compute(bytes)),
        64 => hex::encode(Sha256::digest(bytes)),
        _ => return Err("segment checksum type is unsupported"),
    };
    if expected.bytes().all(|c| c.is_ascii_hexdigit()) && actual == expected {
        Ok(())
    } else {
        Err("segment checksum does not match")
    }
}
/// Decompress every gzip member (CRC and trailer verified by the decoder),
/// failing if the output would exceed `limit` bytes.
pub fn gunzip(bytes: &[u8], limit: u64) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    MultiGzDecoder::new(bytes)
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|_| "segment is not valid gzip data")?;
    if out.len() as u64 > limit {
        return Err("segment expands beyond the decompression limit");
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    fn gz(text: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(text).unwrap();
        e.finish().unwrap()
    }
    #[test]
    fn production_hosts_are_anchored() {
        let ok = |u: &str| validate_url(u, None).is_ok();
        assert!(ok(
            "https://asp-qa-us-west-2.s3.us-west-2.amazonaws.com/r/0.csv.gz?X-Amz-Signature=s"
        ));
        assert!(ok("https://asp-prod.s3.amazonaws.com/r"));
        assert!(ok("https://reports.itunes.apple.com/r"));
        for bad in [
            "https://example.com/r",
            "https://evilapple.com/r",
            "https://apple.com.evil.com/r",
            "https://s3.amazonaws.com/asp-x/r",
            "https://other-bucket.s3.us-west-2.amazonaws.com/r",
            "https://asp-x.evil.s3.us-west-2.amazonaws.com/r",
            "https://asp-x.s3.us-west-2.amazonaws.com.evil.com/r",
            "http://asp-x.s3.us-west-2.amazonaws.com/r",
            "https://asp-x.s3.us-west-2.amazonaws.com:8443/r",
            "https://u:p@asp-x.s3.us-west-2.amazonaws.com/r",
            "https://asp-x.s3.us-west-2.amazonaws.com/r#frag",
            "https://127.0.0.1/r",
            "https://[::1]/r",
            "https://apple.com./r",
            "file:///etc/passwd",
        ] {
            assert!(!ok(bad), "{bad}");
        }
    }
    #[test]
    fn only_the_exact_offline_origin_is_allowed() {
        let offline = Url::parse("http://127.0.0.1:8080").unwrap();
        assert!(validate_url("http://127.0.0.1:8080/d?sig=s", Some(&offline)).is_ok());
        assert!(validate_url("http://127.0.0.1:8081/d", Some(&offline)).is_err());
        assert!(validate_url("http://localhost:8080/d", Some(&offline)).is_err());
        assert!(validate_url("http://127.0.0.1:8080/d", None).is_err());
    }
    #[test]
    fn checksums_and_gzip_are_strict() {
        let data = gz(b"hello");
        let md5 = format!("{:x}", md5::compute(&data));
        assert!(verify_checksum(&data, &md5).is_ok());
        assert!(verify_checksum(&data, &md5.to_uppercase()).is_ok());
        assert!(verify_checksum(&data, &hex::encode(Sha256::digest(&data))).is_ok());
        assert!(verify_checksum(&data, "abcd").is_err());
        assert!(verify_checksum(&data, &"0".repeat(32)).is_err());
        assert_eq!(gunzip(&data, 5).unwrap(), b"hello");
        assert!(gunzip(&data, 4).is_err());
        assert!(gunzip(b"plain", 100).is_err());
        assert!(gunzip(&data[..data.len() - 4], 100).is_err());
        let mut two = gz(b"ab");
        two.extend(gz(b"cd"));
        assert_eq!(gunzip(&two, 10).unwrap(), b"abcd");
    }
}
