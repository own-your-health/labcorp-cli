//! Labcorp Patient's answers, read into labcorp's model (docs/api.md,
//! "Reads"). Fields are read by name from the JSON, a number accepted where
//! text is expected, so a field the site renames reads as absent rather than
//! failing the whole answer; `tests/fixture_shapes.rs` holds the fields that
//! matter to their shapes.

use pk_cli_core::{dates, CliError};
use serde_json::Value;

/// A field as text: a non-blank string trimmed, or a number written out.
pub fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    v.get(key)?.as_bool()
}

fn count(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The entries of a list answer: the array itself, or the first array an
/// object answer holds.
pub fn entries(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(items) => items.iter().collect(),
        Value::Object(map) => map
            .values()
            .find_map(|v| v.as_array())
            .map(|items| items.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `YYYY-MM-DD` from the site's date forms: an ISO date or date-time (the
/// day as written, whatever offset follows), or `MM/DD/YYYY`.
pub fn day(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        return dates::parse_iso(&s[..10]).ok().map(dates::fmt_iso);
    }
    dates::parse_mm_slash_dd_yyyy(s.get(..10).unwrap_or(s)).map(dates::fmt_iso)
}

/// An RFC 3339 instant, when `s` is a date-time with an offset or `Z`.
/// The site's `dateOfService` ends in a `Z` the web app strips: it is the
/// lab's local time, not UTC, so it is never read through this.
pub fn instant(s: &str) -> Option<String> {
    let s = s.trim();
    let t = s.get(10..)?;
    let zoned = t.starts_with('T') && (t.ends_with('Z') || t.rfind(['+', '-']).is_some_and(|i| i > 6));
    (zoned && day(s).is_some()).then(|| s.to_string())
}

// --------------------------------------------------------------- people

/// How a person reaches the signed-in account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The account holder.
    Account,
    /// A dependent the account manages.
    Dependent,
    /// An account shared with this one (`linkedAccounts`).
    Linked,
}

/// A person whose results the account reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// Labcorp's patient id (`id`, or a linked account's `patientMasterId`).
    pub id: String,
    pub kind: Kind,
    /// A linked account's `recipientLinkedId`, else its `linkedId`: the
    /// segment its results are read under.
    pub linked_id: Option<String>,
    pub first_name: Option<String>,
    pub middle_name: Option<String>,
    pub last_name: Option<String>,
    /// `YYYY-MM-DD`.
    pub birth_date: Option<String>,
    /// As the site writes it (`MALE`, `FEMALE`, `UNKNOWN`).
    pub gender: Option<String>,
}

impl Person {
    fn read(v: &Value, id: String, kind: Kind) -> Person {
        Person {
            id,
            kind,
            linked_id: text(v, "recipientLinkedId").or_else(|| text(v, "linkedId")),
            first_name: text(v, "firstName"),
            middle_name: text(v, "middleName"),
            last_name: text(v, "lastName"),
            birth_date: text(v, "dateOfBirth").and_then(|d| day(&d)),
            gender: text(v, "gender"),
        }
    }

    /// The signed-in account, from `GET /protected/patients/current`.
    pub fn account(v: &Value) -> Result<Person, CliError> {
        let id = text(v, "id").ok_or_else(|| {
            CliError::Upstream("the account's record carries no `id` (docs/api.md, \"Reads\")".into())
        })?;
        Ok(Person::read(v, id, Kind::Account))
    }

    /// The name as the site shows it, for the person's own terminal.
    pub fn name(&self) -> Option<String> {
        let parts: Vec<&str> = [&self.first_name, &self.middle_name, &self.last_name]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// The path segment the person's results are read under:
    /// `/protected/patients/current{scope}/results/...`.
    pub fn scope(&self) -> String {
        match (self.kind, &self.linked_id) {
            (Kind::Account, _) => String::new(),
            (_, Some(id)) => format!("/linkedAccounts/{id}"),
            (_, None) => "/linkedAccounts".into(),
        }
    }
}

/// Everyone the account reads: itself, the dependents its record lists
/// (`dependents[].dependent`), and the linked accounts that accepted
/// sharing with it (`linkedAcctToCurrentUserStatus`, or for a minor
/// `currentUserToLinkedAcctStatus`, `accepted`), each once by id.
pub fn household(account: &Value, linked: &Value) -> Result<Vec<Person>, CliError> {
    let mut people = vec![Person::account(account)?];
    let mut add = |p: Person| {
        if !people.iter().any(|q| q.id == p.id) {
            people.push(p);
        }
    };
    if let Some(deps) = account.get("dependents").and_then(Value::as_array) {
        for entry in deps {
            let d = entry.get("dependent").unwrap_or(entry);
            if let Some(id) = text(d, "id") {
                add(Person::read(d, id, Kind::Dependent));
            }
        }
    }
    for l in entries(linked) {
        let accepted = |k: &str| text(l, k).is_some_and(|s| s.eq_ignore_ascii_case("accepted"));
        let minor = flag(l, "isMinor").unwrap_or(false);
        if !(accepted("linkedAcctToCurrentUserStatus") || minor && accepted("currentUserToLinkedAcctStatus")) {
            continue;
        }
        if let Some(id) = text(l, "patientMasterId").or_else(|| text(l, "id")) {
            add(Person::read(l, id, Kind::Linked));
        }
    }
    Ok(people)
}

// -------------------------------------------------------------- results

/// One result as the results list shows it: an accession, with the tests
/// ordered on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub id: String,
    /// The patient's id (`pid`).
    pub patient_id: Option<String>,
    pub patient_name: Option<String>,
    /// When the specimen was collected, as written (the lab's local time).
    pub date_of_service: Option<String>,
    pub report_date: Option<String>,
    pub ordered_date: Option<String>,
    pub ordering_provider: Option<String>,
    /// The ordering practice.
    pub account_name: Option<String>,
    pub detail_available: bool,
    pub pdf_available: bool,
    /// `new`, `upcoming`, `updated`, `recently_viewed`.
    pub category: Option<String>,
    pub test_codes: Vec<String>,
    pub total_tests: Option<u64>,
    pub completed_tests: Option<u64>,
    /// Each ordered test's status (`itemStatuses`' `status`).
    pub statuses: Vec<String>,
}

impl Header {
    pub fn read(v: &Value) -> Result<Header, CliError> {
        let id = text(v, "id")
            .ok_or_else(|| CliError::Upstream("a result carries no `id` (docs/api.md, \"Reads\")".into()))?;
        let statuses = match v.get("itemStatuses") {
            Some(Value::Object(map)) => map.values().filter_map(|s| text(s, "status")).collect(),
            Some(Value::Array(items)) => items.iter().filter_map(|s| text(s, "status")).collect(),
            _ => Vec::new(),
        };
        let test_codes = match v.get("testCodes") {
            Some(Value::Array(codes)) => codes
                .iter()
                .filter_map(|c| match c {
                    Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect(),
            Some(Value::String(s)) => s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        Ok(Header {
            id,
            patient_id: text(v, "pid"),
            patient_name: text(v, "patientName"),
            date_of_service: text(v, "dateOfService"),
            report_date: text(v, "reportDate"),
            ordered_date: text(v, "orderedDate"),
            ordering_provider: text(v, "orderingProviderName"),
            account_name: text(v, "accountName"),
            detail_available: flag(v, "isDetailAvailable").unwrap_or(true),
            pdf_available: flag(v, "hasMinimumDataForPdfRetrieval").unwrap_or(true),
            category: text(v, "dashboardCategory"),
            test_codes,
            total_tests: count(v, "totalTests"),
            completed_tests: count(v, "completedTests"),
            statuses,
        })
    }

    /// The day of service, `YYYY-MM-DD`.
    pub fn service_day(&self) -> Option<String> {
        self.date_of_service.as_deref().and_then(day)
    }

    /// The day of the report, `YYYY-MM-DD`.
    pub fn report_day(&self) -> Option<String> {
        self.report_date.as_deref().and_then(day)
    }

    /// The later of the report's and the service's day: when the result
    /// last changed, as far as the list says.
    pub fn changed_day(&self) -> Option<String> {
        match (self.report_day(), self.service_day()) {
            (Some(r), Some(s)) => Some(r.max(s)),
            (r, s) => r.or(s),
        }
    }

    /// Whether every ordered test has reported: none `UPCOMING` or
    /// `PRELIMINARY` (without statuses, not in `upcoming`), as the web app
    /// decides it.
    pub fn complete(&self) -> bool {
        if self.statuses.is_empty() {
            return !self
                .category
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case("upcoming"));
        }
        !self
            .statuses
            .iter()
            .any(|s| s.eq_ignore_ascii_case("upcoming") || s.eq_ignore_ascii_case("preliminary"))
    }

    /// `final`, `partial` (some tests in, some to come), `preliminary`, or
    /// `registered` (nothing in yet).
    pub fn status(&self) -> &'static str {
        let any = |w: &str| self.statuses.iter().any(|s| s.eq_ignore_ascii_case(w));
        let all = |w: &str| !self.statuses.is_empty() && self.statuses.iter().all(|s| s.eq_ignore_ascii_case(w));
        if self.complete() {
            "final"
        } else if all("upcoming") || (self.statuses.is_empty() && !self.detail_available) {
            "registered"
        } else if any("preliminary") && !any("upcoming") {
            "preliminary"
        } else {
            "partial"
        }
    }
}

/// The results list (`results/headers/all`): every result's header. A
/// header without an id fails the read: it could be neither shown nor read.
pub fn headers(v: &Value) -> Result<Vec<Header>, CliError> {
    entries(v).into_iter().map(Header::read).collect()
}

/// One result's detail (`results/{id}`): its header and each ordered test
/// with its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub header: Header,
    pub items: Vec<Item>,
}

/// An ordered test (`orderedItems[]`): a panel or a single test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub test_name: Option<String>,
    pub test_code: Option<String>,
    pub summary: Option<String>,
    pub status: Option<String>,
    pub lines: Vec<Line>,
}

/// One reported value (`orderedItems[].results[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Labcorp's code for the result (`number`).
    pub number: Option<String>,
    pub name: Option<String>,
    /// As written: a number, a comparison (`<0.5`), or text.
    pub value: Option<String>,
    /// `H`, `L`, `HH`, `LL`, `A`, ...; `T` not performed, `Q` quantity not
    /// sufficient.
    pub abnormal: Option<String>,
    pub range: Option<String>,
    pub units: Option<String>,
    pub status: Option<String>,
    pub comments: Vec<String>,
}

impl Line {
    fn read(v: &Value) -> Line {
        Line {
            number: text(v, "number"),
            name: text(v, "name"),
            value: match v.get("value") {
                Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim_end().to_string()),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => None,
            },
            abnormal: text(v, "abnormalIndicator"),
            range: text(v, "referenceRange"),
            units: text(v, "units"),
            status: text(v, "status"),
            comments: comments(v.get("comments")),
        }
    }

    /// A line the web app shows as a note, not a value: its name starts
    /// `please note:`, `comment:`, `disclaimer`, `interpretation` or `note:`.
    pub fn is_note(&self) -> bool {
        self.name.as_deref().is_some_and(|n| {
            let n = n.trim().to_ascii_lowercase();
            ["please note:", "comment:", "disclaimer", "interpretation", "note:"]
                .iter()
                .any(|p| n.starts_with(p))
        })
    }
}

/// A result's comments: strings, or objects whose string members are the
/// text.
fn comments(v: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|c| match c {
            Value::String(s) => Some(s.trim_end().to_string()),
            Value::Object(map) => {
                let parts: Vec<&str> = map.values().filter_map(Value::as_str).map(str::trim_end).collect();
                Some(parts.join("\n"))
            }
            _ => None,
        })
        .filter(|s| !s.trim().is_empty())
        .collect()
}

impl Detail {
    pub fn read(v: &Value) -> Result<Detail, CliError> {
        let header = Header::read(v.get("header").unwrap_or(v))?;
        let items = v
            .get("orderedItems")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|i| Item {
                        test_name: text(i, "testName"),
                        test_code: text(i, "testCode"),
                        summary: text(i, "summary"),
                        status: text(i, "itemStatus"),
                        lines: i
                            .get("results")
                            .and_then(Value::as_array)
                            .map(|r| r.iter().map(Line::read).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Detail { header, items })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn days_read_from_each_form() {
        assert_eq!(day("2026-03-04T09:15:00Z").as_deref(), Some("2026-03-04"));
        assert_eq!(day("2026-03-04").as_deref(), Some("2026-03-04"));
        assert_eq!(day("03/04/2026").as_deref(), Some("2026-03-04"));
        assert_eq!(day("soon"), None);
        assert_eq!(instant("2026-03-04T09:15:00Z").as_deref(), Some("2026-03-04T09:15:00Z"));
        assert_eq!(
            instant("2026-03-04T09:15:00-05:00").as_deref(),
            Some("2026-03-04T09:15:00-05:00")
        );
        assert_eq!(instant("2026-03-04T09:15:00"), None);
        assert_eq!(instant("2026-03-04"), None);
    }

    #[test]
    fn the_household_is_the_account_its_dependents_and_accepted_links() {
        let account = json!({
            "id": 1, "firstName": "Alex", "lastName": "Example", "dateOfBirth": "1990-01-01",
            "dependents": [{ "relationshipType": "CHILD", "dependent": { "id": 2, "firstName": "Sam" } }],
        });
        let linked = json!([
            { "patientMasterId": "3", "linkedId": "L3", "linkedAcctToCurrentUserStatus": "accepted" },
            { "patientMasterId": "4", "linkedId": "L4", "linkedAcctToCurrentUserStatus": "initiated" },
            { "patientMasterId": "5", "recipientLinkedId": "R5", "linkedId": "L5", "isMinor": true,
              "currentUserToLinkedAcctStatus": "accepted" },
            { "patientMasterId": "1", "linkedAcctToCurrentUserStatus": "accepted" },
        ]);
        let people = household(&account, &linked).unwrap();
        let ids: Vec<&str> = people.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3", "5"]);
        assert_eq!(people[0].scope(), "");
        assert_eq!(people[0].name().as_deref(), Some("Alex Example"));
        assert_eq!(people[1].scope(), "/linkedAccounts");
        assert_eq!(people[2].scope(), "/linkedAccounts/L3");
        assert_eq!(people[3].scope(), "/linkedAccounts/R5");
    }

    #[test]
    fn a_header_says_how_complete_it_is() {
        let h = |statuses: Value, category: &str| {
            Header::read(&json!({ "id": "r1", "itemStatuses": statuses, "dashboardCategory": category })).unwrap()
        };
        assert_eq!(h(json!({ "a": { "status": "FINAL" } }), "new").status(), "final");
        assert_eq!(
            h(
                json!({ "a": { "status": "FINAL" }, "b": { "status": "UPCOMING" } }),
                "new"
            )
            .status(),
            "partial"
        );
        assert_eq!(
            h(json!({ "a": { "status": "PRELIMINARY" } }), "new").status(),
            "preliminary"
        );
        assert_eq!(
            h(json!({ "a": { "status": "UPCOMING" } }), "upcoming").status(),
            "registered"
        );
        assert_eq!(h(json!({}), "upcoming").status(), "partial");
        assert!(Header::read(&json!({ "pid": 1 })).is_err());
    }

    #[test]
    fn a_detail_reads_its_items_and_lines() {
        let d = Detail::read(&json!({
            "header": { "id": "r1", "dateOfService": "2026-03-04T09:15:00Z" },
            "orderedItems": [{
                "testName": "Panel", "testCode": "000001", "itemStatus": "FINAL",
                "results": [
                    { "number": "000002", "name": "Value A", "value": "1.50", "units": "mg/dL",
                      "referenceRange": "0.5-2.0", "comments": ["first", { "text": "second" }] },
                    { "number": "000003", "name": "Please Note:", "value": "dummy note" },
                ],
            }],
        }))
        .unwrap();
        assert_eq!(d.header.service_day().as_deref(), Some("2026-03-04"));
        let lines = &d.items[0].lines;
        assert_eq!(lines[0].value.as_deref(), Some("1.50"));
        assert_eq!(lines[0].comments, ["first", "second"]);
        assert!(!lines[0].is_note() && lines[1].is_note());
    }
}
