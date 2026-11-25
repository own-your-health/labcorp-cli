//! `auth login`, `auth status`, `auth logout` and `auth reset`, and what
//! `profiles remove` removes: a Labcorp Patient session per profile, kept
//! through [`SessionStore`], which alone knows where.
//!
//! A `--browser` sign-in runs Chrome in the profile's own kept browser
//! profile (`Launch::profile_name`), so Labcorp remembers each account's
//! device apart and one account's window never opens signed in as another;
//! `auth logout --profile <NAME>` removes the session, then that browser
//! profile. While `LABCORP_KEYCHAIN_SERVICE` points the store elsewhere
//! ([`session::hermetic`]), nothing touches the real browser profiles: `auth
//! status` does not report them, `auth logout` leaves them in place, and
//! `auth login --browser` signs in through a fresh one.

use pk_cli_auth::{AuthLogin, AuthMethod, AuthStatus};
use pk_cli_browser::BrowserError;
use pk_cli_config::profile::ProfileName;
use pk_cli_core::{output, CliError, Reason};
use serde_json::{json, Value};

use crate::cli::{LoginCmd, BIN};
use crate::session::{self, SessionStore, KEYCHAIN_SERVICE_ENV};
use crate::site;

// ----------------------------------------------------------------- login

pub fn login(cmd: &LoginCmd, profile: &ProfileName, json: bool, quiet: bool) -> Result<(), CliError> {
    if !cmd.browser.browser {
        return Err(CliError::Usage(format!(
            "pass --browser: `{BIN} auth login --browser --profile {profile}` opens a Chrome window on \
             patient.labcorp.com, where the account signs in on Labcorp's own page"
        )));
    }
    let verify = !cmd.no_verify;
    let store = SessionStore::open()?;
    // Before Chrome opens, so no sign-in is spent on a session that would not
    // be stored.
    if !cmd.overwrite {
        let stored = match store.get(profile) {
            Ok(found) => found.is_some(),
            Err(e) if e.reason() == Some(Reason::CredentialUnreadable) => true,
            Err(e) => return Err(e),
        };
        if stored {
            return Err(CliError::Usage(format!(
                "a Labcorp Patient session is already stored for profile `{profile}`; pass --overwrite to \
                 replace it (`{BIN} auth logout --profile {profile}` removes it)"
            )));
        }
    }
    let scratch = cmd.browser.fresh_profile || session::hermetic();
    if scratch && !cmd.browser.fresh_profile && !quiet {
        eprintln!(
            "{KEYCHAIN_SERVICE_ENV} is set, so this sign-in uses a fresh browser profile and leaves the kept one alone"
        );
    }
    let session = site::browser_session(profile, scratch, verify, quiet)?;
    store.put(profile, &session)?;
    if verify {
        if !quiet {
            eprintln!("Labcorp Patient session stored for profile `{profile}`");
        }
    } else {
        // Even under -q: the session is stored with nothing to show it works.
        eprintln!(
            "warning: the session was stored unverified (--no-verify): no read checked it. \
             `{BIN} auth login --browser --profile {profile} --overwrite` replaces it"
        );
    }
    if json {
        output::json(&AuthLogin::stored(verify).to_json());
    }
    Ok(())
}

// ---------------------------------------------------------------- status

/// `auth status`: local facts only, no request.
pub fn status(profile: &ProfileName, json: bool, quiet: bool) -> Result<(), CliError> {
    let store = SessionStore::open()?;
    let (stored, session) = match store.get(profile) {
        Ok(session) => (session.is_some(), session),
        Err(e) if e.reason() == Some(Reason::CredentialUnreadable) => {
            eprintln!("warning: {}", e.message());
            (true, None)
        }
        Err(e) => return Err(e),
    };
    let mut status = AuthStatus::new(true, session.is_some(), AuthMethod::BrowserSession);
    status.account = Some(profile.as_str().to_string());
    status.credential_in_keychain = Some(stored);
    let (kept, line) = browser_profile_status(session::hermetic(), || {
        pk_cli_browser::named_profile_exists(BIN, profile.as_str())
    });
    status.browser_profile = kept;
    if let (Some(line), false) = (line, quiet) {
        eprintln!("{line}");
    }
    let mut extra = serde_json::Map::new();
    if let Some(s) = &session {
        extra.insert("captured_at".into(), json!(s.captured_at));
        if let Some(at) = &s.renewed_at {
            extra.insert("renewed_at".into(), json!(at));
        }
        extra.insert("token_expires_at".into(), json!(crate::time::rfc3339(s.expires_at)));
        extra.insert("renewable".into(), json!(s.okta_cookies.is_some()));
    }
    if json {
        let mut v = status.to_json();
        for (k, val) in extra {
            v[k] = val;
        }
        output::json(&v);
    } else {
        status.render();
        if let Some(s) = &session {
            println!("Captured:      {}", s.captured_at);
            if let Some(at) = &s.renewed_at {
                println!("Renewed:       {at}");
            }
            println!("Token until:   {}", crate::time::rfc3339(s.expires_at));
            println!(
                "Renewable:     {}",
                if s.okta_cookies.is_some() {
                    "while Labcorp's sign-in session lasts"
                } else {
                    "no"
                }
            );
        }
    }
    Ok(())
}

/// `auth status`'s browser field: whether a `--browser` sign-in keeps a
/// profile here, not whether it is signed in, and any line for stderr. A
/// profile that needs repair is a warning and leaves the field out, and so
/// does [`KEYCHAIN_SERVICE_ENV`], under which nothing reads the real profile.
/// The check is passed in: the binary binds it to `named_profile_exists`, the
/// tests to `named_profile_exists_in` under a temp dir.
fn browser_profile_status(
    overridden: bool,
    exists: impl FnOnce() -> Result<bool, BrowserError>,
) -> (Option<bool>, Option<String>) {
    if overridden {
        return (
            None,
            Some(format!(
                "{KEYCHAIN_SERVICE_ENV} is set, so the kept browser profile is not reported"
            )),
        );
    }
    match exists() {
        Ok(kept) => (Some(kept), None),
        Err(e) => (None, Some(format!("warning: {e}"))),
    }
}

// ---------------------------------------------------------------- logout

pub fn logout(profile: &ProfileName, json: bool, quiet: bool) -> Result<(), CliError> {
    let done = forget_login(profile)?;
    if json {
        output::json(&done.to_json());
    } else if !quiet {
        eprintln!("{}", done.line());
    }
    Ok(())
}

/// What a login keeps, removed: the profile's session, then the browser
/// profile its `--browser` sign-in keeps (`forget_named_profile`), which
/// [`session::hermetic`] leaves alone. `auth logout` and `profiles remove`.
pub fn forget_login(profile: &ProfileName) -> Result<LoggedOut, CliError> {
    let store = SessionStore::open()?;
    let forget = (!session::hermetic()).then_some(|| pk_cli_browser::forget_named_profile(BIN, profile.as_str()));
    run_logout(|| store.delete(profile), forget, profile)
}

/// What `auth logout` removed (`auth-logout/v1`).
#[derive(Debug, PartialEq, Eq)]
pub struct LoggedOut {
    pub session_removed: bool,
    /// `None` when the profile was left alone ([`KEYCHAIN_SERVICE_ENV`]).
    pub browser_profile_removed: Option<bool>,
}

impl LoggedOut {
    fn to_json(&self) -> Value {
        let mut v = json!({ "schema": "auth-logout/v1", "session_removed": self.session_removed });
        if let Some(removed) = self.browser_profile_removed {
            v["browser_profile_removed"] = removed.into();
        }
        v
    }

    fn line(&self) -> String {
        format!(
            "logged out: {}; {}",
            session_said(self.session_removed),
            profile_said(self.browser_profile_removed)
        )
    }
}

fn session_said(removed: bool) -> &'static str {
    if removed {
        "the session was removed from the vault"
    } else {
        "no session was stored"
    }
}

fn profile_said(removed: Option<bool>) -> String {
    match removed {
        Some(true) => "the browser profile was removed".into(),
        Some(false) => "no browser profile was kept".into(),
        None => format!("{KEYCHAIN_SERVICE_ENV} is set, so the kept browser profile was left alone"),
    }
}

/// `auth logout`'s two removals: the session first, then the browser
/// profile (`None` leaves it alone). Both are attempted whatever the other
/// did, so a refusal of one leaves the other done, and the error says what
/// was. They are passed in, so a test never removes the owner's session or
/// profile.
fn run_logout(
    delete_session: impl FnOnce() -> Result<bool, CliError>,
    forget_profile: Option<impl FnOnce() -> Result<bool, BrowserError>>,
    who: &ProfileName,
) -> Result<LoggedOut, CliError> {
    let session = delete_session();
    let profile = forget_profile.map(|forget| forget().map_err(CliError::from));
    match (session, profile) {
        (Ok(session_removed), None) => Ok(LoggedOut {
            session_removed,
            browser_profile_removed: None,
        }),
        (Ok(session_removed), Some(Ok(removed))) => Ok(LoggedOut {
            session_removed,
            browser_profile_removed: Some(removed),
        }),
        (Ok(session_removed), Some(Err(e))) => Err(e.with_remedy(format!(
            "{}; the browser profile is all that is left",
            session_said(session_removed)
        ))),
        (Err(e), profile) => {
            let profile = match profile {
                None => profile_said(None),
                Some(Ok(removed)) => profile_said(Some(removed)),
                Some(Err(_)) => "the browser profile was not removed either".into(),
            };
            Err(e.with_remedy(format!("{profile}; run `{BIN} auth logout --profile {who}` again")))
        }
    }
}

// ----------------------------------------------------------------- reset

/// `auth reset`: every profile's session and the vault's key, removed under
/// the vault's key lock (`Vault::clear`). For a key that is lost or damaged,
/// when no session reads; the profiles and the kept browser profiles stay.
/// Destructive, so it asks on a terminal and needs `--force` otherwise
/// (exit 6), decided before the vault is opened.
pub fn reset(force: bool, interactive: bool, json: bool, quiet: bool) -> Result<(), CliError> {
    let what = "remove every profile's Labcorp Patient session and the vault's key";
    pk_cli_core::confirm::require_confirmable(force, interactive, what)?;
    pk_cli_core::confirm::confirm(
        force,
        "Remove every profile's stored Labcorp Patient session and the vault's key? Each profile signs in again after",
    )?;
    let removed = SessionStore::open()?.clear()?;
    if json {
        output::json(&json!({ "schema": "auth-reset/v1", "sessions_removed": removed }));
    } else if !quiet {
        eprintln!(
            "reset: {removed} stored session(s) and the vault's key removed; sign in again with `{BIN} auth login \
             --browser --profile <NAME>`"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> ProfileName {
        ProfileName::parse("home").unwrap()
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("labcorp-auth-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn auth_status_reports_the_kept_browser_profile() {
        let root = temp_root("status");
        let (kept, line) =
            browser_profile_status(false, || pk_cli_browser::named_profile_exists_in(&root, BIN, "home"));
        assert_eq!(kept, Some(false));
        assert!(line.is_none());
        let (kept, line) = browser_profile_status(true, || unreachable!("never read under the override"));
        assert_eq!(kept, None);
        assert!(line.unwrap().contains(KEYCHAIN_SERVICE_ENV));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn logout_removes_the_session_then_the_kept_profile() {
        let root = temp_root("logout");
        let done = run_logout(
            || Ok(true),
            Some(|| pk_cli_browser::forget_named_profile_in(&root, BIN, "home")),
            &home(),
        )
        .unwrap();
        assert_eq!(
            done,
            LoggedOut {
                session_removed: true,
                browser_profile_removed: Some(false)
            }
        );
        assert_eq!(done.to_json()["schema"], "auth-logout/v1");
        let left = run_logout(|| Ok(false), None::<fn() -> Result<bool, BrowserError>>, &home()).unwrap();
        assert_eq!(left.browser_profile_removed, None);
        assert!(left.line().contains(KEYCHAIN_SERVICE_ENV));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_logout_that_fails_halfway_says_what_was_done() {
        let e = run_logout(
            || Err(CliError::Keychain("dummy refusal".into())),
            Some(|| Ok(true)),
            &home(),
        )
        .err()
        .unwrap();
        let remedy = e.remedy().unwrap();
        assert!(remedy.contains("the browser profile was removed"), "{remedy}");
        assert!(remedy.contains("auth logout --profile home"), "{remedy}");
    }
}
