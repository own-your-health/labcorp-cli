//! Where a profile's Labcorp Patient session is kept: the one seam that
//! knows.
//!
//! A [`Session`] is what the web app itself holds once signed in: Okta's
//! access token for the portal API, the Okta session cookies that renew it
//! without a password (docs/api.md, "The session"), and the API host's own
//! cookies. It is kept as one JSON entry per profile, `<profile>/session`, in
//! labcorp's vault (`pk_cli_auth::vault`, cli-common DESIGN.md §2.5): each
//! entry an encrypted owner-only file under `~/.config/labcorp/vault/`, all
//! under one key, the keychain item `vault-key` in `piekstra.labcorp`. A new
//! build of labcorp asks the keychain once, however many profiles are signed
//! in, and a run with nothing stored reads no keychain item at all.
//!
//! [`KEYCHAIN_SERVICE_ENV`] names another keychain service for the key, for
//! the tests and the smoke run, and moves the vault under
//! `$XDG_CONFIG_HOME/labcorp/vault`, which must then be set: a test's vault
//! is never the owner's. While it is set ([`hermetic`]) nothing touches the
//! real browser profiles either.

use std::path::PathBuf;

use pk_cli_auth::vault::{self, Vault};
use pk_cli_config::profile::ProfileName;
use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::{CredentialStore, Secret};
use serde::{Deserialize, Serialize};

use crate::cli::BIN;

/// Names the keychain service holding the vault's key instead of
/// `piekstra.labcorp`, and moves the vault under `$XDG_CONFIG_HOME`. For the
/// tests and the smoke run only.
pub const KEYCHAIN_SERVICE_ENV: &str = "LABCORP_KEYCHAIN_SERVICE";

/// The stored session. The token and the cookie headers are secrets; the
/// rest is what `auth status` reports.
pub struct Session {
    /// The Okta access token the portal API takes as a bearer.
    pub access_token: Secret,
    /// When the access token expires, Unix seconds (its `exp`).
    pub expires_at: u64,
    /// The `Cookie` header Okta's sign-in host takes: its session, which
    /// renews the access token with `prompt=none` and no password.
    pub okta_cookies: Option<Secret>,
    /// The `Cookie` header of the portal API's own host, if it set one.
    pub api_cookies: Option<Secret>,
    /// The browser's user agent, sent on every read.
    pub user_agent: Option<String>,
    /// When the browser sign-in captured it, RFC 3339 UTC.
    pub captured_at: String,
    /// When the access token was last renewed, RFC 3339 UTC.
    pub renewed_at: Option<String>,
}

#[derive(Serialize)]
struct EntryOut<'a> {
    access_token: &'a str,
    expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    okta_cookies: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_cookies: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<&'a str>,
    captured_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    renewed_at: Option<&'a str>,
}

#[derive(Deserialize)]
struct EntryIn {
    access_token: String,
    expires_at: u64,
    #[serde(default)]
    okta_cookies: Option<String>,
    #[serde(default)]
    api_cookies: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    captured_at: String,
    #[serde(default)]
    renewed_at: Option<String>,
}

impl Session {
    fn entry(&self) -> EntryOut<'_> {
        EntryOut {
            access_token: self.access_token.expose(),
            expires_at: self.expires_at,
            okta_cookies: self.okta_cookies.as_ref().map(Secret::expose),
            api_cookies: self.api_cookies.as_ref().map(Secret::expose),
            user_agent: self.user_agent.as_deref(),
            captured_at: &self.captured_at,
            renewed_at: self.renewed_at.as_deref(),
        }
    }

    /// The session an entry holds; `None` for one with an empty token.
    fn from_entry(read: EntryIn) -> Option<Session> {
        let nonempty = |v: Option<String>| v.filter(|s| !s.is_empty()).map(Secret::new);
        (!read.access_token.is_empty()).then(|| Session {
            access_token: Secret::new(read.access_token),
            expires_at: read.expires_at,
            okta_cookies: nonempty(read.okta_cookies),
            api_cookies: nonempty(read.api_cookies),
            user_agent: read.user_agent,
            captured_at: read.captured_at,
            renewed_at: read.renewed_at,
        })
    }

    /// Whether the access token is past its expiry, or within `skew`
    /// seconds of it, at `now` (Unix seconds).
    pub fn expired(&self, now: u64, skew: u64) -> bool {
        now.saturating_add(skew) >= self.expires_at
    }
}

/// The sessions, one per profile.
pub struct SessionStore {
    vault: Vault,
}

impl SessionStore {
    /// The store every command uses: the vault of `piekstra.labcorp`, or
    /// with [`KEYCHAIN_SERVICE_ENV`] a test's ([`hermetic`]).
    pub fn open() -> Result<SessionStore, CliError> {
        let vault = match service_override(service_env().as_deref()) {
            None => Vault::for_binary(BIN)?,
            Some(service) => Vault::new(CredentialStore::new(service), test_vault_dir()?),
        };
        Ok(SessionStore { vault })
    }

    /// A store over `vault`: the tests' in-memory one
    /// (`Vault::new(MemoryKeys::new(), dir)`).
    pub fn with_vault(vault: Vault) -> SessionStore {
        SessionStore { vault }
    }

    /// The profile's session, `None` when none is stored. An entry this
    /// labcorp cannot read is an error naming the profile and the command
    /// that removes it ([`Reason::CredentialUnreadable`]), never what it
    /// holds.
    pub fn get(&self, profile: &ProfileName) -> Result<Option<Session>, CliError> {
        let Some(raw) = self.vault.get(&entry(profile)).map_err(|e| remedied(e, profile))? else {
            return Ok(None);
        };
        let read: EntryIn = serde_json::from_str(raw.expose()).map_err(|_| unreadable(profile))?;
        Session::from_entry(read).map(Some).ok_or_else(|| unreadable(profile))
    }

    /// The profile's session, for a command that reads the site: none
    /// stored is exit 3, with the sign-in that stores one.
    pub fn load(&self, profile: &ProfileName) -> Result<Session, CliError> {
        self.get(profile)?.ok_or_else(|| {
            CliError::Auth(format!("no Labcorp Patient session is stored for profile `{profile}`"))
                .because(Reason::NoCredential)
                .with_remedy(format!("{BIN} auth login --browser --profile {profile}"))
        })
    }

    pub fn put(&self, profile: &ProfileName, session: &Session) -> Result<(), CliError> {
        self.vault
            .put_json(&entry(profile), &session.entry())
            .map_err(|e| remedied(e, profile))
    }

    /// Whether there was a session to remove. Reads no keychain item.
    pub fn delete(&self, profile: &ProfileName) -> Result<bool, CliError> {
        self.vault.delete(&entry(profile))
    }

    /// Remove every profile's session and the vault's key (`auth reset`).
    /// How many entries were removed.
    pub fn clear(&self) -> Result<usize, CliError> {
        self.vault.clear()
    }
}

/// The vault entry of a profile's session.
fn entry(profile: &ProfileName) -> String {
    format!("{profile}/session")
}

/// The vault's errors with labcorp's own commands as their remedies.
fn remedied(e: CliError, profile: &ProfileName) -> CliError {
    match e.remedy() {
        Some(vault::REMEDY_DELETE_ENTRY) => e
            .with_remedy(format!("{BIN} auth logout --profile {profile}, then sign in again"))
            .because(Reason::CredentialUnreadable),
        Some(vault::REMEDY_CLEAR) => e.with_remedy(format!(
            "{BIN} auth reset removes every profile's session and the key; then sign in again"
        )),
        _ => e,
    }
}

fn unreadable(profile: &ProfileName) -> CliError {
    CliError::Keychain(format!(
        "the session stored for profile `{profile}` is not the shape this labcorp writes"
    ))
    .because(Reason::CredentialUnreadable)
    .with_remedy(format!("{BIN} auth logout --profile {profile}, then sign in again"))
}

/// Whether [`KEYCHAIN_SERVICE_ENV`] points the store elsewhere: a test or
/// the smoke run, under which nothing reads or removes the owner's kept
/// browser profiles.
pub fn hermetic() -> bool {
    service_override(service_env().as_deref()).is_some()
}

/// A test's vault: under `$XDG_CONFIG_HOME/labcorp`, which must be set, so a
/// test never opens the owner's `~/.config/labcorp/vault`.
fn test_vault_dir() -> Result<PathBuf, CliError> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(base) => Ok(PathBuf::from(base).join(BIN).join("vault")),
        None => Err(CliError::Usage(format!(
            "{KEYCHAIN_SERVICE_ENV} is for tests and goes with XDG_CONFIG_HOME, so a test's vault is never the \
             owner's"
        ))),
    }
}

/// The variable's value. One that is not UTF-8 still counts as set, so it
/// never falls back to the real service.
fn service_env() -> Option<String> {
    std::env::var_os(KEYCHAIN_SERVICE_ENV).map(|v| v.to_string_lossy().into_owned())
}

/// A blank value names no service.
fn service_override(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pk_cli_auth::vault::MemoryKeys;
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("labcorp-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn home() -> ProfileName {
        ProfileName::parse("home").unwrap()
    }

    fn dummy() -> Session {
        Session {
            access_token: Secret::new("dummy.access.token"),
            expires_at: 2_000_000_000,
            okta_cookies: Some(Secret::new("dummy_idx=1")),
            api_cookies: None,
            user_agent: Some("Mozilla/5.0 (test)".into()),
            captured_at: "2026-01-02T03:04:05Z".into(),
            renewed_at: None,
        }
    }

    #[test]
    fn a_blank_service_names_none() {
        assert_eq!(service_override(None), None);
        assert_eq!(service_override(Some("  ")), None);
        assert_eq!(
            service_override(Some("piekstra.labcorp.hermetic-tests")),
            Some("piekstra.labcorp.hermetic-tests")
        );
    }

    /// One entry per profile; storing one profile's session leaves another's
    /// alone, a store with nothing in it reads no key, and no secret is on
    /// disk in the clear.
    #[test]
    fn each_profile_has_its_own_entry() {
        let dir = temp_dir("entries");
        let keys = Arc::new(MemoryKeys::new());
        let store = SessionStore::with_vault(Vault::new(keys.clone(), &dir));
        let work = ProfileName::parse("work").unwrap();
        assert!(store.get(&home()).unwrap().is_none());
        assert_eq!(keys.reads(), 0, "nothing stored, nothing read");
        assert_eq!(entry(&home()), "home/session");

        store.put(&home(), &dummy()).unwrap();
        let read = store.get(&home()).unwrap().unwrap();
        assert_eq!(read.access_token.expose(), "dummy.access.token");
        assert_eq!(read.expires_at, 2_000_000_000);
        assert_eq!(read.okta_cookies.as_ref().map(Secret::expose), Some("dummy_idx=1"));
        assert!(read.api_cookies.is_none());
        assert_eq!(read.user_agent.as_deref(), Some("Mozilla/5.0 (test)"));
        assert!(store.get(&work).unwrap().is_none());

        for file in std::fs::read_dir(&dir).unwrap() {
            let bytes = std::fs::read(file.unwrap().path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("dummy.access") && !text.contains("dummy_idx"));
        }
        assert!(store.delete(&home()).unwrap());
        assert!(!store.delete(&home()).unwrap());
        assert!(store.load(&home()).err().unwrap().is_auth());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expiry_counts_the_skew() {
        let s = dummy();
        assert!(!s.expired(1_999_999_000, 60));
        assert!(s.expired(1_999_999_950, 60));
        assert!(s.expired(2_000_000_001, 0));
    }

    /// An entry of another shape names the profile and the command that
    /// removes it, never what it holds.
    #[test]
    fn an_unreadable_entry_is_named_not_quoted() {
        let dir = temp_dir("unreadable");
        let vault = Vault::new(MemoryKeys::new(), &dir);
        vault
            .put("home/session", &Secret::new(r#"{"token":"dummy_secret"}"#))
            .unwrap();
        vault
            .put(
                "work/session",
                &Secret::new(r#"{"access_token":"","expires_at":1,"captured_at":"x"}"#),
            )
            .unwrap();
        let store = SessionStore::with_vault(vault);
        for name in ["home", "work"] {
            let e = store.get(&ProfileName::parse(name).unwrap()).err().unwrap();
            assert_eq!(e.reason(), Some(Reason::CredentialUnreadable), "{name}");
            assert!(!e.to_string().contains("dummy"), "{e}");
            assert!(
                e.remedy().unwrap().contains(&format!("auth logout --profile {name}")),
                "{e}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
