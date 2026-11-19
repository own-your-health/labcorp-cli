//! The repository carries no personal data; runtime output may.
//!
//! Walks every tracked file (`git ls-files`) for the *shapes* personal data
//! takes (phone numbers, long id numbers, street addresses, email
//! addresses) rather than a list of real values, which would publish what
//! it exists to keep out.

use std::process::Command;

fn root() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files"])
        .current_dir(root())
        .output()
        .expect("git ls-files");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn digits_run(s: &str, min: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            if cur.len() >= min {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    out
}

/// A digit run is a dummy when it is all one digit, zeros then one digit
/// (`00000001`), or a 555 phone number: `555…`, or any area code with the
/// 555-0100 to 555-0199 lines reserved for fiction.
fn is_dummy_number(n: &str) -> bool {
    let first = n.chars().next().unwrap();
    let head = &n[..n.len() - 1];
    let local = n.strip_prefix('1').filter(|r| r.len() == 10).unwrap_or(n);
    let fictional = local.len() == 10 && &local[3..8] == "55501";
    n.chars().all(|c| c == first)
        || head.chars().all(|c| c == '0')
        || n.starts_with("555")
        || n.starts_with("1555")
        || fictional
}

const PUBLIC_EMAIL_DOMAINS: &[&str] = &["example.com", "labcorp.com"];

/// The personal-data shapes on one line, each as `kind value`.
fn scan_line(line: &str) -> Vec<String> {
    let mut findings = Vec::new();
    let phoneish = line.replace(['(', ')', '-', '.', ' '], "");
    for n in digits_run(line, 7)
        .into_iter()
        .chain(digits_run(&phoneish, 10).into_iter().filter(|n| n.len() <= 11))
    {
        if !is_dummy_number(&n) {
            findings.push(format!("number {n}"));
        }
    }
    for word in line.split(|c: char| c.is_whitespace() || "\"'<>(),;`".contains(c)) {
        if let Some((local, domain)) = word.split_once('@') {
            let domain = domain.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
            let ok = local.is_empty()
                || !domain.contains('.')
                || PUBLIC_EMAIL_DOMAINS.contains(&domain)
                || domain.ends_with(".example")
                || domain.ends_with("users.noreply.github.com");
            if !ok {
                findings.push(format!("email {word}"));
            }
        }
    }
    let lower = line.to_ascii_lowercase();
    for suffix in [" way", " st", " ave", " blvd", " dr", " ln", " ct", " rd"] {
        if let Some(pos) = lower.find(suffix) {
            let has_number = lower[..pos]
                .split_whitespace()
                .rev()
                .take(4)
                .any(|w| w.len() >= 3 && w.chars().all(|c| c.is_ascii_digit()));
            if has_number && !lower.contains("example st") {
                findings.push(format!("address-like `{}`", line.trim()));
            }
        }
    }
    findings
}

/// `<commit>:<path>:<rule>:<line>`: a 40-character hex commit, a path with
/// no space, a lowercase rule id and a line number.
fn is_fingerprint(line: &str) -> bool {
    let parts: Vec<&str> = line.split(':').collect();
    let [commit, path, rule, number] = parts.as_slice() else {
        return false;
    };
    commit.len() == 40
        && commit.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && !path.is_empty()
        && !path.contains(char::is_whitespace)
        && !rule.is_empty()
        && rule.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

#[test]
fn a_fingerprint_is_only_a_fingerprint() {
    assert!(is_fingerprint(
        "865517eb837b9fb4fc7d1983f6cc1de7376015f8:src/site.rs:generic-api-key:120"
    ));
    assert!(!is_fingerprint(
        "865517eb837b9fb4fc7d1983f6cc1de7376015f8:src/site.rs:generic-api-key:120 note"
    ));
    assert!(!is_fingerprint("call 5551234567"));
    assert!(!is_fingerprint("865517e:src/site.rs:generic-api-key:120"));
}

#[test]
fn tracked_files_carry_no_personal_data() {
    let files = tracked_files();
    if files.is_empty() {
        // Not a git checkout (a packaged crate): nothing to scan.
        return;
    }
    let mut findings = Vec::new();
    for file in files {
        if file.ends_with(".lock") || file.starts_with("LICENSE") || file == "tests/no_personal_data.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(format!("{}/{file}", root())) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            // A gitleaks fingerprint's commit hash can hold a phone-length
            // digit run. Such a line is checked for that shape instead, so
            // nothing else can ride along in the file uncommented.
            if file == ".gitleaksignore" && !line.starts_with('#') {
                if !is_fingerprint(line) {
                    findings.push(format!("{file}:{}: not a gitleaks fingerprint", i + 1));
                }
                continue;
            }
            for f in scan_line(line) {
                findings.push(format!("{file}:{}: {f}", i + 1));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "personal-data shapes in tracked files:\n{}",
        findings.join("\n")
    );
}

#[test]
fn no_tracked_file_is_a_raw_capture_or_a_report() {
    for file in tracked_files() {
        let lower = file.to_ascii_lowercase();
        for ext in [".pdf", ".har", ".csv", ".xlsx", ".raw.json", ".raw.html", ".ndjson"] {
            assert!(
                !lower.ends_with(ext),
                "{file} is tracked; real reports and captures never are"
            );
        }
    }
}

#[test]
fn the_scanner_flags_real_shapes_and_passes_dummies() {
    for real in [
        "call (772) 404-1234",
        "call 888-555-0200",
        "member id 9234567812",
        "mail someone@gmail.com",
        "lives at 1234 NW Oak St, Somewhere",
        "claim 7342918",
    ] {
        assert!(!scan_line(real).is_empty(), "missed: {real}");
    }
    for dummy in [
        "call (555) 010-0001",
        "customer service 888-555-0100",
        "SYNP000001 SYNR00000003",
        "000000000000000-A-S-PS-E",
        "pat@example.com",
        "100 Example St, Exampleton",
        "$16,000.00",
    ] {
        assert!(
            scan_line(dummy).is_empty(),
            "false positive: {dummy}: {:?}",
            scan_line(dummy)
        );
    }
}
