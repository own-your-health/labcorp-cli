//! Black-box tests of the binary's surface. None reaches Labcorp: each
//! renders help or info, or is refused by argument checks that run before
//! the vault or the site is read, or reads an empty test vault. The config
//! directory is a temp dir, never the developer's, and `LABCORP_PROFILE` is
//! cleared.
//!
//! None reaches the real keychain or browser profile:
//! `LABCORP_KEYCHAIN_SERVICE` names a service that holds nothing (a missing
//! item needs no approval), and under it the binary never touches the kept
//! profile. `XDG_DATA_HOME`, where Linux keeps that profile, is a temp dir
//! besides. No test stores a session, so no test writes the vault's key or
//! reads one: the vault reads its key only when an entry exists (cli-common
//! DESIGN.md §2.5).
//!
//! The temp config of [`labcorp`] configures two profiles, `adult1` and
//! `adult2`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn tmp() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "labcorp-cli-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The empty keychain service every test reads instead of `piekstra.labcorp`.
const HERMETIC_SERVICE: &str = "piekstra.labcorp.hermetic-tests";

/// A config directory whose config configures `profiles`.
fn config_with(profiles: &[&str]) -> std::path::PathBuf {
    let base = tmp();
    std::fs::create_dir_all(base.join("labcorp")).unwrap();
    let names: serde_json::Map<String, Value> = profiles
        .iter()
        .map(|p| (p.to_string(), serde_json::json!({})))
        .collect();
    std::fs::write(
        base.join("labcorp").join("config.json"),
        serde_json::json!({ "profiles": names }).to_string(),
    )
    .unwrap();
    base
}

fn labcorp_in(config: std::path::PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("labcorp").unwrap();
    cmd.env("LABCORP_KEYCHAIN_SERVICE", HERMETIC_SERVICE)
        .env("XDG_DATA_HOME", tmp())
        .env("XDG_CONFIG_HOME", config)
        .env_remove("LABCORP_PROFILE")
        .env_remove("NO_COLOR");
    cmd
}

fn labcorp() -> Command {
    labcorp_in(config_with(&["adult1", "adult2"]))
}

fn error_of(out: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).expect("one JSON error document on stdout")
}

#[test]
fn every_command_renders_help() {
    for args in [
        vec!["--help"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["auth", "status", "--help"],
        vec!["auth", "logout", "--help"],
        vec!["auth", "reset", "--help"],
        vec!["profiles", "--help"],
        vec!["profiles", "list", "--help"],
        vec!["profiles", "add", "--help"],
        vec!["profiles", "remove", "--help"],
        vec!["patients", "--help"],
        vec!["patients", "list", "--help"],
        vec!["patients", "label", "--help"],
        vec!["records", "--help"],
        vec!["records", "export", "--help"],
        vec!["results", "--help"],
        vec!["results", "list", "--help"],
        vec!["results", "get", "--help"],
        vec!["documents", "--help"],
        vec!["documents", "list", "--help"],
        vec!["documents", "download", "--help"],
        vec!["api", "--help"],
        vec!["config", "--help"],
        vec!["config", "path", "--help"],
        vec!["config", "show", "--help"],
        vec!["self-update", "--help"],
        vec!["completions", "--help"],
        vec!["info", "--help"],
    ] {
        labcorp().args(&args).assert().success();
    }
}

#[test]
fn help_says_read_only_and_unofficial() {
    labcorp()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Unofficial read-only"));
}

#[test]
fn info_declares_the_profiles_and_the_login_profiles_capability() {
    let out = labcorp().arg("info").assert().success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "labcorp");
    let profiles: Vec<&str> = v["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(profiles.contains(&"health-records/v1") && profiles.contains(&"documents/v1"));
    let caps = v["capabilities"].as_array().unwrap();
    assert!(caps.iter().any(|c| c == "login-profiles"));
    assert!(caps.iter().any(|c| c == "patient-labels"));
    assert!(
        !caps.iter().any(|c| c == "records-files"),
        "labcorp has no files source"
    );
    assert_eq!(v["auth"]["required"], true);
    assert_eq!(v["auth"]["method"], "browser-session");
}

#[test]
fn completions_render() {
    for shell in ["bash", "zsh", "fish"] {
        labcorp().args(["completions", shell]).assert().success();
    }
}

#[test]
fn no_color_is_accepted() {
    labcorp().env("NO_COLOR", "1").arg("info").assert().success();
}

/// Every command that uses a login needs --profile: none is exit 2 naming
/// the configured ones, and a lone profile is never picked.
#[test]
fn a_login_command_without_a_profile_is_a_usage_error() {
    for args in [
        vec!["auth", "status"],
        vec!["auth", "login", "--browser"],
        vec!["auth", "logout"],
        vec!["patients", "list"],
        vec!["records", "export", "-o", "unused-dir"],
        vec!["results", "list"],
        vec!["results", "get", "SYNR00000001"],
        vec!["documents", "list"],
        vec!["documents", "download", "--all"],
        vec!["api", "GET", "/protected/patients/current"],
    ] {
        let out = labcorp().args(&args).arg("--json").assert().code(2);
        let e = error_of(&out);
        assert_eq!(e["error"]["code"], "usage", "{args:?}");
        let text = e.to_string();
        assert!(text.contains("adult1") && text.contains("adult2"), "{args:?}: {text}");
    }
    let lone = labcorp_in(config_with(&["adult2"]))
        .args(["api", "GET", "/protected/patients/current", "--json"])
        .assert()
        .code(2);
    assert_eq!(error_of(&lone)["error"]["code"], "usage");
}

#[test]
fn an_unknown_profile_is_a_usage_error() {
    labcorp()
        .args(["auth", "status", "--profile", "adult3"])
        .assert()
        .code(2);
}

#[test]
fn the_profile_variable_is_the_fallback() {
    labcorp()
        .env("LABCORP_PROFILE", "adult2")
        .args(["auth", "status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"account\": \"adult2\""));
}

#[test]
fn a_command_without_a_login_takes_no_profile() {
    for args in [
        vec!["info"],
        vec!["profiles", "list"],
        vec!["config", "path"],
        vec!["completions", "bash"],
        vec!["auth", "reset", "--force"],
    ] {
        labcorp().args(&args).args(["--profile", "adult1"]).assert().code(2);
    }
    // The variable is not read by them.
    labcorp()
        .env("LABCORP_PROFILE", "adult1")
        .arg("info")
        .assert()
        .success();
}

#[test]
fn auth_status_without_a_session_says_so() {
    let out = labcorp()
        .args(["auth", "status", "--profile", "adult1", "--json"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-status/v1");
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["method"], "browser-session");
    assert_eq!(v["account"], "adult1");
}

/// A read without a stored session is exit 3 naming the sign-in, before any
/// request.
#[test]
fn a_read_without_a_session_is_exit_3() {
    for args in [
        vec!["patients", "list"],
        vec!["results", "list"],
        vec!["results", "get", "SYNR00000001"],
        vec!["documents", "list"],
        vec!["documents", "download", "--all", "-o", "unused-dir"],
        vec!["records", "export", "-o", "unused-dir"],
        vec!["api", "GET", "/protected/patients/current"],
    ] {
        let out = labcorp()
            .args(&args)
            .args(["--profile", "adult1", "--json"])
            .assert()
            .code(3);
        let e = error_of(&out);
        assert_eq!(e["error"]["code"], "auth", "{args:?}");
        assert!(
            e.to_string().contains("auth login --browser --profile adult1"),
            "{args:?}"
        );
    }
    // The stream's own errors before its header are the error document's.
    labcorp()
        .args(["records", "export", "--ndjson", "--profile", "adult1"])
        .assert()
        .code(3)
        .stdout(predicate::str::is_empty());
}

#[test]
fn records_export_needs_a_destination_and_no_json_with_the_stream() {
    for args in [
        vec!["records", "export"],
        vec!["records", "export", "--ndjson", "--json"],
        vec!["records", "export", "--ndjson", "-o", "x"],
        vec!["records", "export", "--ndjson", "--since", "11/19/2026"],
    ] {
        labcorp().args(&args).args(["--profile", "adult1"]).assert().code(2);
    }
}

#[test]
fn date_flags_are_iso() {
    labcorp()
        .args(["results", "list", "--profile", "adult1", "--since", "2026-13-01"])
        .assert()
        .code(2);
    labcorp()
        .args(["documents", "list", "--profile", "adult1", "--until", "yesterday"])
        .assert()
        .code(2);
}

#[test]
fn documents_download_needs_an_id_or_all_and_all_needs_a_folder() {
    labcorp()
        .args(["documents", "download", "--profile", "adult1"])
        .assert()
        .code(2);
    labcorp()
        .args(["documents", "download", "x", "--all", "--profile", "adult1"])
        .assert()
        .code(2);
    labcorp()
        .args(["documents", "download", "--all", "-o", "-", "--profile", "adult1"])
        .assert()
        .code(2);
}

#[test]
fn auth_login_needs_the_browser_flag() {
    let out = labcorp()
        .args(["auth", "login", "--profile", "adult1", "--json"])
        .assert()
        .code(2);
    assert!(error_of(&out).to_string().contains("--browser"));
    labcorp()
        .args(["auth", "login", "--profile", "adult1", "--fresh-profile"])
        .assert()
        .code(2);
}

#[test]
fn auth_logout_without_a_session_removes_nothing() {
    let out = labcorp()
        .args(["auth", "logout", "--profile", "adult1", "--json"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-logout/v1");
    assert_eq!(v["session_removed"], false);
    assert!(
        v.get("browser_profile_removed").is_none(),
        "the kept profile is left alone in tests"
    );
}

/// `auth reset` removes every session and the vault's key: without a
/// terminal it needs --force (exit 6), decided before the vault is opened.
/// A forced reset is not run here: it deletes a keychain item, and CI's
/// Linux runner has no keychain.
#[test]
fn auth_reset_needs_force_without_a_terminal() {
    let out = labcorp().args(["auth", "reset", "--json"]).assert().code(6);
    assert_eq!(error_of(&out)["error"]["code"], "confirmation_required");
}

#[test]
fn api_is_read_only_and_stays_on_the_portal_api() {
    for args in [
        vec!["api", "POST", "/protected/patients/current"],
        vec!["api", "DELETE", "/protected/patients/current"],
        vec!["api", "GET", "https://example.com/protected"],
        vec!["api", "GET", "protected/patients/current"],
    ] {
        labcorp().args(&args).args(["--profile", "adult1"]).assert().code(2);
    }
}

#[test]
fn profiles_add_list_remove() {
    let config = config_with(&[]);
    let run = || labcorp_in(config.clone());
    run()
        .args(["profiles", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"profiles\": []"));
    run()
        .args(["profiles", "add", "adult2", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"added\": true"));
    run().args(["profiles", "add", "Adult 2"]).assert().code(2);
    run()
        .args(["profiles", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("adult2"));
    run().args(["profiles", "remove", "adult2"]).assert().code(6);
    run().args(["profiles", "remove", "adult9", "--force"]).assert().code(4);
    run()
        .args(["profiles", "remove", "adult2", "--force", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"session_removed\": false"));
    run()
        .args(["profiles", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"profiles\": []"));
}

/// Two synthetic patient ids (17 characters, as labcorp derives them, but
/// not the digest of anyone) that a read of `adult1` has listed: one
/// labelled `me`, one with no label.
const ME: &str = "l0000000000000001";
const KID: &str = "l0000000000000002";

/// A config where `adult1` has listed [`ME`] (labelled `me`) and [`KID`],
/// and `adult2` nobody.
fn config_with_people() -> std::path::PathBuf {
    let base = tmp();
    std::fs::create_dir_all(base.join("labcorp")).unwrap();
    let config = serde_json::json!({ "profiles": {
        "adult1": { "members": {
            ME: { "label": "me", "relationship": "self" },
            KID: { "relationship": "proxy" },
        } },
        "adult2": {},
    } });
    std::fs::write(base.join("labcorp").join("config.json"), config.to_string()).unwrap();
    base
}

fn label_of(config: &std::path::Path, id: &str) -> Value {
    let raw = std::fs::read_to_string(config.join("labcorp").join("config.json")).unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    v["profiles"]["adult1"]["members"][id]["label"].clone()
}

#[test]
fn patients_label_sets_and_clears_a_label_without_a_session() {
    let config = config_with_people();
    let run = |args: &[&str]| {
        labcorp_in(config.clone())
            .args(["--json", "patients", "label", "--profile", "adult1"])
            .args(args)
            .assert()
    };
    let out = run(&["--patient", KID, "kid"]).success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "patient-list/v1");
    assert_eq!(v["items"][0]["id"], KID);
    assert_eq!(v["items"][0]["label"], "kid");
    assert_eq!(v["items"][0]["relationship"], "proxy");
    assert!(v["items"][0].get("name").is_none(), "{v}");
    assert_eq!(label_of(&config, KID), "kid");
    // The label stands for the person from now on, case aside; setting it
    // again changes nothing.
    run(&["--patient", "KID", "kid"]).success();
    assert_eq!(label_of(&config, KID), "kid");
    run(&["--patient", "kid", "--clear"]).success();
    assert_eq!(label_of(&config, KID), Value::Null);
    // Clearing one that is not set succeeds too.
    run(&["--patient", KID, "--clear"]).success();
}

#[test]
fn patients_label_refuses_what_would_make_patient_ambiguous() {
    let config = config_with_people();
    let run = |args: &[&str]| {
        labcorp_in(config.clone())
            .args(["--json", "patients", "label", "--profile", "adult1"])
            .args(args)
            .assert()
    };
    // Held by another person: exit 2, with the remedy that clears it.
    let held = run(&["--patient", KID, "ME"]).code(2);
    let e = error_of(&held);
    assert!(
        e["error"]["remedy"].as_str().unwrap().contains("--patient ME --clear"),
        "{e}"
    );
    // A person's id, an empty label, both or neither: exit 2.
    run(&["--patient", KID, ME]).code(2);
    run(&["--patient", KID, "  "]).code(2);
    run(&["--patient", KID, "kid", "--clear"]).code(2);
    run(&["--patient", KID]).code(2);
    assert_eq!(label_of(&config, KID), Value::Null);
    assert_eq!(label_of(&config, ME), "me");
}

#[test]
fn patients_label_for_no_one_listed_is_exit_4_and_repeats_nothing() {
    let config = config_with_people();
    for (profile, q) in [("adult1", "l00000000000000ff"), ("adult1", "Jordan"), ("adult2", ME)] {
        let out = labcorp_in(config.clone())
            .args(["--json", "patients", "label", "--profile", profile, "--patient", q, "x"])
            .assert()
            .code(4);
        let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
        assert!(!text.contains(q), "{q} in: {text}");
        assert!(text.contains("patients list"), "{text}");
    }
}

#[test]
fn config_show_prints_counts_not_patient_ids_or_labels() {
    for json in [true, false] {
        let mut cmd = labcorp_in(config_with_people());
        if json {
            cmd.arg("--json");
        }
        let out = cmd.args(["config", "show"]).assert().success();
        let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
        assert!(!text.contains(ME) && !text.contains(KID), "{text}");
        assert!(!text.contains("\"me\"") && !text.contains(" me\n"), "{text}");
        if json {
            let v: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["profiles"]["adult1"]["members"]["people"], 2, "{v}");
            assert_eq!(v["profiles"]["adult1"]["members"]["labelled"], 1, "{v}");
        }
    }
}
