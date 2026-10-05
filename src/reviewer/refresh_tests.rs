use super::*;
use httpmock::prelude::{GET, MockServer};

#[tokio::test]
async fn refresh_survives_client_restart_without_changing_source() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let reviewer = Reviewer::for_test(&server.base_url(), dir.path());
    let source = fs::read(&reviewer.cookie_path).unwrap();
    let first = server.mock(|when, then| {
        when.method(GET)
            .path("/first")
            .header("cookie", "session=secret-sentinel");
        then.header(
            "set-cookie",
            "session=rotated-sentinel; Domain=apple.com; Path=/",
        )
        .json_body(json!({"data":[]}));
    });
    reviewer
        .request(
            Method::GET,
            reviewer.url("/first", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(fs::read(&reviewer.cookie_path).unwrap(), source);
    let second = server.mock(|when, then| {
        when.method(GET)
            .path("/second")
            .header("cookie", "session=rotated-sentinel");
        then.json_body(json!({"data":[]}));
    });
    let restarted = Reviewer::for_test(&server.base_url(), dir.path());
    restarted
        .request(
            Method::GET,
            restarted.url("/second", &[]).unwrap(),
            None,
            None,
        )
        .await
        .unwrap();
    first.assert();
    second.assert();
}

#[tokio::test]
async fn forbidden_does_not_claim_expired_authentication() {
    let server = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test(&server.base_url(), dir.path());
    server.mock(|when, then| {
        when.method(GET);
        then.status(403).body("private-body");
    });
    let err = r
        .request(Method::GET, r.url("/", &[]).unwrap(), None, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("reviewer_forbidden"), "{err}");
    assert!(!err.contains("private-body"));
}

fn headers(raw: &[&str]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for v in raw {
        h.append(reqwest::header::SET_COOKIE, v.parse().unwrap());
    }
    h
}

#[tokio::test]
async fn multiple_response_headers_apply_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test("http://127.0.0.1:1", dir.path());
    let s = session::load(r.cookie_path.clone(), r.root.clone())
        .await
        .unwrap();
    let delta = session::updates(
        &headers(&[
            "session=temporary; Domain=apple.com; Path=/",
            "session=; Domain=apple.com; Path=/; Max-Age=0",
        ]),
        &r.origin,
        &s.cookies,
    );
    session::persist(r.cookie_path.clone(), r.root.clone(), s.generation, delta)
        .await
        .unwrap();
    assert!(
        session::load(r.cookie_path.clone(), r.root.clone())
            .await
            .unwrap()
            .cookies
            .is_empty()
    );
}

#[test]
fn cookie_expiry_scope_and_prefixes() {
    let u = Url::parse("https://appstoreconnect.apple.com/iris/v1/apps").unwrap();
    let h = headers(&[
        "accepted=yes; Domain=.apple.com; Path=/; Max-Age=60; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        "foreign=bad; Domain=evil.example; Path=/",
        "scoped=bad; Domain=apple.com; Path=/iris",
        "defaultpath=bad; Domain=apple.com",
        "__Secure-bad=bad; Domain=apple.com; Path=/",
        "__Host-bad=bad; Domain=apple.com; Path=/; Secure",
        "__Host-good=yes; Path=/; Secure",
        "huge=bad; Domain=apple.com; Path=/; Max-Age=9223372036854775807",
    ]);
    let updates = session::updates(&h, &u, &[]);
    let names: Vec<_> = updates
        .iter()
        .filter_map(|d| d.cookie().map(|c| c.name.as_str()))
        .collect();
    assert_eq!(names, ["accepted", "__Host-good"]);
    let insecure = Url::parse("http://appstoreconnect.apple.com/").unwrap();
    assert!(
        session::updates(
            &headers(&["secret=value; Domain=apple.com; Path=/; Secure"]),
            &insecure,
            &[]
        )
        .is_empty()
    );
}

#[test]
fn unrepresentable_future_expiry_is_ignored_not_deleted() {
    let u = Url::parse("https://appstoreconnect.apple.com/").unwrap();
    let delta = session::updates(
        &headers(&["session=value; Domain=apple.com; Path=/; Max-Age=1000000000000"]),
        &u,
        &[],
    );
    assert!(delta.is_empty());
}

#[tokio::test]
async fn deletion_does_not_resurrect_source_and_changed_source_wins() {
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test("http://127.0.0.1:1", dir.path());
    let snapshot = session::load(r.cookie_path.clone(), r.root.clone())
        .await
        .unwrap();
    let update = session::updates(
        &headers(&["session=; Domain=apple.com; Path=/; Max-Age=0"]),
        &r.origin,
        &snapshot.cookies,
    );
    session::persist(
        r.cookie_path.clone(),
        r.root.clone(),
        snapshot.generation.clone(),
        update,
    )
    .await
    .unwrap();
    let deleted = session::load(r.cookie_path.clone(), r.root.clone())
        .await
        .unwrap();
    assert!(deleted.cookies.is_empty());
    atomic_json(&r.cookie_path, &json!({"cookies":[{"name":"session","value":"fresh-login","domain":"apple.com","path":"/"}]})).unwrap();
    let stale = session::updates(
        &headers(&["session=stale-response; Domain=apple.com; Path=/"]),
        &r.origin,
        &snapshot.cookies,
    );
    session::persist(
        r.cookie_path.clone(),
        r.root.clone(),
        snapshot.generation,
        stale,
    )
    .await
    .unwrap();
    let fresh = session::load(r.cookie_path.clone(), r.root.clone())
        .await
        .unwrap();
    assert_eq!(
        cookie_header(&fresh.cookies).unwrap(),
        "session=fresh-login"
    );
    fs::remove_file(&r.cookie_path).unwrap();
    assert!(
        session::load(r.cookie_path.clone(), r.root.clone())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_deltas_preserve_unrelated_cookies_and_newer_deletion_target() {
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test("http://127.0.0.1:1", dir.path());
    let snapshot = session::load(r.cookie_path.clone(), r.root.clone())
        .await
        .unwrap();
    let mut jobs = Vec::new();
    for i in 0..12 {
        let updates = session::updates(
            &headers(&[&format!("c{i}=value{i}; Domain=apple.com; Path=/")]),
            &r.origin,
            &snapshot.cookies,
        );
        jobs.push(tokio::spawn(session::persist(
            r.cookie_path.clone(),
            r.root.clone(),
            snapshot.generation.clone(),
            updates,
        )));
    }
    for j in jobs {
        j.await.unwrap().unwrap();
    }
    let rotated = session::updates(
        &headers(&["session=rotated; Domain=apple.com; Path=/"]),
        &r.origin,
        &snapshot.cookies,
    );
    session::persist(
        r.cookie_path.clone(),
        r.root.clone(),
        snapshot.generation.clone(),
        rotated,
    )
    .await
    .unwrap();
    let stale_deletion = session::updates(
        &headers(&["session=; Domain=apple.com; Path=/; Max-Age=0"]),
        &r.origin,
        &snapshot.cookies,
    );
    session::persist(
        r.cookie_path.clone(),
        r.root.clone(),
        snapshot.generation,
        stale_deletion,
    )
    .await
    .unwrap();
    let merged = session::load(r.cookie_path.clone(), r.root.clone())
        .await
        .unwrap();
    assert_eq!(merged.cookies.len(), 13);
    assert!(
        merged
            .cookies
            .iter()
            .any(|c| c.name == "session" && c.value == "rotated")
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(r.root.join("session.state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn errors_refresh_once_and_redact_rotated_cookie() {
    for status in [200, 401, 403, 500] {
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let r = Reviewer::for_test(&server.base_url(), dir.path());
        let m = server.mock(|w, t| {
            w.method(httpmock::Method::POST).path("/request");
            t.status(status)
                .header(
                    "set-cookie",
                    "session=rotated-sentinel; Domain=apple.com; Path=/",
                )
                .json_body(json!({"echo":"rotated-sentinel","old":"secret-sentinel"}));
        });
        let result = r
            .request(
                Method::POST,
                r.url("/request", &[]).unwrap(),
                Some(json!({})),
                None,
            )
            .await;
        if status == 200 {
            let (body, h) = result.unwrap();
            assert!(!body.to_string().contains("sentinel"));
            assert!(!h.contains_key(reqwest::header::SET_COOKIE));
        } else {
            let error = result.unwrap_err().to_string();
            assert!(!error.contains("sentinel"));
            if status == 500 {
                assert!(error.contains("delivery_unknown"));
            }
        }
        let state = session::load(r.cookie_path.clone(), r.root.clone())
            .await
            .unwrap();
        assert_eq!(state.cookies[0].value, "rotated-sentinel");
        m.assert_calls(1);
    }
}

#[tokio::test]
async fn redirects_are_classified_without_following_or_exposing_location() {
    for (location, code) in [
        (
            "https://idmsa.apple.com/login?secret=hidden",
            "reviewer_auth_required",
        ),
        ("/login", "reviewer_auth_required"),
        (
            "https://evil.example/?secret=hidden",
            "reviewer_unexpected_redirect",
        ),
    ] {
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let r = Reviewer::for_test(&server.base_url(), dir.path());
        let m = server.mock(|w, t| {
            w.method(GET).path("/request");
            t.status(302)
                .header("location", location)
                .header("set-cookie", "extra=value; Domain=apple.com; Path=/");
        });
        let e = r
            .request(Method::GET, r.url("/request", &[]).unwrap(), None, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains(code));
        assert!(!e.contains("hidden"));
        assert!(
            session::load(r.cookie_path.clone(), r.root.clone())
                .await
                .unwrap()
                .cookies
                .iter()
                .any(|c| c.name == "extra")
        );
        m.assert_calls(1);
    }
}

#[tokio::test]
async fn foreign_origin_is_rejected_before_reading_credentials() {
    let server = MockServer::start();
    let foreign = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let r = Reviewer::for_test(&server.base_url(), dir.path());
    fs::remove_file(&r.cookie_path).unwrap();
    let any = foreign.mock(|_, t| {
        t.json_body(json!({}));
    });
    let e = r
        .request(
            Method::GET,
            Url::parse(&foreign.base_url()).unwrap(),
            None,
            None,
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(e, "reviewer_untrusted_origin");
    any.assert_calls(0);
}
