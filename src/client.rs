//! The portal API client: every read `labcorp` makes of Labcorp Patient, as
//! the signed-in account, and the token's renewal (docs/api.md, "The
//! session").
//!
//! Each read sends the stored access token as a bearer, the API host's own
//! cookies, and the browser's user agent. An access token past its expiry,
//! or one the API answers 401, is renewed the way the web app renews it: an
//! authorization request with `prompt=none` that Okta answers from its own
//! session cookie, exchanged with PKCE for a new token. No password is ever
//! involved. When Okta's session has ended too, the answer is exit 3 with
//! the sign-in that starts a new one.
//!
//! Reads are paced ([`PACE`]) and never retried on their own, past the one
//! retry after a renewal.

use std::io::Read;
use std::time::{Duration, Instant};

use pk_cli_auth::oauth::{
    code_from_callback, random_token, AuthorizationRequest, ClientAuth, Pkce, TokenRequest, TokenResponse,
};
use pk_cli_config::profile::ProfileName;
use pk_cli_core::{transport, CliError, Reason};
use pk_cli_secrets::Secret;
use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use serde_json::Value;

use crate::cli::BIN;
use crate::session::{Session, SessionStore};
use crate::site;

/// The least time between two reads.
pub const PACE: Duration = Duration::from_millis(400);
/// Renew a token this close to its expiry rather than send it.
const SKEW: u64 = 60;
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most JSON read from one answer.
const MAX_JSON: u64 = 16 * 1024 * 1024;
/// The most bytes read for one file.
pub const MAX_FILE: u64 = 48 * 1024 * 1024;

/// Labcorp asked the client to wait: a 429, or an answer with
/// `Retry-After`. A run of reads stops on it rather than going on asking.
pub const RATE_LIMITED: Reason = Reason::Provider("rate_limited");
/// No answer came at all: the connection failed or timed out.
pub const UNREACHABLE: Reason = Reason::Provider("unreachable");

/// Where the client sends its requests: the real hosts, or a test server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api_base: String,
    pub issuer: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            api_base: site::API_BASE.into(),
            issuer: site::ISSUER.into(),
        }
    }
}

/// Where a renewed session is written: the vault, or a test's closure.
pub trait Keep {
    fn keep(&self, profile: &ProfileName, session: &Session) -> Result<(), CliError>;
}

impl Keep for SessionStore {
    fn keep(&self, profile: &ProfileName, session: &Session) -> Result<(), CliError> {
        self.put(profile, session)
    }
}

pub struct Client<'k> {
    http: reqwest::blocking::Client,
    endpoints: Endpoints,
    profile: ProfileName,
    session: Session,
    keep: &'k dyn Keep,
    last: Option<Instant>,
    /// Whether this run renewed the token already.
    renewed: bool,
}

/// An answer to one read: its status, content type and body.
pub struct Answer {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
}

impl<'k> Client<'k> {
    pub fn new(
        profile: &ProfileName,
        session: Session,
        keep: &'k dyn Keep,
        endpoints: Endpoints,
    ) -> Result<Client<'k>, CliError> {
        let http = pk_cli_http::builder(BIN, env!("CARGO_PKG_VERSION"))
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))?;
        Ok(Client {
            http,
            endpoints,
            profile: profile.clone(),
            session,
            keep,
            last: None,
            renewed: false,
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// `GET <path>` on the portal API, as JSON.
    pub fn get_json(&mut self, path: &str) -> Result<Value, CliError> {
        let a = self.get(path, "application/json", MAX_JSON)?;
        judge(&a, path, &self.profile)?;
        if !a.content_type.as_deref().is_some_and(|t| t.contains("json")) {
            return Err(CliError::Upstream(format!(
                "GET {} answered HTTP {} without JSON",
                site_path(path),
                a.status
            )));
        }
        serde_json::from_slice(&a.body).map_err(|e| {
            CliError::Upstream(format!(
                "GET {} answered JSON that does not parse ({})",
                site_path(path),
                pk_med_fhir::describe_json_error(&e)
            ))
        })
    }

    /// `GET <path>` on the portal API, as bytes (a report PDF).
    pub fn get_file(&mut self, path: &str, accept: &str) -> Result<Answer, CliError> {
        let a = self.get(path, accept, MAX_FILE)?;
        judge(&a, path, &self.profile)?;
        Ok(a)
    }

    /// One read, renewing the token first when it is due and once more on a
    /// 401. The answer is not judged.
    pub fn get(&mut self, path: &str, accept: &str, max: u64) -> Result<Answer, CliError> {
        let url = self.url(path)?;
        if self.session.expired(crate::time::now(), SKEW) {
            self.renew()?;
        }
        let a = self.send(&url, accept, max)?;
        if a.status == 401 && !self.renewed {
            self.renew()?;
            return self.send(&url, accept, max);
        }
        Ok(a)
    }

    /// The URL of `path` (a path under the API base, or a URL on its host).
    /// Anything else is refused: the token goes to the API's host only.
    pub fn url(&self, path: &str) -> Result<String, CliError> {
        let base = self.endpoints.api_base.trim_end_matches('/');
        let url = if path.starts_with('/') {
            format!("{base}{path}")
        } else if path.starts_with(&format!("{base}/")) {
            path.to_string()
        } else {
            return Err(CliError::Usage(format!(
                "a path on the portal API starts with `/`, or is a URL under {base}"
            )));
        };
        transport::require_secure_url(&url)?;
        if url.contains("/../") || url.ends_with("/..") {
            return Err(CliError::Usage("a path with `..` in it is refused".into()));
        }
        Ok(url)
    }

    fn pace(&mut self) {
        if let Some(last) = self.last {
            let since = last.elapsed();
            if since < PACE {
                std::thread::sleep(PACE - since);
            }
        }
        self.last = Some(Instant::now());
    }

    fn send(&mut self, url: &str, accept: &str, max: u64) -> Result<Answer, CliError> {
        self.pace();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", self.session.access_token.expose()))
            .map_err(|_| CliError::Other("the stored access token is not a header value".into()))?;
        bearer.set_sensitive(true);
        let mut req = self
            .http
            .get(url)
            .header(AUTHORIZATION, bearer)
            .header(ACCEPT, accept)
            .header("Origin", format!("https://{}", site::HOST));
        if let Some(c) = &self.session.api_cookies {
            let mut v = HeaderValue::from_str(c.expose())
                .map_err(|_| CliError::Other("the stored API cookies are not a header value".into()))?;
            v.set_sensitive(true);
            req = req.header(COOKIE, v);
        }
        if let Some(ua) = &self.session.user_agent {
            req = req.header(reqwest::header::USER_AGENT, ua.as_str());
        }
        let resp = req.send().map_err(|e| {
            CliError::Upstream(format!("GET {} got no answer: {}", site_path(url), e.without_url()))
                .because(UNREACHABLE)
        })?;
        read_answer(resp, max, url)
    }

    /// A new access token from Okta's session, the way the web app gets one:
    /// an authorization request with `prompt=none`, then the code exchanged
    /// with its PKCE verifier. The renewed session is kept at once.
    pub fn renew(&mut self) -> Result<(), CliError> {
        let profile = self.profile.clone();
        let ended = move || {
            CliError::Auth(format!("Labcorp's sign-in session for profile `{profile}` has ended"))
                .because(Reason::CredentialRefused)
                .with_remedy(format!("{BIN} auth login --browser --profile {profile} --overwrite"))
        };
        let Some(okta) = self.session.okta_cookies.as_ref().map(|c| Secret::new(c.expose())) else {
            return Err(ended());
        };
        self.renewed = true;
        let pkce = Pkce::generate()?;
        let state = random_token(16)?;
        let issuer = self.endpoints.issuer.trim_end_matches('/').to_string();
        let url = AuthorizationRequest::new(
            format!("{issuer}/v1/authorize"),
            site::CLIENT_ID,
            site::REDIRECT_URI,
            &pkce,
            &state,
        )
        .scope(site::SCOPES)
        .param("nonce", random_token(16)?)
        .param("prompt", "none")
        .param("response_mode", "query")
        .url()?;
        self.pace();
        let mut cookie = HeaderValue::from_str(okta.expose())
            .map_err(|_| CliError::Other("the stored Okta cookies are not a header value".into()))?;
        cookie.set_sensitive(true);
        let mut req = self.http.get(&url).header(COOKIE, cookie);
        if let Some(ua) = &self.session.user_agent {
            req = req.header(reqwest::header::USER_AGENT, ua.as_str());
        }
        let resp = req.send().map_err(|e| {
            CliError::Upstream(format!("renewing the token got no answer: {}", e.without_url())).because(UNREACHABLE)
        })?;
        let status = resp.status().as_u16();
        let okta_cookies = merge_set_cookies(okta.expose(), resp.headers());
        let location = resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let Some(location) = location.filter(|l| l.starts_with(site::REDIRECT_URI)) else {
            return Err(if status == 429 {
                CliError::Upstream("renewing the token: Okta answered HTTP 429".into()).because(RATE_LIMITED)
            } else if (500..600).contains(&status) {
                CliError::Upstream(format!("renewing the token: Okta answered HTTP {status}"))
            } else {
                ended()
            });
        };
        if callback_error(&location).is_some_and(|e| {
            matches!(
                e.as_str(),
                "login_required" | "consent_required" | "interaction_required"
            )
        }) {
            return Err(ended());
        }
        let code = code_from_callback(&location, &state, None)
            .map_err(|e| e.with_remedy(ended().remedy().unwrap_or_default().to_string()))?;
        let http = TokenRequest::authorization_code(&code, site::REDIRECT_URI, &pkce)
            .to_http(&format!("{issuer}/v1/token"), &ClientAuth::public(site::CLIENT_ID))?;
        self.pace();
        let mut req = self.http.post(&http.url);
        for (k, v) in &http.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req = req.header("Origin", format!("https://{}", site::HOST));
        if let Some(ua) = &self.session.user_agent {
            req = req.header(reqwest::header::USER_AGENT, ua.as_str());
        }
        let resp = req.body(http.body.unwrap_or_default()).send().map_err(|e| {
            CliError::Upstream(format!("the token exchange got no answer: {}", e.without_url())).because(UNREACHABLE)
        })?;
        let status = resp.status().as_u16();
        let mut body = String::new();
        resp.take(MAX_JSON)
            .read_to_string(&mut body)
            .map_err(|_| CliError::Upstream("the token exchange's answer did not arrive".into()))?;
        if !(200..300).contains(&status) {
            return Err(if status >= 500 || status == 429 {
                CliError::Upstream(format!("the token exchange answered HTTP {status}"))
            } else {
                ended()
            });
        }
        let token = TokenResponse::parse(&body)?;
        let now = crate::time::now();
        self.session.access_token = token.access_token;
        self.session.expires_at = now + token.expires_in.unwrap_or(3600);
        self.session.okta_cookies = okta_cookies.map(Secret::new).or(Some(okta));
        self.session.renewed_at = Some(crate::time::rfc3339(now));
        self.keep.keep(&self.profile, &self.session)?;
        Ok(())
    }
}

/// The `error` an authorization answer carries, if any.
fn callback_error(location: &str) -> Option<String> {
    let query = location.split_once('?')?.1.split('#').next()?;
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "error")
        .map(|(_, v)| v.to_string())
}

/// `cookies` (a `Cookie` header) with the answer's `Set-Cookie`s applied: a
/// new value replaces the old, an emptied or expired one is dropped. `None`
/// when nothing changed.
pub fn merge_set_cookies(cookies: &str, headers: &HeaderMap) -> Option<String> {
    let mut pairs: Vec<(String, String)> = cookies
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut changed = false;
    for sc in headers.get_all(SET_COOKIE) {
        let Ok(sc) = sc.to_str() else { continue };
        let mut parts = sc.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.trim().split_once('=')) else {
            continue;
        };
        let attrs: Vec<String> = parts.map(|a| a.trim().to_ascii_lowercase()).collect();
        let gone = value.is_empty()
            || attrs.iter().any(|a| a == "max-age=0" || a.starts_with("max-age=-"))
            || attrs.iter().any(|a| a.starts_with("expires=") && a.contains("1970"));
        pairs.retain(|(k, _)| k != name);
        if !gone {
            pairs.push((name.to_string(), value.to_string()));
        }
        changed = true;
    }
    changed.then(|| {
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn read_answer(resp: Response, max: u64, url: &str) -> Result<Answer, CliError> {
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let retry_after = pk_cli_http::retry_after(resp.headers());
    let mut body = Vec::new();
    resp.take(max + 1)
        .read_to_end(&mut body)
        .map_err(|_| CliError::Upstream(format!("GET {}: the answer did not arrive whole", site_path(url))))?;
    if body.len() as u64 > max {
        return Err(CliError::Upstream(format!(
            "GET {} answered with more than {} MiB",
            site_path(url),
            max / (1024 * 1024)
        )));
    }
    Ok(Answer {
        status,
        content_type,
        body,
        retry_after,
    })
}

/// What an answer means: a 2xx passes; a 401 after renewal
/// (`CredentialRefused`) and a 403 (`Forbidden`) are exit 3, a 404 exit 4,
/// anything else exit 5, a 429 or an answer asking to wait tagged
/// [`RATE_LIMITED`]. The errors carry the path without its query, never the
/// body.
pub fn judge(a: &Answer, path: &str, profile: &ProfileName) -> Result<(), CliError> {
    let what = format!("GET {}", site_path(path));
    match a.status {
        200..=299 => Ok(()),
        401 => Err(
            CliError::Auth(format!("{what} answered 401: the session was not accepted"))
                .because(Reason::CredentialRefused)
                .with_remedy(format!("{BIN} auth login --browser --profile {profile} --overwrite")),
        ),
        403 => Err(
            CliError::Auth(format!("{what} answered 403: the account may not read this")).because(Reason::Forbidden),
        ),
        404 => Err(CliError::NotFound(format!("{what} answered 404"))),
        s => {
            let mut e = CliError::Upstream(format!("{what} answered HTTP {s}"));
            if s == 429 || a.retry_after.is_some() {
                e = e.because(RATE_LIMITED);
            }
            if let Some(after) = a.retry_after {
                e = e.with_remedy(format!(
                    "Labcorp asked to wait {} s; run it again after that",
                    after.as_secs()
                ));
            }
            Err(e)
        }
    }
}

/// A URL or path's path, without its query.
pub fn site_path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = if url.contains("://") {
        rest.find('/').map_or("/", |i| &rest[i..])
    } else {
        rest
    };
    path.split(['?', '#']).next().unwrap_or("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn judge_types_each_refusal() {
        let profile = ProfileName::parse("adult1").unwrap();
        let answer = |status, retry_after| Answer {
            status,
            content_type: Some("application/json".into()),
            body: Vec::new(),
            retry_after,
        };
        let e = |status, retry_after| judge(&answer(status, retry_after), "/p?x=1", &profile).unwrap_err();
        assert!(judge(&answer(200, None), "/p", &profile).is_ok());
        assert_eq!(e(401, None).reason(), Some(Reason::CredentialRefused));
        assert_eq!(e(401, None).exit_code(), 3);
        assert_eq!(e(403, None).reason(), Some(Reason::Forbidden));
        assert_eq!(e(403, None).exit_code(), 3);
        assert_eq!(e(404, None).exit_code(), 4);
        assert_eq!(e(404, None).reason(), None);
        assert_eq!(e(429, None).reason(), Some(RATE_LIMITED));
        assert_eq!(e(429, None).exit_code(), 5);
        let wait = e(503, Some(Duration::from_secs(30)));
        assert_eq!(wait.reason(), Some(RATE_LIMITED));
        assert!(wait.remedy().unwrap().contains("30 s"));
        assert_eq!(e(503, None).reason(), None);
        assert!(!e(500, None).message().contains("x=1"));
    }

    #[test]
    fn set_cookies_replace_add_and_drop() {
        let mut h = HeaderMap::new();
        h.append(
            SET_COOKIE,
            HeaderValue::from_static("idx=dummy2; Path=/; Secure; HttpOnly"),
        );
        h.append(SET_COOKIE, HeaderValue::from_static("JSESSIONID=dummy3; Path=/"));
        h.append(SET_COOKIE, HeaderValue::from_static("sid=; Max-Age=0; Path=/"));
        let merged = merge_set_cookies("idx=dummy1; sid=dummy9; DT=dummy4", &h).unwrap();
        assert_eq!(merged, "DT=dummy4; idx=dummy2; JSESSIONID=dummy3");
        assert!(merge_set_cookies("idx=dummy1", &HeaderMap::new()).is_none());
    }

    #[test]
    fn a_callback_error_is_read() {
        assert_eq!(
            callback_error("https://patient.labcorp.com/callback?state=s&error=login_required&error_description=x")
                .as_deref(),
            Some("login_required")
        );
        assert!(callback_error("https://patient.labcorp.com/callback?code=c&state=s").is_none());
    }

    #[test]
    fn a_path_is_shown_without_its_query() {
        assert_eq!(site_path("https://h.example/a/b?token=dummy"), "/a/b");
        assert_eq!(site_path("/a/b?x=1"), "/a/b");
    }
}
