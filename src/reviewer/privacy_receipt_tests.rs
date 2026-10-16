//! Local transport tests for the narrowly scoped privacy-create receipt policy.
use super::*;
use httpmock::prelude::{MockServer, POST};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CREATE_PATH: &str = "/iris/v1/appDataUsages";
const PRIVATE_BODY: &str = "private-non-json-receipt-sentinel";

fn create_payload() -> Value {
    json!({"data":{"type":"appDataUsages","relationships":{
        "app":{"data":{"type":"apps","id":"123"}},
        "category":{"data":{"type":"appDataUsageCategories","id":"DEVICE_ID"}},
        "purpose":{"data":{"type":"appDataUsagePurposes","id":"ANALYTICS"}},
        "dataProtection":{"data":{"type":"appDataUsageDataProtections","id":"DATA_LINKED_TO_YOU"}}
    }}})
}

fn receipt_error(result: Result<Raw>) -> String {
    match result {
        Ok(_) => panic!("a non-JSON response was accepted outside the scoped create policy"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                !message.contains(PRIVATE_BODY),
                "response body escaped into an error"
            );
            message
        }
    }
}

#[tokio::test]
async fn privacy_create_complete_non_json_201_keeps_cookie_rotation_and_private_sentinel() {
    let server = MockServer::start();
    let directory = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), directory.path());
    let source = fs::read(&reviewer.cookie_path).unwrap();
    let pin = reviewer.pin().await.unwrap();
    let create = server.mock(|when, then| {
        when.method(POST)
            .path(CREATE_PATH)
            .header("cookie", "session=secret-sentinel")
            .header("x-csrf-itc", "[asc-ui]")
            .json_body(create_payload());
        then.status(201)
            .header("content-type", "text/plain")
            .header(
                "set-cookie",
                "session=privacy-rotated-sentinel; Domain=apple.com; Path=/",
            )
            .body(PRIVATE_BODY);
    });

    let (data, headers) = reviewer
        .pinned_privacy_create(&pin, create_payload())
        .await
        .unwrap();
    assert_eq!(data, json!({"data":null}));
    assert_eq!(headers["x-asc-mcp-status"], "201");
    assert!(!headers.contains_key(reqwest::header::SET_COOKIE));
    assert!(!data.to_string().contains(PRIVATE_BODY));
    assert!(!pin.public("privacy-rotated-sentinel"));
    assert!(!pin.public("secret-sentinel"));
    assert_eq!(fs::read(&reviewer.cookie_path).unwrap(), source);
    let session = session::load(reviewer.cookie_path.clone(), reviewer.root.clone())
        .await
        .unwrap();
    assert_eq!(session.generation, pin.generation);
    assert!(
        session.cookies.iter().any(|cookie| {
            cookie.name == "session" && cookie.value == "privacy-rotated-sentinel"
        })
    );
    create.assert_calls(1);
}

#[tokio::test]
async fn privacy_receipt_policy_never_expands_to_other_request_shapes() {
    struct Case {
        name: &'static str,
        method: Method,
        path: &'static str,
        query: Option<&'static str>,
        fragment: Option<&'static str>,
        status: u16,
        pinned: bool,
        iris: bool,
    }
    let cases = [
        Case {
            name: "GET",
            method: Method::GET,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "PATCH",
            method: Method::PATCH,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "DELETE",
            method: Method::DELETE,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "another Iris create route",
            method: Method::POST,
            path: "/iris/v1/resolutionCenterMessages",
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "route suffix",
            method: Method::POST,
            path: "/iris/v1/appDataUsages/row1",
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "trailing slash",
            method: Method::POST,
            path: "/iris/v1/appDataUsages/",
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "query",
            method: Method::POST,
            path: CREATE_PATH,
            query: Some("include=category"),
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "empty query",
            method: Method::POST,
            path: CREATE_PATH,
            query: Some(""),
            fragment: None,
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "fragment",
            method: Method::POST,
            path: CREATE_PATH,
            query: None,
            fragment: Some("other"),
            status: 201,
            pinned: true,
            iris: true,
        },
        Case {
            name: "HTTP 200",
            method: Method::POST,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 200,
            pinned: true,
            iris: true,
        },
        Case {
            name: "HTTP 202",
            method: Method::POST,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 202,
            pinned: true,
            iris: true,
        },
        Case {
            name: "unpinned",
            method: Method::POST,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 201,
            pinned: false,
            iris: true,
        },
        Case {
            name: "non-Iris",
            method: Method::POST,
            path: CREATE_PATH,
            query: None,
            fragment: None,
            status: 201,
            pinned: true,
            iris: false,
        },
    ];
    for case in cases {
        let server = MockServer::start();
        let directory = tempfile::tempdir().unwrap();
        let reviewer = Reviewer::for_test(&server.base_url(), directory.path());
        let pin = reviewer.pin().await.unwrap();
        let response = server.mock(|when, then| {
            when.any_request();
            then.status(case.status).body(PRIVATE_BODY);
        });
        let mut url = reviewer.url(case.path, &[]).unwrap();
        url.set_query(case.query);
        url.set_fragment(case.fragment);
        let result = reviewer
            .fetch_with_policy(
                case.method,
                url,
                Some(create_payload()),
                None,
                case.iris,
                FetchPolicy {
                    pin: case.pinned.then_some(pin.generation.as_str()),
                    response: ResponsePolicy::PrivacyUsageCreate201,
                },
            )
            .await;
        let error = receipt_error(result);
        assert!(!error.is_empty(), "{}", case.name);
        assert!(response.calls() <= 1, "{} was retried", case.name);
    }
}

#[tokio::test]
async fn privacy_create_does_not_relax_existing_fetch_or_explicit_strict_json_policy() {
    let server = MockServer::start();
    let directory = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), directory.path());
    let pin = reviewer.pin().await.unwrap();
    let response = server.mock(|when, then| {
        when.method(POST).path(CREATE_PATH);
        then.status(201).body(PRIVATE_BODY);
    });
    let strict = reviewer
        .fetch(
            Method::POST,
            reviewer.url(CREATE_PATH, &[]).unwrap(),
            Some(create_payload()),
            None,
            true,
            Some(&pin.generation),
        )
        .await;
    let error = receipt_error(strict);
    assert!(
        error.contains("delivery_unknown")
            && error.contains("invalid JSON")
            && error.contains("201"),
        "{error}"
    );
    let strict = reviewer
        .fetch_with_policy(
            Method::POST,
            reviewer.url(CREATE_PATH, &[]).unwrap(),
            Some(create_payload()),
            None,
            true,
            FetchPolicy {
                pin: Some(&pin.generation),
                response: ResponsePolicy::StrictJson,
            },
        )
        .await;
    let error = receipt_error(strict);
    assert!(
        error.contains("delivery_unknown")
            && error.contains("invalid JSON")
            && error.contains("201"),
        "{error}"
    );
    response.assert_calls(2);
}

#[tokio::test]
async fn privacy_create_policy_preserves_valid_json_response() {
    let server = MockServer::start();
    let directory = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), directory.path());
    let pin = reviewer.pin().await.unwrap();
    let expected = json!({"data":{"type":"appDataUsages","id":"created-row"}});
    let response = server.mock(|when, then| {
        when.method(POST).path(CREATE_PATH);
        then.status(201).json_body(expected.clone());
    });
    let (data, headers) = reviewer
        .pinned_privacy_create(&pin, create_payload())
        .await
        .unwrap();
    assert_eq!(data, expected);
    assert_eq!(headers["x-asc-mcp-status"], "201");
    response.assert_calls(1);
}

#[tokio::test]
async fn privacy_create_oversized_non_json_201_stays_unknown_without_retry() {
    let server = MockServer::start();
    let directory = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), directory.path());
    let pin = reviewer.pin().await.unwrap();
    let response = server.mock(|when, then| {
        when.method(POST).path(CREATE_PATH);
        then.status(201).body(vec![b'x'; MAX_BODY + 1]);
    });
    let error = reviewer
        .pinned_privacy_create(&pin, create_payload())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("delivery_unknown")
            && error.contains("oversized response")
            && error.contains("201"),
        "{error}"
    );
    response.assert_calls(1);
}

struct ReceiptServerTask(Option<tokio::task::JoinHandle<()>>);
impl Drop for ReceiptServerTask {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

#[tokio::test]
async fn privacy_create_interrupted_non_json_201_stays_unknown_without_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut server = ReceiptServerTask(Some(tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0, "client closed before sending its request");
            request.extend_from_slice(&buffer[..count]);
            assert!(
                request.len() <= 64 * 1024,
                "fixture request exceeded its bound"
            );
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = std::str::from_utf8(&request[..end]).unwrap();
                assert!(header.starts_with("POST /iris/v1/appDataUsages HTTP/1.1\r\n"));
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    })
                    .map(|(_, value)| value.trim().parse().unwrap())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        // A complete status/header block followed by an incomplete declared body.
        // Closing the connection must not convert this into a completed receipt.
        stream.write_all(b"HTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nContent-Length: 1024\r\nConnection: close\r\n\r\npartial-private-response").await.unwrap();
        stream.shutdown().await.unwrap();
    })));
    let directory = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&origin, directory.path());
    let pin = reviewer.pin().await.unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        reviewer.pinned_privacy_create(&pin, create_payload()),
    )
    .await
    .expect("interrupted-response fixture must finish promptly")
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("delivery_unknown")
            && error.contains("interrupted response")
            && error.contains("201"),
        "{error}"
    );
    assert!(!error.contains("partial-private-response"));
    server.0.take().unwrap().await.unwrap();
}
