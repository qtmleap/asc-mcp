//! Compound binary asset delivery and opaque sales/finance report responses.
use crate::client::ApiClient;
use anyhow::{Result, anyhow, bail, ensure};
use base64::{Engine, engine::general_purpose};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};

pub const MAX_UPLOAD_SIZE: usize = 600 * 1024 * 1024;
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) => Ok(s),
        _ => bail!("{key} must be a string"),
    }
}
fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let s = string(args, key)?;
    ensure!(!s.is_empty(), "{key} is required");
    Ok(s)
}
fn identifier(s: &str) -> Result<&str> {
    ensure!(
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')),
        "invalid resource identifier"
    );
    Ok(s)
}
fn load_body(args: &Value) -> Result<Vec<u8>> {
    let path = string(args, "file_path")?;
    let data = string(args, "file_data_base64")?;
    ensure!(
        path.is_empty() != data.is_empty(),
        "provide exactly one of file_path or file_data_base64"
    );
    let body = if !data.is_empty() {
        ensure!(
            data.len() <= (MAX_UPLOAD_SIZE / 3 + 1) * 4,
            "file_data exceeds maximum upload size"
        );
        [
            general_purpose::STANDARD,
            general_purpose::STANDARD_NO_PAD,
            general_purpose::URL_SAFE,
            general_purpose::URL_SAFE_NO_PAD,
        ]
        .iter()
        .find_map(|engine| engine.decode(data).ok())
        .ok_or_else(|| anyhow!("file_data_base64 is not valid base64"))?
    } else {
        // Relative paths resolve from the server's working directory, as in the Go server.
        let mut file =
            std::fs::File::open(Path::new(path)).map_err(|_| anyhow!("cannot open file_path"))?;
        let metadata = file
            .metadata()
            .map_err(|_| anyhow!("cannot inspect file_path"))?;
        ensure!(metadata.is_file(), "file_path must be a regular file");
        ensure!(
            metadata.len() <= MAX_UPLOAD_SIZE as u64,
            "file exceeds maximum upload size"
        );
        let mut body = Vec::new();
        (&mut file)
            .take(MAX_UPLOAD_SIZE as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| anyhow!("cannot read file_path"))?;
        body
    };
    ensure!(!body.is_empty(), "file body is empty");
    ensure!(
        body.len() <= MAX_UPLOAD_SIZE,
        "file body exceeds maximum upload size"
    );
    Ok(body)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadOperation {
    #[serde(default)]
    method: String,
    url: String,
    offset: u64,
    length: u64,
    #[serde(default)]
    request_headers: Vec<UploadHeader>,
}
#[derive(Deserialize)]
struct UploadHeader {
    name: String,
    value: String,
}
fn upload_url(s: &str, allow_local: bool) -> Result<url::Url> {
    let url = url::Url::parse(s).map_err(|_| anyhow!("invalid upload URL"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
        "invalid upload URL credentials or fragment"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("upload URL has no hostname"))?;
    if allow_local && matches!(host, "127.0.0.1" | "localhost" | "[::1]") && url.scheme() == "http"
    {
        return Ok(url);
    }
    ensure!(
        url.scheme() == "https" && url.port_or_known_default() == Some(443),
        "upload URL must use HTTPS on port 443"
    );
    ensure!(
        ["apple.com", "icloud.com", "mzstatic.com"]
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}"))),
        "upload URL hostname is not an Apple storage host"
    );
    Ok(url)
}
async fn perform_upload(
    http: &reqwest::Client,
    operations: &[UploadOperation],
    body: &[u8],
    allow_local: bool,
) -> Result<()> {
    ensure!(!operations.is_empty(), "no upload operations to perform");
    // Validate the complete plan before issuing any chunk, preventing partial
    // delivery caused by a malformed later operation.
    let mut requests = Vec::new();
    for (i, op) in operations.iter().enumerate() {
        let end = op
            .offset
            .checked_add(op.length)
            .ok_or_else(|| anyhow!("upload operation {i} range overflow"))?;
        ensure!(
            op.length > 0 && end <= body.len() as u64,
            "upload operation {i} out of bounds"
        );
        let url = upload_url(&op.url, allow_local)?;
        let method = if op.method.is_empty() {
            reqwest::Method::PUT
        } else {
            reqwest::Method::from_bytes(op.method.as_bytes())
                .map_err(|_| anyhow!("invalid upload method"))?
        };
        ensure!(
            method == reqwest::Method::PUT || method == reqwest::Method::POST,
            "unsupported upload method"
        );
        let mut headers = reqwest::header::HeaderMap::new();
        for h in &op.request_headers {
            let name = reqwest::header::HeaderName::from_bytes(h.name.as_bytes())
                .map_err(|_| anyhow!("invalid upload header name"))?;
            ensure!(
                !matches!(
                    name.as_str(),
                    "authorization"
                        | "cookie"
                        | "proxy-authorization"
                        | "host"
                        | "connection"
                        | "transfer-encoding"
                ),
                "forbidden upload header"
            );
            let value = reqwest::header::HeaderValue::from_str(&h.value)
                .map_err(|_| anyhow!("invalid upload header value"))?;
            if name == reqwest::header::CONTENT_LENGTH {
                ensure!(
                    h.value.parse::<u64>().ok() == Some(op.length),
                    "upload Content-Length does not match chunk size"
                );
            }
            headers.insert(name, value);
        }
        requests.push((method, url, headers, op.offset as usize, end as usize));
    }
    for (i, (method, url, headers, start, end)) in requests.into_iter().enumerate() {
        let response = http
            .request(method, url)
            .headers(headers)
            .timeout(Duration::from_secs(300))
            .body(body[start..end].to_vec())
            .send()
            .await
            .map_err(|_| anyhow!("upload operation {i} transport failed"))?;
        ensure!(
            response.status().is_success(),
            "upload operation {i} failed with HTTP {}",
            response.status().as_u16()
        );
        // No response body is required; dropping it avoids unbounded buffering.
    }
    Ok(())
}
fn result(text: String, data: Option<Value>) -> Value {
    let mut value = json!({"content":[{"type":"text","text":text}]});
    if let Some(data) = data {
        value["structuredContent"] = data;
    }
    value
}

pub async fn handle(client: &ApiClient, name: &str, args: &Value) -> Result<Option<Value>> {
    handle_inner(client, name, args, false).await
}

async fn handle_inner(
    client: &ApiClient,
    name: &str,
    args: &Value,
    allow_local: bool,
) -> Result<Option<Value>> {
    if matches!(name, "get_sales_report" | "get_finance_report") {
        let sales = name == "get_sales_report";
        let pairs: &[(&str, &str)] = if sales {
            &[
                ("vendor_number", "vendorNumber"),
                ("report_type", "reportType"),
                ("report_sub_type", "reportSubType"),
                ("frequency", "frequency"),
                ("report_date", "reportDate"),
            ]
        } else {
            &[
                ("vendor_number", "vendorNumber"),
                ("region_code", "regionCode"),
                ("report_type", "reportType"),
                ("report_date", "reportDate"),
            ]
        };
        let mut query = Vec::new();
        for (arg, key) in pairs {
            query.push((format!("filter[{key}]"), required(args, arg)?.to_owned()));
        }
        let response = client
            .raw_request(
                "GET",
                if sales {
                    "/v1/salesReports"
                } else {
                    "/v1/financeReports"
                },
                query,
                None,
            )
            .await?;
        let preview = String::from_utf8_lossy(&response.body[..response.body.len().min(1000)]);
        let suffix = if response.body.len() > 1000 {
            "..."
        } else {
            ""
        };
        return Ok(Some(result(
            format!(
                "{} report downloaded ({} bytes). Data is gzip-compressed TSV format.\n\nFirst 1000 bytes:\n{preview}{suffix}",
                if sales { "Sales" } else { "Finance" },
                response.body.len()
            ),
            None,
        )));
    }
    let (resource, version, rel, parent_type, parent_arg, label, checksum) = match name {
        "upload_app_screenshot" => (
            "appScreenshots",
            "v1",
            "appScreenshotSet",
            "appScreenshotSets",
            "screenshot_set_id",
            "screenshot",
            0,
        ),
        "upload_app_preview" => (
            "appPreviews",
            "v1",
            "appPreviewSet",
            "appPreviewSets",
            "preview_set_id",
            "preview",
            0,
        ),
        "upload_review_attachment" => (
            "appStoreReviewAttachments",
            "v1",
            "appStoreReviewDetail",
            "appStoreReviewDetails",
            "review_detail_id",
            "review attachment",
            0,
        ),
        "upload_build_file" => (
            "buildUploadFiles",
            "v1",
            "buildUpload",
            "buildUploads",
            "build_upload_id",
            "build file",
            1,
        ),
        "upload_background_asset_file" => (
            "backgroundAssetUploadFiles",
            "v1",
            "backgroundAssetVersion",
            "backgroundAssetVersions",
            "version_id",
            "background asset file",
            1,
        ),
        "upload_game_center_leaderboard_set_image" => (
            "gameCenterLeaderboardSetImages",
            "v2",
            "localization",
            "gameCenterLeaderboardSetLocalizations",
            "localization_id",
            "leaderboard set image",
            2,
        ),
        "upload_game_center_activity_image" => (
            "gameCenterActivityImages",
            "v1",
            "localization",
            "gameCenterActivityLocalizations",
            "localization_id",
            "activity image",
            2,
        ),
        "upload_game_center_challenge_image" => (
            "gameCenterChallengeImages",
            "v1",
            "localization",
            "gameCenterChallengeLocalizations",
            "localization_id",
            "challenge image",
            2,
        ),
        _ => return Ok(None),
    };
    let file_name = required(args, "file_name")?;
    let mut rel = rel;
    let mut parent_type = parent_type.to_owned();
    let parent = if matches!(
        name,
        "upload_game_center_activity_image" | "upload_game_center_challenge_image"
    ) {
        let localization = string(args, "localization_id")?;
        let version = string(args, "version_id")?;
        ensure!(
            localization.is_empty() != version.is_empty(),
            "provide exactly one of localization_id or version_id"
        );
        if !version.is_empty() {
            rel = "version";
            parent_type = parent_type.replace("Localizations", "Versions");
            version
        } else {
            localization
        }
    } else {
        required(args, parent_arg)?
    };
    identifier(parent)?;
    let body = load_body(args)?;
    let mut attributes = json!({"fileName":file_name,"fileSize":body.len()});
    if name == "upload_app_preview" {
        let mime = string(args, "mime_type")?;
        attributes["mimeType"] = json!(if mime.is_empty() { "video/mp4" } else { mime });
    }
    if checksum == 1 {
        let asset = string(args, "asset_type")?;
        let asset = if asset.is_empty() { "ASSET" } else { asset };
        let allowed = if name == "upload_build_file" {
            &["ASSET", "ASSET_DESCRIPTION", "ASSET_SPI"][..]
        } else {
            &["ASSET", "MANIFEST"][..]
        };
        ensure!(allowed.contains(&asset), "invalid asset_type");
        attributes["assetType"] = json!(asset);
        if name == "upload_build_file" {
            let uti = string(args, "uti")?;
            let uti = if uti.is_empty() { "com.apple.ipa" } else { uti };
            ensure!(
                [
                    "com.apple.ipa",
                    "com.apple.pkg",
                    "com.pkware.zip-archive",
                    "com.apple.binary-property-list",
                    "com.apple.xml-property-list"
                ]
                .contains(&uti),
                "invalid uti"
            );
            attributes["uti"] = json!(uti);
        }
    }
    let path = format!("/{version}/{resource}");
    let reservation = client.request("POST",&path,Vec::new(),Some(json!({"data":{"type":resource,"attributes":attributes,"relationships":{rel:{"data":{"type":parent_type,"id":parent}}}}}))).await?;
    let id = reservation["data"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("reservation has no resource identifier"))?;
    identifier(id)?;
    let operations: Vec<UploadOperation> =
        serde_json::from_value(reservation["data"]["attributes"]["uploadOperations"].clone())
            .map_err(|_| anyhow!("reservation contains invalid upload operations"))?;
    perform_upload(&client.http, &operations, &body, allow_local).await?;
    let mut commit_attributes = json!({"uploaded":true});
    let hash = hex::encode(Sha256::digest(&body));
    if checksum == 0 {
        commit_attributes["sourceFileChecksum"] = json!(hash);
    }
    if checksum == 1 {
        commit_attributes["sourceFileChecksums"] =
            json!({"file":{"hash":hash,"algorithm":"SHA_256"}});
    }
    let response = client
        .request(
            "PATCH",
            &format!("{path}/{id}"),
            Vec::new(),
            Some(json!({"data":{"type":resource,"id":id,"attributes":commit_attributes}})),
        )
        .await?;
    let mut text = format!("Uploaded {label} {id} ({file_name}, {} bytes).", body.len());
    if name == "upload_build_file" {
        text.push_str(" Poll get_build_upload for processing status.");
    }
    if name == "upload_background_asset_file" {
        text.push_str(" Poll get_background_asset_version for processing status.");
    }
    Ok(Some(result(text, Some(response["data"].clone()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{
        Method::{PATCH, POST, PUT},
        MockServer,
    };

    #[test]
    fn validates_signed_urls_without_disclosing_them() {
        for url in [
            "http://upload.apple.com/file?token=secret",
            "https://apple.com.evil.test/?token=secret",
            "https://127.0.0.1/file",
            "https://user:secret@upload.apple.com/file",
            "https://upload.apple.com:444/file",
            "https://evilapple.com/file",
        ] {
            let error = upload_url(url, false).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        assert!(upload_url("https://upload.apple.com/file?token=secret", false).is_ok());
    }
    #[test]
    fn bounded_upload_sources_and_flexible_base64() {
        for encoding in [
            general_purpose::STANDARD,
            general_purpose::STANDARD_NO_PAD,
            general_purpose::URL_SAFE,
            general_purpose::URL_SAFE_NO_PAD,
        ] {
            assert_eq!(
                load_body(&json!({"file_data_base64":encoding.encode([255,254,253,252])})).unwrap(),
                [255, 254, 253, 252]
            );
        }
        assert!(load_body(&json!({})).is_err());
        assert!(load_body(&json!({"file_path":"foo","file_data_base64":"YQ=="})).is_err());
        assert!(load_body(&json!({"file_data_base64":"%%%"})).is_err());
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_UPLOAD_SIZE as u64 + 1).unwrap();
        assert!(load_body(&json!({"file_path":file.path()})).is_err());
        assert!(load_body(&json!({"file_path":std::env::temp_dir()})).is_err());
    }
    #[tokio::test]
    async fn reserve_chunk_commit_and_checksums() {
        for (tool, resource, parent_key, parent_type, checksums) in [
            (
                "upload_app_screenshot",
                "appScreenshots",
                "appScreenshotSet",
                "appScreenshotSets",
                false,
            ),
            (
                "upload_build_file",
                "buildUploadFiles",
                "buildUpload",
                "buildUploads",
                true,
            ),
        ] {
            let server = MockServer::start_async().await;
            let mut attributes = json!({"fileName":"file.bin","fileSize":6});
            if checksums {
                attributes["assetType"] = json!("ASSET");
                attributes["uti"] = json!("com.apple.ipa");
            }
            let reservation = server.mock_async(|when,then| {
                when.method(POST).path(format!("/v1/{resource}"))
                    .json_body(json!({"data":{"type":resource,"attributes":attributes,"relationships":{parent_key:{"data":{"type":parent_type,"id":"parent"}}}}}));
                then.status(201).json_body(json!({"data":{"type":resource,"id":"asset","attributes":{"uploadOperations":[
                    {"url":server.url("/one?signature=secret"),"offset":0,"length":2,"requestHeaders":[{"name":"x-upload","value":"first"}]},
                    {"url":server.url("/two"),"method":"PUT","offset":2,"length":4}
                ]}}}));
            }).await;
            let first = server
                .mock_async(|when, then| {
                    when.method(PUT)
                        .path("/one")
                        .body("ab")
                        .header("x-upload", "first")
                        .is_true(|req| {
                            !req.headers().iter().any(|(key, _)| {
                                key.as_str().eq_ignore_ascii_case("authorization")
                                    || key.as_str().eq_ignore_ascii_case("cookie")
                            })
                        });
                    then.status(200);
                })
                .await;
            let second = server
                .mock_async(|when, then| {
                    when.method(PUT).path("/two").body("cdef");
                    then.status(204);
                })
                .await;
            let hash = hex::encode(Sha256::digest(b"abcdef"));
            let mut attrs = json!({"uploaded":true});
            if checksums {
                attrs["sourceFileChecksums"] = json!({"file":{"hash":hash,"algorithm":"SHA_256"}});
            } else {
                attrs["sourceFileChecksum"] = json!(hash);
            }
            let commit = server.mock_async(|when,then| { when.method(PATCH).path(format!("/v1/{resource}/asset")).json_body(json!({"data":{"type":resource,"id":"asset","attributes":attrs}})); then.status(200).json_body(json!({"data":{"type":resource,"id":"asset","attributes":{"uploaded":true}}})); }).await;
            let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
            let args = json!({"screenshot_set_id":"parent","build_upload_id":"parent","file_name":"file.bin","file_data_base64":"YWJjZGVm"});
            let result = handle_inner(&client, tool, &args, true)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result["structuredContent"]["id"], "asset");
            reservation.assert_async().await;
            first.assert_async().await;
            second.assert_async().await;
            commit.assert_async().await;
        }
    }
    #[tokio::test]
    async fn failed_chunk_is_not_retried_or_committed_and_error_is_sanitized() {
        let server = MockServer::start_async().await;
        server.mock_async(|when,then| { when.method(POST).path("/v1/appScreenshots"); then.status(201).json_body(json!({"data":{"id":"asset","attributes":{"uploadOperations":[{"url":server.url("/upload?signature=secret"),"offset":0,"length":1}]}}})); }).await;
        let chunk = server
            .mock_async(|when, then| {
                when.method(PUT).path("/upload");
                then.status(500).body("signed secret private error body");
            })
            .await;
        let commit = server
            .mock_async(|when, then| {
                when.method(PATCH);
                then.status(200);
            })
            .await;
        let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
        let error = handle_inner(
            &client,
            "upload_app_screenshot",
            &json!({"screenshot_set_id":"parent","file_name":"file.bin","file_data_base64":"YQ=="}),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("HTTP 500"));
        assert!(!error.contains("secret"));
        chunk.assert_calls_async(1).await;
        commit.assert_calls_async(0).await;
    }
    #[tokio::test]
    async fn validates_whole_chunk_plan_before_network() {
        let server = MockServer::start_async().await;
        let chunk = server
            .mock_async(|when, then| {
                when.method(PUT);
                then.status(200);
            })
            .await;
        let operations: Vec<UploadOperation> = serde_json::from_value(json!([
            {"url":server.url("/one"),"offset":0,"length":1},
            {"url":server.url("/two"),"offset":u64::MAX,"length":2}
        ]))
        .unwrap();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert!(
            perform_upload(&http, &operations, b"abc", true)
                .await
                .is_err()
        );
        chunk.assert_calls_async(0).await;
        for name in ["Authorization", "Cookie", "Host"] {
            let operations: Vec<UploadOperation>=serde_json::from_value(json!([{"url":server.url("/one"),"offset":0,"length":1,"requestHeaders":[{"name":name,"value":"secret"}]}])).unwrap();
            assert!(
                perform_upload(&http, &operations, b"abc", true)
                    .await
                    .is_err()
            );
        }
    }
}
