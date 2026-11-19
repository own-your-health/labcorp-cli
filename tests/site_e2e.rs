//! The portal reads end to end, in-process, against a loopback server that
//! answers with the synthetic fixtures in `tests/fixtures/site/` (their
//! README has the household) and plays Okta's renewal. The session is
//! built in the test and a renewed one is kept in memory (`Kept`): no
//! keychain item or vault is read or written, and nothing is contacted
//! beyond 127.0.0.1.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use labcorp::client::{Client, Endpoints, Keep};
use labcorp::commands::{self, Site};
use labcorp::session::Session;
use pk_cli_config::profile::ProfileName;
use pk_cli_core::CliError;
use pk_cli_secrets::Secret;
use pk_med_records::{Item, RecordsReader, StreamError, Trailer};
use serde_json::Value;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/site");
const AGENT: &str = "Mozilla/5.0 (synthetic test browser)";
/// The bearer the server accepts before a renewal, and the one Okta's
/// renewal hands out.
const LIVE: &str = "dummy-live-token";
const RENEWED: &str = "dummy-renewed-token";
/// The Okta cookie whose session still lasts, and one whose has ended.
const OKTA_LIVE: &str = "idx=dummy-okta-live; DT=dummy-device";
const OKTA_ENDED: &str = "idx=dummy-okta-ended; DT=dummy-device";
/// A PDF as `verify_pdf` sees one; never a tracked file.
const PDF: &[u8] = b"%PDF-1.4\n% SYNTHETIC TEST DATA - NOT A REAL REPORT\n%%EOF\n";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: String,
}

/// After this many requests, answer every API read with this status.
#[derive(Clone, Copy)]
struct Fault {
    after: usize,
    status: u16,
}

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    fault: Arc<Mutex<Option<Fault>>>,
}

impl Server {
    fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fault = Arc::new(Mutex::new(None));
        let (s, f) = (seen.clone(), fault.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (s, f) = (s.clone(), f.clone());
                std::thread::spawn(move || serve(stream, &s, &f));
            }
        });
        Server { base, seen, fault }
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            api_base: self.base.clone(),
            issuer: format!("{}/oauth2/default", self.base),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.target).collect()
    }

    fn fail_after(&self, after: usize, status: u16) {
        *self.fault.lock().unwrap() = Some(Fault { after, status });
    }
}

fn respond(stream: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &[u8]) {
    let mut head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn json(stream: &mut TcpStream, status: u16, body: &str) {
    respond(
        stream,
        status,
        &[("Content-Type", "application/json".into())],
        body.as_bytes(),
    );
}

fn query(target: &str, key: &str) -> Option<String> {
    let q = target.split_once('?')?.1;
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>, fault: &Mutex<Option<Fault>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if reader.read(&mut byte).unwrap_or(0) != 1 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).to_string();
    let n = {
        let mut s = seen.lock().unwrap();
        s.push(Seen {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
            body: body.clone(),
        });
        s.len()
    };
    let path = target.split('?').next().unwrap_or("").to_string();

    // Okta.
    if path == "/oauth2/default/v1/authorize" {
        let state = query(&target, "state").unwrap_or_default();
        let cookie = headers.get("cookie").cloned().unwrap_or_default();
        let location = if cookie.contains("idx=dummy-okta-live") {
            format!("https://patient.labcorp.com/callback?code=dummy-code&state={state}")
        } else {
            format!("https://patient.labcorp.com/callback?state={state}&error=login_required&error_description=x")
        };
        respond(
            &mut stream,
            302,
            &[
                ("Location", location),
                ("Set-Cookie", "idx=dummy-okta-live; Path=/; Secure; HttpOnly".into()),
            ],
            b"",
        );
        return;
    }
    if path == "/oauth2/default/v1/token" {
        if !body.contains("code=dummy-code") || !body.contains("code_verifier=") {
            json(&mut stream, 400, r#"{"error":"invalid_grant"}"#);
            return;
        }
        json(
            &mut stream,
            200,
            &format!(
                r#"{{"access_token":"{RENEWED}","token_type":"Bearer","expires_in":3600,"scope":"openid profile email"}}"#
            ),
        );
        return;
    }

    // The portal API.
    if let Some(f) = *fault.lock().unwrap() {
        if n > f.after {
            json(&mut stream, f.status, r#"{"message":"dummy refusal"}"#);
            return;
        }
    }
    let auth = headers.get("authorization").cloned().unwrap_or_default();
    if auth != format!("Bearer {LIVE}") && auth != format!("Bearer {RENEWED}") {
        json(&mut stream, 401, r#"{"message":"Unauthorized"}"#);
        return;
    }
    if method != "GET" {
        json(&mut stream, 405, "{}");
        return;
    }
    match path.as_str() {
        "/protected/patients/current" => json(&mut stream, 200, &fixture("account.json")),
        "/protected/patients/current/linkedAccounts" => json(&mut stream, 200, &fixture("linked-accounts.json")),
        "/protected/patients/current/linkedAccounts/results/headers/all" => {
            json(&mut stream, 200, &fixture("headers-all.json"))
        }
        p => {
            let tail = p.rsplit("/results/").next().unwrap_or("");
            if let Some(id) = tail.strip_suffix("/pdf") {
                if id == "SYNR00000003" {
                    respond(
                        &mut stream,
                        200,
                        &[("Content-Type", "text/html".into())],
                        b"<html><body>dummy error page</body></html>",
                    );
                } else {
                    respond(&mut stream, 200, &[("Content-Type", "application/pdf".into())], PDF);
                }
                return;
            }
            match std::fs::read_to_string(format!("{FIXTURES}/result-{tail}.json")) {
                Ok(text) => json(&mut stream, 200, &text),
                Err(_) => json(&mut stream, 404, r#"{"message":"Not Found"}"#),
            }
        }
    }
}

/// A kept session: its access token, renewal time and Okta cookies.
type KeptSession = (String, Option<String>, Option<String>);

/// The sessions a client kept.
#[derive(Default)]
struct Kept(Mutex<Vec<KeptSession>>);

impl Keep for Kept {
    fn keep(&self, _profile: &ProfileName, s: &Session) -> Result<(), CliError> {
        self.0.lock().unwrap().push((
            s.access_token.expose().to_string(),
            s.renewed_at.clone(),
            s.okta_cookies.as_ref().map(|c| c.expose().to_string()),
        ));
        Ok(())
    }
}

fn session(token: &str, expires_at: u64, okta: Option<&str>) -> Session {
    Session {
        access_token: Secret::new(token.to_string()),
        expires_at,
        okta_cookies: okta.map(|c| Secret::new(c.to_string())),
        api_cookies: Some(Secret::new("dummy_api=1".to_string())),
        user_agent: Some(AGENT.into()),
        captured_at: "2026-01-01T00:00:00Z".into(),
        renewed_at: None,
    }
}

const FAR: u64 = 4_102_444_800;

fn profile() -> ProfileName {
    ProfileName::parse("adult1").unwrap()
}

/// One person's export from the server, read back: the resources by type
/// and the trailer.
fn export(server: &Server, s: Session, kept: &Kept, who: &str) -> (Vec<Value>, Trailer) {
    let client = Client::new(&profile(), s, kept, server.endpoints()).unwrap();
    let mut site = Site { client };
    let people = site.household().unwrap();
    let p = people.iter().find(|p| p.id == who).unwrap().clone();
    let headers = site.headers().unwrap();
    let mut out = Vec::new();
    let finished = commands::stream(&mut site, &people, &p, headers, None, true, &mut out).unwrap();
    let mut reader = RecordsReader::new(std::io::Cursor::new(out));
    reader.header().unwrap();
    let mut resources = Vec::new();
    for item in reader.by_ref() {
        match item {
            Ok(Item::Resource(r)) => resources.push(r.value().unwrap()),
            Ok(Item::Trailer(_)) => {}
            // An incomplete stream ends in `Incomplete`, every resource
            // before it yielded.
            Err(StreamError::Incomplete { .. }) if !finished.trailer.complete => break,
            Err(e) => panic!("{e}"),
        }
    }
    (resources, finished.trailer)
}

fn of<'a>(resources: &'a [Value], kind: &str) -> Vec<&'a Value> {
    resources.iter().filter(|r| r["resourceType"] == kind).collect()
}

#[test]
fn an_export_carries_the_persons_results_values_and_pdfs() {
    let server = Server::start();
    let kept = Kept::default();
    let (rs, trailer) = export(&server, session(LIVE, FAR, Some(OKTA_LIVE)), &kept, "SYNP000001");
    assert!(trailer.complete, "{trailer:?}");
    let counts: Vec<(String, u64)> = trailer.counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    assert_eq!(
        counts,
        [
            ("Binary".into(), 1),
            ("DiagnosticReport".into(), 2),
            ("DocumentReference".into(), 1),
            ("Observation".into(), 9),
            ("Organization".into(), 1),
            ("Patient".into(), 1),
        ]
    );
    // Oldest collection first: the error page served for a PDF is skipped
    // as not a PDF; the upcoming result is skipped until it has values.
    let skipped: Vec<(String, String)> = trailer
        .skipped
        .iter()
        .map(|s| (s.id.clone().unwrap_or_default(), s.code.clone()))
        .collect();
    assert_eq!(
        skipped,
        [
            ("lcb-SYNR00000003".into(), "verify_failed".into()),
            ("lcr-SYNR00000002".into(), "not_ready".into()),
        ]
    );
    for r in &rs {
        assert_eq!(r["meta"]["source"], "labcorp:portal", "{}", r["resourceType"]);
    }
    let patient = of(&rs, "Patient")[0];
    assert_eq!(patient["id"], "lcp-SYNP000001");
    assert_eq!(patient["birthDate"], "1990-01-01");
    let reports = of(&rs, "DiagnosticReport");
    let r1 = reports.iter().find(|r| r["id"] == "lcr-SYNR00000001").unwrap();
    assert_eq!(r1["status"], "final");
    assert_eq!(r1["effectiveDateTime"], "2026-03-04");
    assert_eq!(r1["presentedForm"][0]["url"], "Binary/lcb-SYNR00000001");
    assert_eq!(r1["result"].as_array().unwrap().len(), 2);
    let r3 = reports.iter().find(|r| r["id"] == "lcr-SYNR00000003").unwrap();
    assert!(r3.get("presentedForm").is_none());
    let obs = of(&rs, "Observation");
    let a = obs
        .iter()
        .find(|o| o["id"] == "lco-SYNR00000001-000001-000002")
        .unwrap();
    assert_eq!(a["interpretation"][0]["coding"][0]["code"], "H");
    let b = obs
        .iter()
        .find(|o| o["id"] == "lco-SYNR00000001-000001-000003")
        .unwrap();
    assert_eq!(b["valueQuantity"]["comparator"], "<");
    assert_eq!(b["note"].as_array().unwrap().len(), 2);
    let d = obs
        .iter()
        .find(|o| o["id"] == "lco-SYNR00000001-000010-000011")
        .unwrap();
    assert_eq!(d["dataAbsentReason"]["text"], "Test Not Performed");
    let binary = of(&rs, "Binary")[0];
    assert_eq!(binary["contentType"], "application/pdf");
    // Every request was a GET with the bearer, the API's cookie and the
    // browser's user agent; no Okta request was needed.
    for s in server.seen() {
        assert_eq!(s.method, "GET", "{}", s.target);
        assert_eq!(s.headers["authorization"], format!("Bearer {LIVE}"));
        assert_eq!(s.headers["cookie"], "dummy_api=1");
        assert_eq!(s.headers["user-agent"], AGENT);
        assert_eq!(s.headers["origin"], "https://patient.labcorp.com");
        assert!(!s.target.starts_with("/oauth2"));
    }
    assert!(kept.0.lock().unwrap().is_empty(), "nothing renewed, nothing kept");
}

#[test]
fn a_spelled_value_keeps_its_spelling() {
    let server = Server::start();
    let kept = Kept::default();
    let client = Client::new(&profile(), session(LIVE, FAR, None), &kept, server.endpoints()).unwrap();
    let mut site = Site { client };
    let people = site.household().unwrap();
    let headers = site.headers().unwrap();
    let mut out = Vec::new();
    commands::stream(&mut site, &people, &people[0], headers, None, false, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains(r#""valueQuantity":{"value":1.50,"unit":"mg/dL"}"#));
    assert!(text.contains(r#""low":{"value":0.50,"unit":"mg/dL"}"#));
    assert!(!text.contains("labcorp-number"));
}

#[test]
fn dependents_and_linked_accounts_are_read_under_their_own_paths() {
    let server = Server::start();
    let kept = Kept::default();
    let (rs, t) = export(&server, session(LIVE, FAR, None), &kept, "SYNP000002");
    assert!(t.complete);
    assert_eq!(of(&rs, "DiagnosticReport").len(), 1);
    let (rs, t) = export(&server, session(LIVE, FAR, None), &kept, "SYNP000003");
    assert!(t.complete);
    assert_eq!(of(&rs, "Patient")[0]["name"][0]["given"][0], "Jordan");
    let paths = server.paths();
    assert!(paths.contains(&"/protected/patients/current/linkedAccounts/results/SYNR00000004".to_string()));
    assert!(paths.contains(&"/protected/patients/current/linkedAccounts/SYNL000003/results/SYNR00000005".to_string()));
    // The linked account still waiting on its owner is not read.
    assert!(!paths.iter().any(|p| p.contains("SYNL000004")));
}

#[test]
fn an_expired_token_is_renewed_from_oktas_session_and_kept() {
    let server = Server::start();
    let kept = Kept::default();
    let (_, t) = export(
        &server,
        session("dummy-old-token", 1, Some(OKTA_LIVE)),
        &kept,
        "SYNP000001",
    );
    assert!(t.complete, "{t:?}");
    let seen = server.seen();
    let authorize = seen
        .iter()
        .find(|s| s.target.starts_with("/oauth2/default/v1/authorize"))
        .unwrap();
    for (k, v) in [
        ("prompt", "none"),
        ("response_type", "code"),
        ("client_id", "0oaympyx2kMM41A140x7"),
        ("code_challenge_method", "S256"),
        ("response_mode", "query"),
    ] {
        assert_eq!(query(&authorize.target, k).as_deref(), Some(v), "{k}");
    }
    assert_eq!(authorize.headers["cookie"], OKTA_LIVE);
    let token = seen.iter().find(|s| s.target == "/oauth2/default/v1/token").unwrap();
    assert_eq!(token.method, "POST");
    assert!(token.body.contains("grant_type=authorization_code"));
    // Every API read went with the renewed token.
    for s in seen.iter().filter(|s| s.target.starts_with("/protected")) {
        assert_eq!(s.headers["authorization"], format!("Bearer {RENEWED}"));
    }
    let kept = kept.0.lock().unwrap();
    assert_eq!(kept.len(), 1, "one renewal, kept once");
    assert_eq!(kept[0].0, RENEWED);
    assert!(kept[0].1.is_some(), "renewed_at is set");
    assert!(kept[0].2.as_deref().unwrap().contains("idx=dummy-okta-live"));
}

#[test]
fn a_refused_token_is_renewed_once_and_the_read_retried() {
    let server = Server::start();
    let kept = Kept::default();
    let (_, t) = export(
        &server,
        session("dummy-stale-token", FAR, Some(OKTA_LIVE)),
        &kept,
        "SYNP000001",
    );
    assert!(t.complete);
    let seen = server.seen();
    assert_eq!(seen[0].headers["authorization"], "Bearer dummy-stale-token");
    assert_eq!(seen.iter().filter(|s| s.target.contains("/v1/token")).count(), 1);
    assert_eq!(kept.0.lock().unwrap().len(), 1);
}

#[test]
fn an_ended_okta_session_is_exit_3_naming_the_sign_in() {
    let server = Server::start();
    let kept = Kept::default();
    let client = Client::new(
        &profile(),
        session("dummy-old-token", 1, Some(OKTA_ENDED)),
        &kept,
        server.endpoints(),
    )
    .unwrap();
    let mut site = Site { client };
    let e = site.household().err().unwrap();
    assert_eq!(e.exit_code(), 3);
    assert!(e.remedy().unwrap().contains("auth login --browser --profile adult1"));
    assert!(!e.to_string().contains("dummy"), "{e}");
    assert!(kept.0.lock().unwrap().is_empty());
    // No session to renew from at all is the same.
    let client = Client::new(
        &profile(),
        session("dummy-old-token", 1, None),
        &kept,
        server.endpoints(),
    )
    .unwrap();
    let e = Site { client }.household().err().unwrap();
    assert_eq!(e.exit_code(), 3);
}

#[test]
fn a_refusal_partway_ends_the_stream_incomplete_with_its_error() {
    let server = Server::start();
    let kept = Kept::default();
    // The household and the list read; then every read is refused, and
    // Okta's session has ended, so the renewal fails too.
    server.fail_after(3, 401);
    let (rs, t) = export(&server, session(LIVE, FAR, Some(OKTA_ENDED)), &kept, "SYNP000001");
    assert!(!t.complete);
    let err = t.error.as_ref().unwrap();
    assert_eq!(err.code, "auth");
    assert_eq!(err.exit_code, Some(3));
    assert_eq!(of(&rs, "DiagnosticReport").len(), 0);
    assert_eq!(of(&rs, "Patient").len(), 1);
}

#[test]
fn a_server_error_skips_that_result_and_goes_on() {
    let server = Server::start();
    let kept = Kept::default();
    server.fail_after(3, 503);
    let (_, t) = export(&server, session(LIVE, FAR, None), &kept, "SYNP000001");
    assert!(t.complete);
    let codes: Vec<&str> = t.skipped.iter().map(|s| s.code.as_str()).collect();
    // Oldest collection first: two results refused, the upcoming one not
    // read at all.
    assert_eq!(codes, ["upstream", "upstream", "not_ready"]);
}

#[test]
fn api_paths_stay_on_the_portal_api() {
    let server = Server::start();
    let kept = Kept::default();
    let client = Client::new(&profile(), session(LIVE, FAR, None), &kept, server.endpoints()).unwrap();
    assert!(client.url("https://example.com/protected").is_err());
    assert!(client.url("/protected/../x").is_err());
    assert_eq!(
        client.url("/protected/patients/current").unwrap(),
        format!("{}/protected/patients/current", server.base)
    );
}
