//! Labcorp Patient's results as FHIR R4, mapped by hand (no FHIR library):
//! the person as a `Patient`, Labcorp as the `Organization`, each result as a
//! `DiagnosticReport` whose `result` are one `Observation` per ordered test,
//! each a panel whose `hasMember` are the values reported for it, and the
//! report PDF as a `Binary` with the `DocumentReference` that files it
//! (medical-common DESIGN.md §1.4, §1.7).
//!
//! Every id derives from Labcorp's own ids, so a re-export carries the same
//! ids and the store merges it: `lcp-` a person, `lcr-` a result, `lco-` an
//! observation, `lcd-` and `lcb-` the report's document and file. Every
//! resource carries `meta.source` [`META_SOURCE`] and the tag
//! [`TAG_MAPPED`]. A value keeps its written spelling (`1.50` stays `1.50`):
//! [`resource`] writes the JSON with each number's own text.

use base64::Engine;
use pk_cli_core::CliError;
use pk_med_records::RawResource;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::portal::{self, Detail, Header, Item, Line, Person};

pub const ORGANIZATION_ID: &str = "labcorp";
pub const ORGANIZATION_NAME: &str = "Labcorp";
pub const META_SOURCE: &str = "labcorp:portal";
pub const TAG_SYSTEM: &str = "https://github.com/piekstra/labcorp-cli/fhir/tag";
pub const TAG_MAPPED: &str = "mapped-from-labcorp-patient";
/// Labcorp's own codes: an ordered test's `testCode`, a value's `number`.
pub const TEST_CODE_SYSTEM: &str = "https://www.labcorp.com/tests";
pub const RESULT_CODE_SYSTEM: &str = "https://www.labcorp.com/tests/result-code";
/// Labcorp Patient's id of a result, and of a person.
pub const RESULT_ID_SYSTEM: &str = "https://patient.labcorp.com/fhir/result-id";
pub const PATIENT_ID_SYSTEM: &str = "https://patient.labcorp.com/fhir/patient-id";
/// The abnormal flag as Labcorp writes it.
pub const FLAG_SYSTEM: &str = "https://github.com/piekstra/labcorp-cli/fhir/abnormal-indicator";
const INTERPRETATION: &str = "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation";
const OBSERVATION_CATEGORY: &str = "http://terminology.hl7.org/CodeSystem/observation-category";
const DIAGNOSTIC_SERVICE: &str = "http://terminology.hl7.org/CodeSystem/v2-0074";
const DATA_ABSENT: &str = "http://terminology.hl7.org/CodeSystem/data-absent-reason";
const LOINC: &str = "http://loinc.org";
/// LOINC's "Laboratory report" document type.
const LAB_REPORT_LOINC: &str = "11502-2";
/// FHIR's limit on an id's length.
const MAX_ID: usize = 64;

fn meta() -> Value {
    json!({
        "source": META_SOURCE,
        "tag": [{ "system": TAG_SYSTEM, "code": TAG_MAPPED, "display": "Mapped by labcorp from Labcorp Patient" }],
    })
}

/// `<prefix>-<parts>` as a FHIR id: the parts joined by `-`, every character
/// outside `[A-Za-z0-9.-]` made a `.`. One over 64 characters becomes
/// `<prefix>-` and 40 hex digits of its SHA-256, still stable.
pub fn id(prefix: &str, parts: &[&str]) -> String {
    let joined: String = parts
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '.'
            }
        })
        .collect();
    let plain = format!("{prefix}-{joined}");
    if plain.len() <= MAX_ID {
        return plain;
    }
    let digest = Sha256::digest(joined.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{}", &hex[..40])
}

pub fn patient_id(p: &Person) -> String {
    id("lcp", &[&p.id])
}

pub fn report_id(h: &Header) -> String {
    id("lcr", &[&h.id])
}

pub fn document_id(h: &Header) -> String {
    id("lcd", &[&h.id])
}

pub fn binary_id(h: &Header) -> String {
    id("lcb", &[&h.id])
}

/// The observation ids of one report: one per ordered test (its test code,
/// else its place) and one per value (that and its result code, else its
/// place). An id met twice in the report gets its place appended, so no
/// value is dropped as a repeat.
pub struct Ids {
    pub panels: Vec<String>,
    pub lines: Vec<Vec<String>>,
}

impl Ids {
    pub fn of(d: &Detail) -> Ids {
        let h = &d.header;
        let mut seen = std::collections::HashSet::new();
        let mut unique = |candidate: String, place: &str| {
            if seen.insert(candidate.clone()) {
                return candidate;
            }
            let again = id("lco", &[&candidate["lco-".len()..], place]);
            seen.insert(again.clone());
            again
        };
        let mut panels = Vec::new();
        let mut lines = Vec::new();
        for (i, item) in d.items.iter().enumerate() {
            let test = item.test_code.clone().unwrap_or_else(|| format!("i{i}"));
            panels.push(unique(id("lco", &[&h.id, &test]), &format!("i{i}")));
            lines.push(
                item.lines
                    .iter()
                    .enumerate()
                    .map(|(at, l)| {
                        let key = l.number.clone().unwrap_or_else(|| format!("n{at}"));
                        unique(id("lco", &[&h.id, &test, &key]), &format!("i{i}n{at}"))
                    })
                    .collect(),
            );
        }
        Ids { panels, lines }
    }
}

// ------------------------------------------------------- spelled numbers

/// A FHIR decimal as written: `-?(0|[1-9][0-9]*)(\.[0-9]+)?`.
pub fn is_decimal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
    digits(int) && (int == "0" || !int.starts_with('0')) && frac.is_none_or(digits)
}

/// A resource whose numbers are given as text: each [`Numbers::put`] leaves a
/// placeholder, replaced by the number's own spelling when the resource is
/// written ([`resource`]).
#[derive(Default)]
pub struct Numbers(Vec<String>);

impl Numbers {
    fn put(&mut self, decimal: &str) -> Value {
        self.0.push(decimal.to_string());
        Value::String(format!("\u{1}labcorp-number-{}\u{1}", self.0.len() - 1))
    }
}

/// `v` as a resource line, each placeholder replaced by its number's
/// spelling. A placeholder that is not found exactly once (a value of the
/// record that happens to spell one) leaves the resource as parsed numbers,
/// so the line is always a valid resource.
pub fn resource(v: &Value, numbers: &Numbers) -> Result<RawResource, CliError> {
    let mut text = serde_json::to_string(v).map_err(|e| CliError::Other(format!("writing a resource: {e}")))?;
    let mut spelled = true;
    for (i, n) in numbers.0.iter().enumerate() {
        let quoted = format!("\"\\u0001labcorp-number-{i}\\u0001\"");
        if text.matches(&quoted).count() != 1 {
            spelled = false;
            break;
        }
        text = text.replacen(&quoted, n, 1);
    }
    if spelled {
        return RawResource::parse(&text);
    }
    let mut v = v.clone();
    unspell(&mut v, numbers);
    RawResource::from_value(&v)
}

fn unspell(v: &mut Value, numbers: &Numbers) {
    match v {
        Value::String(s) if s.starts_with("\u{1}labcorp-number-") => {
            let n = s
                .trim_matches('\u{1}')
                .trim_start_matches("labcorp-number-")
                .parse::<usize>()
                .ok()
                .and_then(|i| numbers.0.get(i))
                .and_then(|n| serde_json::from_str::<Value>(n).ok());
            if let Some(n) = n {
                *v = n;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| unspell(v, numbers)),
        Value::Object(map) => map.values_mut().for_each(|v| unspell(v, numbers)),
        _ => {}
    }
}

// ------------------------------------------------------------ resources

pub fn organization() -> Value {
    json!({
        "resourceType": "Organization",
        "id": ORGANIZATION_ID,
        "meta": meta(),
        "active": true,
        "name": ORGANIZATION_NAME,
        "type": [{ "coding": [{
            "system": "http://terminology.hl7.org/CodeSystem/organization-type",
            "code": "prov",
            "display": "Healthcare Provider",
        }] }],
        "telecom": [{ "system": "url", "value": "https://www.labcorp.com" }],
    })
}

fn org_ref() -> Value {
    json!({ "reference": format!("Organization/{ORGANIZATION_ID}"), "display": ORGANIZATION_NAME })
}

fn patient_ref(p: &Person) -> Value {
    json!({ "reference": format!("Patient/{}", patient_id(p)) })
}

pub fn patient(p: &Person) -> Value {
    let mut v = json!({
        "resourceType": "Patient",
        "id": patient_id(p),
        "meta": meta(),
        "identifier": [{ "system": PATIENT_ID_SYSTEM, "value": p.id }],
        "managingOrganization": org_ref(),
    });
    let given: Vec<&str> = [&p.first_name, &p.middle_name]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if !given.is_empty() || p.last_name.is_some() {
        let mut name = json!({ "use": "official" });
        if let Some(text) = p.name() {
            name["text"] = text.into();
        }
        if let Some(last) = &p.last_name {
            name["family"] = last.clone().into();
        }
        if !given.is_empty() {
            name["given"] = json!(given);
        }
        v["name"] = json!([name]);
    }
    if let Some(g) = p.gender.as_deref().and_then(gender) {
        v["gender"] = g.into();
    }
    if let Some(b) = &p.birth_date {
        v["birthDate"] = b.clone().into();
    }
    v
}

fn gender(g: &str) -> Option<&'static str> {
    match g.trim().to_ascii_lowercase().as_str() {
        "male" | "m" => Some("male"),
        "female" | "f" => Some("female"),
        "unknown" | "u" => Some("unknown"),
        "other" | "o" => Some("other"),
        _ => None,
    }
}

/// An observation's or a panel's status, from Labcorp's word for it; `None`
/// for a word this does not know.
fn observation_status(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "final" | "complete" | "completed" | "f" => Some("final"),
        "corrected" | "c" => Some("corrected"),
        "amended" | "updated" | "a" => Some("amended"),
        "preliminary" | "p" | "partial" => Some("preliminary"),
        "cancelled" | "canceled" | "x" => Some("cancelled"),
        "upcoming" | "pending" | "registered" | "i" => Some("registered"),
        _ => None,
    }
}

fn header_observation_status(h: &Header) -> &'static str {
    match h.status() {
        "final" => "final",
        "registered" => "registered",
        _ => "preliminary",
    }
}

fn lab_category() -> Value {
    json!([{ "coding": [{ "system": OBSERVATION_CATEGORY, "code": "laboratory", "display": "Laboratory" }] }])
}

fn effective(v: &mut Value, h: &Header) {
    if let Some(d) = h.service_day() {
        v["effectiveDateTime"] = d.into();
    }
    if let Some(i) = h.report_date.as_deref().and_then(portal::instant) {
        v["issued"] = i.into();
    }
}

/// The flag Labcorp writes beside a value, as FHIR's interpretation where
/// it has one, always with Labcorp's own code.
fn interpretation(flag: &str) -> Value {
    let display = match flag {
        "H" => Some("High"),
        "L" => Some("Low"),
        "HH" => Some("Critical high"),
        "LL" => Some("Critical low"),
        "A" => Some("Abnormal"),
        "AA" => Some("Critical abnormal"),
        "N" => Some("Normal"),
        _ => None,
    };
    let mut coding = Vec::new();
    if let Some(d) = display {
        coding.push(json!({ "system": INTERPRETATION, "code": flag, "display": d }));
    }
    coding.push(json!({ "system": FLAG_SYSTEM, "code": flag }));
    json!({ "coding": coding, "text": flag })
}

/// Why a value is absent, from the flag that says so: `T` (test not
/// performed), `Q` (quantity not sufficient).
fn absent_reason(flag: &str) -> Option<Value> {
    let text = match flag {
        "T" => "Test Not Performed",
        "Q" => "Quantity Not Sufficient",
        _ => return None,
    };
    Some(json!({
        "coding": [{ "system": DATA_ABSENT, "code": "not-performed", "display": "Not Performed" }],
        "text": text,
    }))
}

/// `<`, `<=`, `>=` or `>` and a decimal, or a decimal alone.
fn comparison(value: &str) -> Option<(Option<&str>, &str)> {
    let v = value.trim();
    for c in ["<=", ">=", "<", ">"] {
        if let Some(rest) = v.strip_prefix(c) {
            let rest = rest.trim();
            return is_decimal(rest).then_some((Some(c), rest));
        }
    }
    is_decimal(v).then_some((None, v))
}

fn quantity(n: &mut Numbers, decimal: &str, units: Option<&str>) -> Value {
    let mut q = json!({ "value": n.put(decimal) });
    if let Some(u) = units {
        q["unit"] = u.into();
    }
    q
}

/// A reference range as written, with its low and high when it is
/// `<low>-<high>`, `<<high>`, `<=<high>`, `><low>` or `>=<low>`.
fn reference_range(n: &mut Numbers, range: &str, units: Option<&str>) -> Value {
    let mut r = json!({ "text": range });
    let t = range.trim();
    if let Some((low, high)) = t.split_once('-').filter(|(l, _)| !l.trim().is_empty()) {
        let (low, high) = (low.trim(), high.trim());
        if is_decimal(low) && is_decimal(high) {
            r["low"] = quantity(n, low, units);
            r["high"] = quantity(n, high, units);
        }
    } else if let Some((Some(c), d)) = comparison(t) {
        let side = if c.starts_with('<') { "high" } else { "low" };
        r[side] = quantity(n, d, units);
    }
    r
}

/// One value reported for an ordered test.
pub fn value_observation(p: &Person, h: &Header, item: &Item, line: &Line, id: &str, n: &mut Numbers) -> Value {
    let status = line
        .status
        .as_deref()
        .and_then(observation_status)
        .or_else(|| item.status.as_deref().and_then(observation_status))
        .unwrap_or_else(|| header_observation_status(h));
    let name = line.name.clone().unwrap_or_else(|| "Result".into());
    let mut code = json!({ "text": name });
    if let Some(num) = &line.number {
        code["coding"] = json!([{ "system": RESULT_CODE_SYSTEM, "code": num, "display": name }]);
    }
    let mut v = json!({
        "resourceType": "Observation",
        "id": id,
        "meta": meta(),
        "status": status,
        "category": lab_category(),
        "code": code,
        "subject": patient_ref(p),
        "performer": [org_ref()],
    });
    effective(&mut v, h);
    let flag = line.abnormal.as_deref().map(str::trim).filter(|f| !f.is_empty());
    if let Some(reason) = flag.and_then(absent_reason) {
        v["dataAbsentReason"] = reason;
    } else if let Some(value) = &line.value {
        match comparison(value) {
            Some((c, d)) if !value.contains(['\n', '\r']) => {
                let mut q = quantity(n, d, line.units.as_deref());
                if let Some(c) = c {
                    q["comparator"] = c.into();
                }
                v["valueQuantity"] = q;
            }
            _ => v["valueString"] = value.clone().into(),
        }
    } else {
        v["dataAbsentReason"] = json!({
            "coding": [{ "system": DATA_ABSENT, "code": "unknown", "display": "Unknown" }],
        });
    }
    if let Some(f) = flag.filter(|f| absent_reason(f).is_none()) {
        v["interpretation"] = json!([interpretation(f)]);
    }
    if let Some(r) = &line.range {
        v["referenceRange"] = json!([reference_range(n, r, line.units.as_deref())]);
    }
    if !line.comments.is_empty() {
        v["note"] = line.comments.iter().map(|c| json!({ "text": c })).collect();
    }
    v
}

/// An ordered test: a panel whose members are its values.
pub fn panel_observation(p: &Person, h: &Header, item: &Item, id: &str, members: &[String]) -> Value {
    let name = item.test_name.clone().unwrap_or_else(|| "Laboratory test".into());
    let mut code = json!({ "text": name });
    if let Some(c) = &item.test_code {
        code["coding"] = json!([{ "system": TEST_CODE_SYSTEM, "code": c, "display": name }]);
    }
    let status = item
        .status
        .as_deref()
        .and_then(observation_status)
        .unwrap_or_else(|| header_observation_status(h));
    let mut v = json!({
        "resourceType": "Observation",
        "id": id,
        "meta": meta(),
        "status": status,
        "category": lab_category(),
        "code": code,
        "subject": patient_ref(p),
        "performer": [org_ref()],
    });
    effective(&mut v, h);
    if !members.is_empty() {
        v["hasMember"] = members
            .iter()
            .map(|m| json!({ "reference": format!("Observation/{m}") }))
            .collect();
    }
    if let Some(s) = &item.summary {
        v["note"] = json!([{ "text": s }]);
    }
    if item.lines.is_empty() {
        v["dataAbsentReason"] = json!({
            "coding": [{ "system": DATA_ABSENT, "code": "unknown", "display": "Unknown" }],
            "text": "No value reported yet",
        });
    }
    v
}

/// The title a report's PDF is filed under: `Labcorp <tests> <day>.pdf`.
pub fn pdf_title(h: &Header, names: &[String]) -> String {
    let what = if names.is_empty() {
        "result".to_string()
    } else {
        names.join(", ")
    };
    let day = h.service_day().unwrap_or_else(|| "undated".into());
    format!("Labcorp {what} {day}.pdf")
}

/// The result as a report: its tests and, with `pdf_size`, its PDF.
pub fn diagnostic_report(p: &Person, d: &Detail, ids: &Ids, pdf_size: Option<u64>) -> Value {
    let h = &d.header;
    let names: Vec<String> = d.items.iter().filter_map(|i| i.test_name.clone()).collect();
    let code = match d.items.as_slice() {
        [one] => {
            let name = one.test_name.clone().unwrap_or_else(|| "Laboratory report".into());
            let mut c = json!({ "text": name });
            if let Some(tc) = &one.test_code {
                c["coding"] = json!([{ "system": TEST_CODE_SYSTEM, "code": tc, "display": name }]);
            }
            c
        }
        _ if !names.is_empty() => json!({ "text": names.join(" \u{2022} ") }),
        _ => json!({ "coding": [{ "system": LOINC, "code": LAB_REPORT_LOINC, "display": "Laboratory report" }] }),
    };
    let mut v = json!({
        "resourceType": "DiagnosticReport",
        "id": report_id(h),
        "meta": meta(),
        "identifier": [{ "system": RESULT_ID_SYSTEM, "value": h.id }],
        "status": h.status(),
        "category": [{ "coding": [{ "system": DIAGNOSTIC_SERVICE, "code": "LAB", "display": "Laboratory" }] }],
        "code": code,
        "subject": patient_ref(p),
        "performer": [org_ref()],
    });
    effective(&mut v, h);
    let ordered_by: Vec<&str> = [&h.ordering_provider, &h.account_name]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if !ordered_by.is_empty() {
        v["basedOn"] =
            json!([{ "type": "ServiceRequest", "display": format!("Ordered by {}", ordered_by.join(", ")) }]);
    }
    let results: Vec<Value> = ids
        .panels
        .iter()
        .map(|id| json!({ "reference": format!("Observation/{id}") }))
        .collect();
    if !results.is_empty() {
        v["result"] = results.into();
    }
    if let Some(size) = pdf_size {
        v["presentedForm"] = json!([{
            "contentType": "application/pdf",
            "url": format!("Binary/{}", binary_id(h)),
            "size": size,
            "title": pdf_title(h, &names),
        }]);
    }
    v
}

pub fn binary(h: &Header, pdf: &[u8]) -> Value {
    json!({
        "resourceType": "Binary",
        "id": binary_id(h),
        "meta": meta(),
        "contentType": "application/pdf",
        "data": base64::engine::general_purpose::STANDARD.encode(pdf),
    })
}

/// The report's PDF as a document of the person, related to its report.
pub fn document_reference(p: &Person, d: &Detail, size: u64) -> Value {
    let h = &d.header;
    let names: Vec<String> = d.items.iter().filter_map(|i| i.test_name.clone()).collect();
    let mut attachment = json!({
        "contentType": "application/pdf",
        "url": format!("Binary/{}", binary_id(h)),
        "size": size,
        "title": pdf_title(h, &names),
    });
    let mut v = json!({
        "resourceType": "DocumentReference",
        "id": document_id(h),
        "meta": meta(),
        "identifier": [{ "system": RESULT_ID_SYSTEM, "value": h.id }],
        "status": "current",
        "type": { "coding": [{ "system": LOINC, "code": LAB_REPORT_LOINC, "display": "Laboratory report" }], "text": "Lab report" },
        "category": [{ "text": "lab-report" }],
        "subject": patient_ref(p),
        "author": [org_ref()],
        "description": format!("Labcorp report: {}", if names.is_empty() { "result".to_string() } else { names.join(", ") }),
        "context": { "related": [{ "reference": format!("DiagnosticReport/{}", report_id(h)) }] },
    });
    if let Some(day) = h.report_day().or_else(|| h.service_day()) {
        v["date"] = format!("{day}T00:00:00Z").into();
        attachment["creation"] = day.into();
    }
    v["content"] = json!([{ "attachment": attachment }]);
    v
}

/// `v` dated by when its page was read (`at`, RFC 3339): medical-common
/// DESIGN.md §1.4 dates a resource mapped from a patient site that way, so
/// a later export's version of it replaces an earlier one.
pub fn read_at(mut v: Value, at: &str) -> Value {
    v["meta"]["lastUpdated"] = at.into();
    v
}

/// A result's resources in stream order: each value, each ordered test,
/// the report, then with `pdf` its file and the document that files it.
/// Each is dated `at`, when the result was read.
pub fn report_resources(p: &Person, d: &Detail, pdf: Option<&[u8]>, at: &str) -> Result<Vec<RawResource>, CliError> {
    let h = &d.header;
    let ids = Ids::of(d);
    let mut out = Vec::new();
    for (i, item) in d.items.iter().enumerate() {
        for (place, line) in item.lines.iter().enumerate() {
            let mut n = Numbers::default();
            let v = read_at(value_observation(p, h, item, line, &ids.lines[i][place], &mut n), at);
            out.push(resource(&v, &n)?);
        }
        let v = read_at(panel_observation(p, h, item, &ids.panels[i], &ids.lines[i]), at);
        out.push(RawResource::from_value(&v)?);
    }
    let size = pdf.map(|b| b.len() as u64);
    out.push(RawResource::from_value(&read_at(
        diagnostic_report(p, d, &ids, size),
        at,
    ))?);
    if let Some(bytes) = pdf {
        out.push(RawResource::from_value(&read_at(binary(h, bytes), at))?);
        out.push(RawResource::from_value(&read_at(
            document_reference(p, d, bytes.len() as u64),
            at,
        ))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::Kind;

    fn person() -> Person {
        Person {
            id: "1".into(),
            kind: Kind::Account,
            linked_id: None,
            first_name: Some("Alex".into()),
            middle_name: None,
            last_name: Some("Example".into()),
            birth_date: Some("1990-01-01".into()),
            gender: Some("FEMALE".into()),
        }
    }

    fn header() -> Header {
        Header::read(&json!({
            "id": "r1", "dateOfService": "2026-03-04T09:15:00Z", "reportDate": "2026-03-05T10:00:00Z",
            "itemStatuses": { "a": { "status": "FINAL" } },
        }))
        .unwrap()
    }

    fn line(value: &str, flag: Option<&str>, range: Option<&str>) -> Line {
        Line {
            number: Some("000002".into()),
            name: Some("Value A".into()),
            value: Some(value.into()),
            abnormal: flag.map(str::to_string),
            range: range.map(str::to_string),
            units: Some("mg/dL".into()),
            status: None,
            comments: vec![],
        }
    }

    fn item(lines: Vec<Line>) -> Item {
        Item {
            test_name: Some("Panel".into()),
            test_code: Some("000001".into()),
            summary: None,
            status: Some("FINAL".into()),
            lines,
        }
    }

    fn written(v: &Value, n: &Numbers) -> String {
        resource(v, n).unwrap().raw
    }

    #[test]
    fn ids_are_stable_safe_and_short() {
        assert_eq!(id("lcr", &["r1"]), "lcr-r1");
        assert_eq!(id("lco", &["r 1", "a/b"]), "lco-r.1-a.b");
        let long = "x".repeat(80);
        let a = id("lcr", &[&long]);
        assert_eq!(a.len(), 44);
        assert_eq!(a, id("lcr", &[&long]));
    }

    #[test]
    fn decimals_keep_their_spelling() {
        assert!(is_decimal("1.50") && is_decimal("0") && is_decimal("-0.5"));
        assert!(!is_decimal("01") && !is_decimal("1.") && !is_decimal(".5") && !is_decimal("1e3"));
        let (h, l) = (header(), line("1.50", Some("H"), Some("0.50-1.20")));
        let it = item(vec![l.clone()]);
        let mut n = Numbers::default();
        let v = value_observation(&person(), &h, &it, &l, "lco-x", &mut n);
        let text = written(&v, &n);
        assert!(
            text.contains(r#""valueQuantity":{"value":1.50,"unit":"mg/dL"}"#),
            "{text}"
        );
        assert!(text.contains(r#""low":{"value":0.50,"unit":"mg/dL"}"#), "{text}");
        assert!(text.contains(r#""high":{"value":1.20,"unit":"mg/dL"}"#), "{text}");
        assert!(text.contains(r#""code":"H""#), "{text}");
        assert!(text.contains(r#""effectiveDateTime":"2026-03-04""#), "{text}");
        assert!(text.contains(r#""issued":"2026-03-05T10:00:00Z""#), "{text}");
    }

    #[test]
    fn a_comparison_is_a_comparator_and_text_stays_text() {
        let (h, mut n) = (header(), Numbers::default());
        let l = line("<0.5", None, Some(">39"));
        let it = item(vec![l.clone()]);
        let v = value_observation(&person(), &h, &it, &l, "lco-x", &mut n);
        assert_eq!(v["valueQuantity"]["comparator"], "<");
        assert!(written(&v, &n).contains(r#""low":{"value":39,"unit":"mg/dL"}"#));
        let l = line("Negative", None, None);
        let v = value_observation(&person(), &h, &it, &l, "lco-x", &mut Numbers::default());
        assert_eq!(v["valueString"], "Negative");
        let l = line("", Some("T"), None);
        let v = value_observation(&person(), &h, &it, &l, "lco-x", &mut Numbers::default());
        assert_eq!(v["dataAbsentReason"]["text"], "Test Not Performed");
        assert!(v.get("valueString").is_none() && v.get("interpretation").is_none());
    }

    #[test]
    fn a_placeholder_spelled_by_the_record_falls_back_to_numbers() {
        let mut n = Numbers::default();
        let placeholder = n.put("1.50");
        let v = json!({ "resourceType": "Observation", "a": placeholder.clone(), "b": placeholder });
        let text = written(&v, &n);
        assert_eq!(text, r#"{"resourceType":"Observation","a":1.5,"b":1.5}"#);
    }

    #[test]
    fn a_report_names_its_panels_and_its_pdf() {
        let l = line("1.50", None, None);
        let d = Detail {
            header: header(),
            items: vec![item(vec![l.clone(), l])],
        };
        let ids = Ids::of(&d);
        assert_eq!(ids.panels, ["lco-r1-000001"]);
        assert_eq!(ids.lines[0], ["lco-r1-000001-000002", "lco-r1-000001-000002-i0n1"]);
        let r = diagnostic_report(&person(), &d, &ids, Some(10));
        assert_eq!(r["id"], "lcr-r1");
        assert_eq!(r["status"], "final");
        assert_eq!(r["result"][0]["reference"], "Observation/lco-r1-000001");
        assert_eq!(r["presentedForm"][0]["url"], "Binary/lcb-r1");
        let doc = document_reference(&person(), &d, 10);
        assert_eq!(doc["context"]["related"][0]["reference"], "DiagnosticReport/lcr-r1");
        assert_eq!(doc["date"], "2026-03-05T00:00:00Z");
        let all = report_resources(&person(), &d, Some(b"%PDF-1.4 dummy"), "2026-03-06T12:00:00Z").unwrap();
        for r in &all {
            let v = r.value().unwrap();
            assert_eq!(v["meta"]["lastUpdated"], "2026-03-06T12:00:00Z", "{}", r.resource_type);
            assert_eq!(v["meta"]["source"], META_SOURCE);
        }
        let kinds: Vec<&str> = all.iter().map(|r| r.resource_type.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "Observation",
                "Observation",
                "Observation",
                "DiagnosticReport",
                "Binary",
                "DocumentReference"
            ]
        );
        let panel = all[2].value().unwrap();
        assert_eq!(
            panel["hasMember"][1]["reference"],
            "Observation/lco-r1-000001-000002-i0n1"
        );
        let p = patient(&person());
        assert_eq!(p["gender"], "female");
        assert_eq!(p["name"][0]["given"][0], "Alex");
    }
}
