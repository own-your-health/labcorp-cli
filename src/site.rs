//! Labcorp Patient's sign-in: what `auth login --browser` reads out of the
//! Chrome window, and the one read that confirms a session before it is
//! stored (docs/api.md, "Sign-in" and "The session").
//!
//! The web app keeps Okta's tokens in `localStorage` (`okta-token-storage`);
//! the capture's expression reads the access token from there once the app
//! has one, and the whole `labcorp.com` jar beside it: Okta's session
//! cookies on `login-patient.labcorp.com` renew the access token later
//! without a password. The kept browser profile clears its site storage
//! (`Launch::clear_site_storage`), so no token sits on disk between sign-ins;
//! Okta's cookies stay, so a profile signed in recently opens signed in.

use std::io::Read;
use std::time::Duration;

use pk_cli_browser::{BrowserError, Capture, Captured, Confirm, HostRule, JarCookie, Launch};
use pk_cli_config::profile::ProfileName;
use pk_cli_core::CliError;
use pk_cli_secrets::Secret;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION};

use crate::cli::BIN;
use crate::session::Session;

/// The provider's name in the browser crate's status lines and errors.
pub const LABEL: &str = "Labcorp Patient";
/// The web app's host, whose pages the capture reads.
pub const HOST: &str = "patient.labcorp.com";
/// Okta's sign-in host.
pub const LOGIN_HOST: &str = "login-patient.labcorp.com";
/// The portal API's host.
pub const API_HOST: &str = "portal-api.patient.cws.labcorp.com";
/// The portal API's base, `api.baseUrl` in the web app's settings.
pub const API_BASE: &str = "https://portal-api.patient.cws.labcorp.com";
/// The domain whose cookies the capture reads.
pub const DOMAIN: &str = "labcorp.com";
/// Where the Chrome window opens.
pub const START_URL: &str = "https://patient.labcorp.com/landing";
/// What signing in involves, for the opening status line.
pub const HINT: &str = "your Labcorp Patient email and password";

/// Okta's authorization server for the web app (`okta.issuer`).
pub const ISSUER: &str = "https://login-patient.labcorp.com/oauth2/default";
/// The web app's public client id (`okta.clientId`).
pub const CLIENT_ID: &str = "0oaympyx2kMM41A140x7";
/// The web app's registered redirect.
pub const REDIRECT_URI: &str = "https://patient.labcorp.com/callback";
/// The scopes the web app asks for.
pub const SCOPES: &str = "openid profile email";

/// The read that shows a token is a signed-in account's: the account's own
/// record.
pub const SESSION_CHECK_PATH: &str = "/protected/patients/current";

/// The expression the capture evaluates in the web app's pages: the access
/// token and its expiry from Okta's token storage as one JSON string, or
/// `null` before the app holds one.
pub const TOKEN_EXPRESSION: &str = "(() => { try { \
    const s = JSON.parse(localStorage.getItem('okta-token-storage') || '{}'); \
    const t = s.accessToken; \
    return t && t.accessToken && t.expiresAt \
      ? JSON.stringify({ access_token: t.accessToken, expires_at: t.expiresAt }) : null; \
  } catch (e) { return null; } })()";

const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an answer's body read for [`judge`].
const MAX_BODY: u64 = 1024 * 1024;

/// What `auth login --browser` reads: the token from a page on [`HOST`],
/// the whole [`DOMAIN`] jar and the browser's user agent. With `verify`, the
/// tab stays open until [`confirm_candidate`] says the token reads the
/// account.
pub fn capture(verify: bool) -> Result<Capture, BrowserError> {
    let capture = Capture::new(LABEL, HostRule::exact(HOST)?)
        .expression(TOKEN_EXPRESSION)
        .cookie_jar(DOMAIN)
        .with_user_agent()
        .hint(HINT);
    Ok(if verify {
        capture.confirm(confirm_candidate)
    } else {
        capture
    })
}

/// The sign-in: a Chrome window in the profile's own browser profile (or a
/// scratch one), the capture, and the session made of what it read.
pub fn browser_session(profile: &ProfileName, scratch: bool, verify: bool, quiet: bool) -> Result<Session, CliError> {
    let launch = Launch::new(BIN)
        .url(START_URL)
        .profile_name(profile.as_str())
        .fresh_profile(scratch)
        .clear_site_storage(true)
        .login_hint(format!("{BIN} auth login --browser --profile {profile}"))
        .quiet(quiet);
    let captured = pk_cli_browser::capture(launch, &capture(verify)?, pk_cli_browser::DEFAULT_TIMEOUT)?;
    session_from(&captured)
}

/// The session a capture holds.
pub fn session_from(captured: &Captured) -> Result<Session, CliError> {
    let token = captured
        .value()
        .ok_or_else(|| CliError::Other("the sign-in handed over no token".into()))
        .and_then(|v| parse_token(v.expose()))?;
    let jar = captured.jar();
    Ok(Session {
        access_token: token.0,
        expires_at: token.1,
        okta_cookies: cookie_header_for(jar, LOGIN_HOST),
        api_cookies: cookie_header_for(jar, API_HOST),
        user_agent: captured.user_agent().map(str::to_string),
        captured_at: crate::time::now_rfc3339(),
        renewed_at: None,
    })
}

/// The expression's answer: the access token and its expiry (Unix seconds).
/// The errors never quote it.
pub fn parse_token(raw: &str) -> Result<(Secret, u64), CliError> {
    #[derive(serde::Deserialize)]
    struct Token {
        access_token: String,
        expires_at: u64,
    }
    let t: Token = serde_json::from_str(raw).map_err(|_| {
        CliError::Other("the web app's token storage is not the shape this labcorp reads (docs/api.md)".into())
    })?;
    if t.access_token.is_empty() || HeaderValue::from_str(&t.access_token).is_err() {
        return Err(CliError::Other(
            "the web app's access token is empty or not a header value".into(),
        ));
    }
    Ok((Secret::new(t.access_token), t.expires_at))
}

/// The `Cookie` header a request to `host` would carry from `jar`: the
/// host-only cookies of that host and the domain cookies of it or a parent,
/// at any path. `None` when there are none.
pub fn cookie_header_for(jar: &[JarCookie], host: &str) -> Option<Secret> {
    let pairs: Vec<String> = jar
        .iter()
        .filter(|c| {
            if c.host_only() {
                c.domain() == host
            } else {
                host == c.domain() || host.ends_with(&format!(".{}", c.domain()))
            }
        })
        .map(|c| format!("{}={}", c.name(), c.value().expose()))
        .collect();
    (!pairs.is_empty()).then(|| Secret::new(pairs.join("; ")))
}

/// The capture's confirm hook: [`confirm_token`] with the candidate's token.
pub fn confirm_candidate(candidate: &Captured) -> Confirm {
    let Some(value) = candidate.value() else {
        return Confirm::NotYet("the web app holds no token yet".into());
    };
    match parse_token(value.expose()) {
        Ok((token, _)) => confirm_token(&token, candidate.user_agent()),
        Err(e) => Confirm::Fail(e),
    }
}

/// One read of [`SESSION_CHECK_PATH`] with `token`, judged by [`judge`].
pub fn confirm_token(token: &Secret, user_agent: Option<&str>) -> Confirm {
    confirm_at(&format!("{API_BASE}{SESSION_CHECK_PATH}"), token, user_agent)
}

/// How the answer from [`SESSION_CHECK_PATH`] shows a signed-in account:
/// `Ready` for a JSON object, `NotYet` for a 401 or another 4xx,
/// `Undecided` for anything else. It sees only answers that [`confirm_at`]
/// has not already decided: no 429, 5xx or 403 with an HTML page.
pub fn judge(status: u16, content_type: Option<&str>, body: &[u8]) -> Confirm {
    let what = format!("GET {SESSION_CHECK_PATH}");
    if (400..500).contains(&status) {
        return Confirm::NotYet(format!("{what} answered HTTP {status}: not signed in"));
    }
    if !(200..300).contains(&status) {
        return Confirm::undecided(format!("{what} answered HTTP {status}"));
    }
    if !content_type.is_some_and(|t| t.contains("json")) {
        return Confirm::undecided(format!("{what} answered HTTP {status} without JSON"));
    }
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(doc) if doc.is_object() => Confirm::Ready,
        Ok(_) => Confirm::undecided(format!("{what} answered without the account")),
        Err(_) => Confirm::undecided(format!("{what} answered HTTP {status} with JSON that does not parse")),
    }
}

/// [`confirm_token`]'s read of `url`, apart from the constant so the tests
/// can point it at a local server.
fn confirm_at(url: &str, token: &Secret, user_agent: Option<&str>) -> Confirm {
    let what = format!("GET {SESSION_CHECK_PATH}");
    let client = match client(user_agent) {
        Ok(client) => client,
        Err(e) => return Confirm::Fail(e),
    };
    let Ok(mut bearer) = HeaderValue::from_str(&format!("Bearer {}", token.expose())) else {
        return Confirm::Fail(CliError::Other("the access token is not a valid header value".into()));
    };
    bearer.set_sensitive(true);
    let answer = match client
        .get(url)
        .header(AUTHORIZATION, bearer)
        .header(ACCEPT, "application/json")
        .send()
    {
        Ok(answer) => answer,
        Err(e) if e.is_timeout() => return Confirm::undecided(format!("{what} timed out")),
        Err(_) => return Confirm::undecided(format!("{what} got no answer")),
    };
    if let Some(said) = pk_cli_http::inconclusive(&answer) {
        return Confirm::Undecided {
            reason: format!("{what} answered HTTP {}", said.status),
            retry_after: said.retry_after,
        };
    }
    let status = answer.status().as_u16();
    if answer.status().is_redirection() {
        return Confirm::NotYet(format!(
            "{what} answered with a redirect (HTTP {status}): not signed in"
        ));
    }
    let content_type = answer
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::new();
    if answer.take(MAX_BODY + 1).read_to_end(&mut body).is_err() {
        return Confirm::undecided(format!("{what} answered HTTP {status} and its body did not arrive"));
    }
    if body.len() as u64 > MAX_BODY {
        return Confirm::undecided(format!("{what} answered with more than 1 MiB"));
    }
    judge(status, content_type.as_deref(), &body)
}

/// The confirm read's client: no redirects followed, bounded in time.
fn client(user_agent: Option<&str>) -> Result<reqwest::blocking::Client, CliError> {
    let mut builder = pk_cli_http::builder(BIN, env!("CARGO_PKG_VERSION"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT);
    if let Some(agent) = user_agent {
        builder = builder.user_agent(agent);
    }
    builder
        .build()
        .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_storage_reads_into_a_token_and_its_expiry() {
        let (token, exp) = parse_token(r#"{"access_token":"dummy.token.value","expires_at":2222222222}"#).unwrap();
        assert_eq!(token.expose(), "dummy.token.value");
        assert_eq!(exp, 2_222_222_222);
        for bad in [
            r#"{"access_token":"","expires_at":1}"#,
            r#"{"access_token":"dummy\nsplit","expires_at":1}"#,
            r#"{"expires_at":1}"#,
            "not json dummy_secret",
        ] {
            let e = parse_token(bad).err().unwrap();
            assert!(!e.to_string().contains("dummy"), "{e}");
        }
    }

    #[test]
    fn a_cookie_goes_to_the_hosts_its_scope_names() {
        let jar = vec![
            JarCookie::for_test("idx", "dummy-okta", "login-patient.labcorp.com", true, "/"),
            JarCookie::for_test("DT", "dummy-device", "login-patient.labcorp.com", true, "/"),
            JarCookie::for_test("visitor", "dummy-visitor", "labcorp.com", false, "/"),
            JarCookie::for_test("api", "dummy-api", "portal-api.patient.cws.labcorp.com", true, "/"),
            JarCookie::for_test("app", "dummy-app", "patient.labcorp.com", true, "/"),
        ];
        assert_eq!(
            cookie_header_for(&jar, LOGIN_HOST).unwrap().expose(),
            "idx=dummy-okta; DT=dummy-device; visitor=dummy-visitor"
        );
        assert_eq!(
            cookie_header_for(&jar, API_HOST).unwrap().expose(),
            "visitor=dummy-visitor; api=dummy-api"
        );
        assert!(cookie_header_for(&jar[..2], API_HOST).is_none());
    }

    #[test]
    fn only_a_json_account_is_ready() {
        assert!(matches!(
            judge(200, Some("application/json"), br#"{"id":"x"}"#),
            Confirm::Ready
        ));
        assert!(matches!(
            judge(401, Some("application/json"), b"{}"),
            Confirm::NotYet(_)
        ));
        assert!(matches!(
            judge(200, Some("text/html"), b"<html>"),
            Confirm::Undecided { .. }
        ));
        assert!(matches!(
            judge(200, Some("application/json"), b"[]"),
            Confirm::Undecided { .. }
        ));
        assert!(matches!(judge(302, None, b""), Confirm::Undecided { .. }));
    }
}
