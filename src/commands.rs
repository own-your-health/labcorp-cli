//! The commands that read Labcorp Patient as a profile: `patients`,
//! `results`, `documents`, `records export`, `api`.

use std::io::Write;
use std::path::{Path, PathBuf};

use pk_cli_config::profile::ProfileName;
use pk_cli_core::private_file::{self, Replace};
use pk_cli_core::{output, CliError, Paged, Reason};
use pk_cli_documents::{fs_safe, verify_pdf, Document, DownloadBatch, SavedDocument, SkippedDocument};
use pk_med_records::dto::{kind, method, relationship};
use pk_med_records::stream::skip;
use pk_med_records::{
    create_export_file, patient_list, Finished, Header as StreamHeader, Organization, PatientContext, RawResource,
    RecordsExport, RecordsWriter, Skip, Source, Trailer,
};
use serde_json::{json, Value};

use crate::app::Ctx;
use crate::cli::*;
use crate::client::{Client, Endpoints, RATE_LIMITED, RENEWAL_FAILED, UNREACHABLE};
use crate::fhir;
use crate::portal::{self, Detail, Header, Kind, Person};
use crate::session::SessionStore;

/// A skip's code for a result Labcorp has not reported any value for yet.
pub const NOT_READY: &str = "not_ready";
/// A report PDF that failed `verify_pdf`.
pub const VERIFY_FAILED: Reason = Reason::Provider("verify_failed");
/// A `documents download --all` skip's code for a report left unread
/// because the batch stopped.
pub const NOT_READ: &str = "not_read";
/// piekstra-cli/1's exit code for a 404.
const EXIT_NOT_FOUND: i32 = 4;

/// Whether a failed read stops a run of reads (an export, `download --all`)
/// instead of skipping its one result: the session was refused, its
/// renewal failed, Labcorp asked to wait, or it did not answer. Each would
/// fail every later read too. A 403 (`Forbidden`) is about one result and
/// is skipped.
pub fn ends_run(e: &CliError) -> bool {
    matches!(
        e.reason(),
        Some(Reason::CredentialRefused | Reason::NoCredential | Reason::ClientBlocked)
    ) || [RATE_LIMITED, UNREACHABLE, RENEWAL_FAILED]
        .iter()
        .any(|r| e.reason() == Some(*r))
}

/// The command's own arguments, checked before the vault or the site is
/// touched.
pub fn check_args(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Records(RecordsCmd::Export(args)) => {
            if !args.ndjson && args.output.is_none() {
                return Err(CliError::Usage(
                    "say where the stream goes: --ndjson for stdout (what `medrec import --from labcorp` reads), or -o <DIR>"
                        .into(),
                ));
            }
            if args.ndjson && cli.common.json {
                return Err(CliError::Usage(
                    "--ndjson writes the stream, which is the output; drop --json".into(),
                ));
            }
            if let Some(d) = &args.since {
                pk_cli_core::dates::parse_iso(d)?;
            }
        }
        Command::Results(ResultsCmd::List(args)) | Command::Documents(DocumentsCmd::List(args)) => {
            for d in [&args.since, &args.until].into_iter().flatten() {
                pk_cli_core::dates::parse_iso(d)?;
            }
        }
        Command::Documents(DocumentsCmd::Download(args)) => {
            if args.all && args.output.as_deref().is_some_and(|p| p.as_os_str() == "-") {
                return Err(CliError::Usage("--all writes a folder, not stdout".into()));
            }
        }
        Command::Api(args) => {
            if !args.method.eq_ignore_ascii_case("GET") {
                return Err(CliError::Usage(format!(
                    "`{BIN} api` is read-only: it sends GET only, not {}",
                    args.method
                )));
            }
            if !args.path.starts_with('/') && !args.path.starts_with(&format!("{}/", crate::site::API_BASE)) {
                return Err(CliError::Usage(format!(
                    "a path on the portal API starts with `/`, or is a URL under {}",
                    crate::site::API_BASE
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn run(ctx: &Ctx, profile: &ProfileName) -> Result<(), CliError> {
    let store = SessionStore::open()?;
    let session = store.load(profile)?;
    let client = Client::new(profile, session, &store, Endpoints::default())?;
    let mut site = Site::new(client, ctx.quiet()).with_labels(
        pk_cli_config::ConfigStore::new(BIN).with_override(ctx.cli.config.clone()),
        profile.clone(),
    );
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match &ctx.cli.command {
        Command::Api(args) => {
            let v = site.client.get_json(&args.path)?;
            output::json(&v);
            Ok(())
        }
        Command::Patients(PatientsCmd::List) => patients(ctx, &mut site),
        Command::Results(ResultsCmd::List(args)) => results_list(ctx, &mut site, args),
        Command::Results(ResultsCmd::Get { who, id }) => results_get(ctx, &mut site, who, id),
        Command::Documents(DocumentsCmd::List(args)) => documents_list(ctx, &mut site, args),
        Command::Documents(DocumentsCmd::Download(args)) => {
            let code = documents_download(ctx, &mut site, args, &mut out)?;
            if code != 0 {
                let _ = out.flush();
                std::process::exit(code);
            }
            Ok(())
        }
        Command::Records(RecordsCmd::Export(args)) => {
            let code = export(ctx, &mut site, args, &mut out)?;
            if code != 0 {
                let _ = out.flush();
                std::process::exit(code);
            }
            Ok(())
        }
        Command::Patients(PatientsCmd::Label(_)) => unreachable!("app::run runs `patients label` itself"),
        _ => unreachable!("app::run sends only the commands that read as a profile here"),
    }
}

// ------------------------------------------------------------------ site

/// The reads labcorp makes of the portal API (docs/api.md, "Reads").
pub struct Site<'k> {
    pub client: Client<'k>,
    /// What the reads left out that the person should know, also printed
    /// to stderr unless `quiet`.
    pub warnings: Vec<String>,
    quiet: bool,
    /// Where the profile's labels are kept, when the reads label the people
    /// they list ([`crate::labels::record`]).
    labels: Option<(pk_cli_config::ConfigStore, ProfileName)>,
}

impl<'k> Site<'k> {
    pub fn new(client: Client<'k>, quiet: bool) -> Site<'k> {
        Site {
            client,
            warnings: Vec::new(),
            quiet,
            labels: None,
        }
    }

    /// Give the people each read lists the labels `profile` holds for them,
    /// and keep the people listed in its settings.
    pub fn with_labels(mut self, store: pk_cli_config::ConfigStore, profile: ProfileName) -> Site<'k> {
        self.labels = Some((store, profile));
        self
    }

    fn warn(&mut self, msg: String) {
        if !self.quiet {
            eprintln!("warning: {msg}");
        }
        self.warnings.push(msg);
    }

    /// Everyone the account reads. When the API answers 404 for the linked
    /// accounts, the account and its dependents are read without them, with
    /// a warning.
    pub fn household(&mut self) -> Result<Vec<Person>, CliError> {
        let account = self.client.get_json("/protected/patients/current")?;
        let linked = match self.client.get_json("/protected/patients/current/linkedAccounts") {
            Ok(v) => v,
            Err(e) if e.exit_code() == EXIT_NOT_FOUND => {
                self.warn(
                    "Labcorp answered 404 for the accounts shared with this login; \
                     reading the account and its dependents only"
                        .into(),
                );
                Value::Array(Vec::new())
            }
            Err(e) => return Err(e),
        };
        let mut people = portal::household(&account, &linked)?;
        if let Some((store, profile)) = &self.labels {
            crate::labels::record(store, profile, &mut people)?;
        }
        Ok(people)
    }

    /// Every result's header, for everyone the account reads.
    pub fn headers(&mut self) -> Result<Vec<Header>, CliError> {
        let v = self
            .client
            .get_json("/protected/patients/current/linkedAccounts/results/headers/all")?;
        portal::headers(&v)
    }

    /// One result with its values. The list's header fills what the
    /// detail's leaves out.
    pub fn detail(&mut self, p: &Person, h: &Header) -> Result<Detail, CliError> {
        let path = format!("/protected/patients/current{}/results/{}", p.scope(), segment(&h.id)?);
        let mut d = Detail::read(&self.client.get_json(&path)?)?;
        d.header = fill(d.header, h);
        Ok(d)
    }

    /// One result's report PDF, checked to be one ([`VERIFY_FAILED`]
    /// otherwise).
    pub fn pdf(&mut self, p: &Person, h: &Header) -> Result<Vec<u8>, CliError> {
        let path = format!(
            "/protected/patients/current{}/results/{}/pdf",
            p.scope(),
            segment(&h.id)?
        );
        let a = self.client.get_file(&path, "application/pdf")?;
        verify_pdf(&a.body).map_err(|e| {
            CliError::Upstream(format!("the report PDF failed its check: {}", e.message())).because(VERIFY_FAILED)
        })?;
        Ok(a.body)
    }
}

/// An id from an answer, as one path segment: refused when it holds
/// anything but letters, digits, `-`, `_` or `.`.
fn segment(id: &str) -> Result<&str, CliError> {
    let ok = !id.is_empty()
        && id != "."
        && id != ".."
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if ok {
        Ok(id)
    } else {
        Err(CliError::Upstream(
            "a result's id is not one this labcorp reads into a path".into(),
        ))
    }
}

/// `detail`'s header with each field it lacks taken from `list`'s.
fn fill(detail: Header, list: &Header) -> Header {
    let or = |a: Option<String>, b: &Option<String>| a.or_else(|| b.clone());
    Header {
        id: detail.id,
        patient_id: or(detail.patient_id, &list.patient_id),
        patient_name: or(detail.patient_name, &list.patient_name),
        date_of_service: or(detail.date_of_service, &list.date_of_service),
        report_date: or(detail.report_date, &list.report_date),
        ordered_date: or(detail.ordered_date, &list.ordered_date),
        ordering_provider: or(detail.ordering_provider, &list.ordering_provider),
        account_name: or(detail.account_name, &list.account_name),
        detail_available: list.detail_available,
        pdf_available: list.pdf_available,
        category: or(detail.category, &list.category),
        test_codes: if detail.test_codes.is_empty() {
            list.test_codes.clone()
        } else {
            detail.test_codes
        },
        total_tests: detail.total_tests.or(list.total_tests),
        completed_tests: detail.completed_tests.or(list.completed_tests),
        statuses: if detail.statuses.is_empty() {
            list.statuses.clone()
        } else {
            detail.statuses
        },
    }
}

// --------------------------------------------------------------- people

pub fn organization() -> Organization {
    Organization {
        id: fhir::ORGANIZATION_ID.into(),
        name: fhir::ORGANIZATION_NAME.into(),
        fhir_base: format!("https://{}", crate::site::HOST),
        vendor: Some("labcorp".into()),
        location: None,
        aliases: vec!["LabCorp".into(), "Laboratory Corporation of America".into()],
    }
}

/// A person as health-records/v1's patient context: the account holder's
/// own record, or another's read by proxy.
pub fn context(p: &Person, with_name: bool) -> PatientContext {
    PatientContext {
        id: fhir::patient_id(p),
        organization: fhir::ORGANIZATION_ID.into(),
        label: p.label.clone(),
        relationship: Some(
            match p.kind {
                Kind::Account => relationship::SELF,
                Kind::Dependent | Kind::Linked => relationship::PROXY,
            }
            .into(),
        ),
        kind: Some(kind::PATIENT.into()),
        member_role: None,
        fhir_user: None,
        name: if with_name { p.name() } else { None },
        session: None,
    }
}

/// The person `--patient` names: their id from `patients list`, or their
/// label, case aside (medical-common DESIGN.md §1). Never a name Labcorp
/// shows, nor Labcorp's own id: either would stay in shell history and in
/// an agent's transcript. Without it, the only person, or with `all`,
/// nobody in particular (`None`). An error names nobody by id and does not
/// repeat the `--patient` value, which is handled as the id it may be
/// (§1.3).
fn pick<'a>(people: &'a [Person], who: &PatientArg, all: bool) -> Result<Option<&'a Person>, CliError> {
    let Some(q) = who.patient.as_deref().map(str::trim) else {
        return match people {
            [one] => Ok(Some(one)),
            _ if all => Ok(None),
            _ => Err(CliError::Usage(format!(
                "this login shows {} people; name one with --patient <ID or LABEL>",
                people.len()
            ))
            .with_remedy(format!("`{BIN} patients list` lists them"))),
        };
    };
    people
        .iter()
        .find(|p| {
            crate::labels::is_the_id(q, &fhir::patient_id(p)) || crate::labels::is_the_label(q, p.label.as_deref())
        })
        .map(Some)
        .ok_or_else(|| {
            CliError::NotFound("--patient names no one this login shows".into()).with_remedy(format!(
                "`{BIN} patients list` lists them; give an id or a label it prints"
            ))
        })
}

/// [`pick`] for a command that reads one person.
fn one<'a>(people: &'a [Person], who: &PatientArg) -> Result<&'a Person, CliError> {
    pick(people, who, false)?.ok_or_else(|| CliError::Usage("name a person with --patient <ID or LABEL>".into()))
}

/// A person as an error message may name them: by label, else not at all.
fn named(p: &Person) -> String {
    match &p.label {
        Some(l) => format!("`{l}`"),
        None => "another person on this login".into(),
    }
}

/// Whose result a header is: the person its `pid` names. A header with a
/// `pid` that names nobody this login shows has no owner, so it is never
/// filed under someone else. Only a header with no `pid` falls back to the
/// one person its patient name names, else the only person.
pub fn owner<'a>(people: &'a [Person], h: &Header) -> Option<&'a Person> {
    if let Some(pid) = &h.patient_id {
        return people.iter().find(|p| &p.id == pid);
    }
    if let Some(name) = &h.patient_name {
        let hits: Vec<&Person> = people
            .iter()
            .filter(|p| p.name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .collect();
        if let [one] = hits.as_slice() {
            return Some(one);
        }
    }
    match people {
        [one] => Some(one),
        _ => None,
    }
}

fn patients(ctx: &Ctx, site: &mut Site) -> Result<(), CliError> {
    let people = site.household()?;
    let items: Vec<PatientContext> = people.iter().map(|p| context(p, true)).collect();
    if ctx.json() {
        patient_list(items).emit(true);
        return Ok(());
    }
    let rows: Vec<Value> = items
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "label": c.label.clone().unwrap_or_default(),
                "relationship": c.relationship,
                "name": c.name.clone().unwrap_or_default(),
            })
        })
        .collect();
    output::table(&output::table_view(&rows, &["id", "label", "relationship", "name"]));
    Ok(())
}

// -------------------------------------------------------------- results

/// The results `args` selects: with `--patient` (or one person) that
/// person's, else everyone's; collected within `--since` and `--until`;
/// newest collection first.
fn selected<'a>(
    people: &'a [Person],
    headers: Vec<Header>,
    args: &ResultsListArgs,
) -> Result<Vec<(Header, Option<&'a Person>)>, CliError> {
    let who = pick(people, &args.who, true)?;
    let since = args.since.as_deref().map(iso).transpose()?;
    let until = args.until.as_deref().map(iso).transpose()?;
    let mut rows: Vec<(Header, Option<&Person>)> = headers
        .into_iter()
        .map(|h| {
            let p = owner(people, &h);
            (h, p)
        })
        .filter(|(_, p)| who.is_none_or(|w| p.is_some_and(|p| p.id == w.id)))
        .filter(|(h, _)| {
            let d = h.service_day();
            since.as_ref().is_none_or(|s| d.as_ref().is_some_and(|d| d >= s))
                && until.as_ref().is_none_or(|u| d.as_ref().is_some_and(|d| d <= u))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0.service_day()));
    Ok(rows)
}

fn iso(d: &str) -> Result<String, CliError> {
    Ok(pk_cli_core::dates::fmt_iso(pk_cli_core::dates::parse_iso(d.trim())?))
}

fn result_row(h: &Header, p: Option<&Person>) -> Value {
    json!({
        "id": h.id,
        "patient": p.map(fhir::patient_id),
        "date_of_service": h.service_day(),
        "report_date": h.report_day(),
        "status": h.status(),
        "tests": h.total_tests.unwrap_or(h.test_codes.len() as u64),
        "ordering_provider": h.ordering_provider,
        "account": h.account_name,
        "pdf": h.pdf_available,
    })
}

fn results_list(ctx: &Ctx, site: &mut Site, args: &ResultsListArgs) -> Result<(), CliError> {
    let people = site.household()?;
    let rows: Vec<Value> = selected(&people, site.headers()?, args)?
        .iter()
        .map(|(h, p)| result_row(h, *p))
        .collect();
    if ctx.json() {
        Paged::new("result", rows).emit(true);
        return Ok(());
    }
    if rows.is_empty() {
        ctx.note("no results");
        return Ok(());
    }
    output::table(&output::table_view(
        &rows,
        &[
            "date_of_service",
            "status",
            "tests",
            "ordering_provider",
            "patient",
            "id",
        ],
    ));
    Ok(())
}

fn find_header<'a>(headers: &'a [Header], id: &str) -> Result<&'a Header, CliError> {
    let id = id.trim();
    headers
        .iter()
        .find(|h| h.id == id || fhir::report_id(h) == id || fhir::document_id(h) == id)
        .ok_or_else(|| {
            CliError::NotFound(format!("no result {id} on this login"))
                .with_remedy(format!("`{BIN} results list` shows them"))
        })
}

/// The person whose result `h` is: its owner, or `--patient`'s when it has
/// none. A `--patient` naming someone else than its owner is refused.
fn reader<'a>(people: &'a [Person], who: &PatientArg, h: &Header) -> Result<&'a Person, CliError> {
    match who.patient {
        Some(_) => {
            let p = one(people, who)?;
            match owner(people, h) {
                Some(o) if o.id != p.id => Err(CliError::Usage(format!(
                    "result {} is not the result of the person --patient names: it belongs to {}",
                    h.id,
                    named(o)
                ))
                .with_remedy("leave out --patient: a result is read as the person it belongs to")),
                _ => Ok(p),
            }
        }
        None => owner(people, h).ok_or_else(|| {
            CliError::Usage(format!(
                "result {} names no person this login shows; give --patient <ID> (`{BIN} patients list`)",
                h.id
            ))
        }),
    }
}

fn results_get(ctx: &Ctx, site: &mut Site, who: &PatientArg, id: &str) -> Result<(), CliError> {
    let people = site.household()?;
    let headers = site.headers()?;
    let h = find_header(&headers, id)?;
    let p = reader(&people, who, h)?;
    let d = site.detail(p, h)?;
    let mut v = result_row(&d.header, Some(p));
    v["schema"] = "result/v1".into();
    v["items"] = d
        .items
        .iter()
        .map(|i| {
            json!({
                "test": i.test_name,
                "code": i.test_code,
                "status": i.status,
                "summary": i.summary,
                "values": i.lines.iter().map(|l| json!({
                    "name": l.name,
                    "code": l.number,
                    "value": l.value,
                    "units": l.units,
                    "flag": l.abnormal,
                    "range": l.range,
                    "status": l.status,
                    "comments": l.comments,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    if ctx.json() {
        output::json(&v);
    } else {
        output::render(&v);
    }
    Ok(())
}

// ------------------------------------------------------------ documents

/// A result's report PDF as documents/v1 lists it.
pub fn document(h: &Header) -> Document {
    let mut name = format!(
        "Labcorp report, collected {}",
        h.service_day().unwrap_or_else(|| "on an unknown day".into())
    );
    if let Some(by) = &h.ordering_provider {
        name.push_str(&format!(", ordered by {by}"));
    }
    let mut d = Document::new(fhir::document_id(h), name);
    d.date = h.report_day().or_else(|| h.service_day());
    d.category = Some("lab-report".into());
    d.file = Some(file_name(h));
    d
}

/// `<day>_labcorp-report_<result id>.pdf`: the same name each run, so a
/// re-run finds what it saved.
pub fn file_name(h: &Header) -> String {
    let day = h.service_day().unwrap_or_else(|| "undated".into());
    format!("{}.pdf", fs_safe(&format!("{day}_labcorp-report_{}", h.id)))
}

fn documents_list(ctx: &Ctx, site: &mut Site, args: &ResultsListArgs) -> Result<(), CliError> {
    let people = site.household()?;
    let rows: Vec<Value> = selected(&people, site.headers()?, args)?
        .iter()
        .filter(|(h, _)| h.pdf_available)
        .map(|(h, _)| serde_json::to_value(document(h)).unwrap_or_default())
        .collect();
    if ctx.json() {
        Paged::new("document", rows).emit(true);
        return Ok(());
    }
    if rows.is_empty() {
        ctx.note("no report PDFs");
        return Ok(());
    }
    output::table(&output::table_view(&rows, &["date", "name", "id"]));
    Ok(())
}

fn documents_download(ctx: &Ctx, site: &mut Site, args: &DownloadArgs, out: &mut dyn Write) -> Result<i32, CliError> {
    let people = site.household()?;
    let headers = site.headers()?;
    if args.all {
        return download_all(ctx, site, &people, headers, args);
    }
    let id = args.id.as_deref().expect("clap requires an id without --all");
    let h = find_header(&headers, id)?;
    let p = reader(&people, &args.who, h)?;
    let doc = document(h);
    let target = match args.output.as_deref() {
        Some(p) if p.as_os_str() == "-" => None,
        Some(p) if p.is_dir() => Some(p.join(file_name(h))),
        Some(p) => Some(p.to_path_buf()),
        None => Some(PathBuf::from(file_name(h))),
    };
    if let Some(path) = &target {
        refuse_existing(path, args.overwrite)?;
    }
    let bytes = site.pdf(p, h)?;
    match target {
        None => out
            .write_all(&bytes)
            .map(|()| 0)
            .map_err(|e| CliError::Other(format!("writing stdout: {e}"))),
        Some(path) => {
            write_private(&path, &bytes, args.overwrite)?;
            let saved = SavedDocument::from_document(&doc, path.display().to_string(), bytes.len() as u64);
            if ctx.json() {
                output::json(&serde_json::to_value(&saved).unwrap_or_default());
            } else {
                ctx.note(&format!("saved {}", saved.path));
            }
            Ok(0)
        }
    }
}

/// `documents download --all`: every report PDF of the people `--patient`
/// selects into one folder. A file already there is kept (`exists`); a
/// report that cannot be read is skipped with its code. A failure that
/// would fail every later read too ([`ends_run`]) stops the batch: the
/// reports not read are skipped as [`NOT_READ`], the batch is printed, and
/// the exit code is the failure's.
fn download_all(
    ctx: &Ctx,
    site: &mut Site,
    people: &[Person],
    headers: Vec<Header>,
    args: &DownloadArgs,
) -> Result<i32, CliError> {
    let list = ResultsListArgs {
        who: args.who.clone(),
        since: None,
        until: None,
    };
    let rows = selected(people, headers, &list)?;
    let dir = args.output.clone().unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
    let mut saved = Vec::new();
    let mut skipped = Vec::new();
    let mut stopped: Option<CliError> = None;
    for (h, p) in rows.iter().filter(|(h, _)| h.pdf_available) {
        let doc = document(h);
        if let Some(e) = &stopped {
            let why = format!("not read: the batch stopped ({})", e.message());
            skipped.push(SkippedDocument::new(doc.id, why).with_code(NOT_READ));
            continue;
        }
        let Some(p) = p else {
            skipped.push(SkippedDocument::new(doc.id, "names no person this login shows").with_code("no_owner"));
            continue;
        };
        let path = dir.join(file_name(h));
        if std::fs::symlink_metadata(&path).is_ok() && !args.overwrite {
            skipped.push(SkippedDocument::new(doc.id, "already saved; --overwrite replaces it").with_code("exists"));
            continue;
        }
        match site.pdf(p, h) {
            Ok(bytes) => {
                write_private(&path, &bytes, args.overwrite)?;
                saved.push(SavedDocument::from_document(
                    &doc,
                    path.display().to_string(),
                    bytes.len() as u64,
                ));
            }
            Err(e) if ends_run(&e) => {
                skipped.push(SkippedDocument::new(doc.id, e.message()).with_code(NOT_READ));
                stopped = Some(e);
            }
            Err(e) => skipped.push(SkippedDocument::new(doc.id, e.message()).with_code(skip_code(&e))),
        }
    }
    let kept = skipped.iter().filter(|s| s.code.as_deref() == Some("exists")).count();
    let failed = skipped.len() - kept;
    let batch = DownloadBatch::new(dir.display().to_string(), saved).with_skipped(skipped);
    if ctx.json() {
        output::json(&serde_json::to_value(&batch).unwrap_or_default());
    } else {
        let mut note = format!("saved {} report(s) to {}", batch.count, batch.dir);
        if kept > 0 {
            note.push_str(&format!("; {kept} already there"));
        }
        if failed > 0 {
            note.push_str(&format!("; {failed} could not be read (--json lists them)"));
        }
        ctx.note(&note);
    }
    Ok(match stopped {
        Some(e) => {
            ctx.note(&format!("stopped: {e}"));
            e.exit_code()
        }
        None => 0,
    })
}

/// documents/v1's and the stream's skip code for a file that failed.
fn skip_code(e: &CliError) -> &'static str {
    match e.reason() {
        Some(r) if r == VERIFY_FAILED => pk_cli_documents::SKIP_VERIFY_FAILED,
        Some(Reason::Forbidden) => skip::FORBIDDEN,
        _ if e.exit_code() == EXIT_NOT_FOUND => pk_cli_documents::SKIP_NO_FILE,
        _ => pk_cli_documents::SKIP_UPSTREAM,
    }
}

fn refuse_existing(path: &Path, overwrite: bool) -> Result<(), CliError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Err(CliError::Usage(format!("{} is a folder", path.display()))),
        Ok(_) if !overwrite => Err(CliError::Usage(format!(
            "{} exists; pass --overwrite to replace it",
            path.display()
        ))),
        _ => Ok(()),
    }
}

/// An owner-only (0600) file, never written through a symlink.
fn write_private(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), CliError> {
    let replace = if overwrite { Replace::Always } else { Replace::Never };
    private_file::write(path, bytes, replace).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CliError::Usage(format!("{} exists; pass --overwrite to replace it", path.display()))
        } else {
            CliError::Other(format!("writing {}: {e}", path.display()))
        }
    })
}

// -------------------------------------------------------------- records

/// The stream header for `p`.
pub fn stream_header(p: &Person, since: Option<&str>) -> StreamHeader {
    let mut h = StreamHeader::new(
        Source::new(BIN, env!("CARGO_PKG_VERSION")),
        organization(),
        context(p, false),
        crate::time::now_rfc3339(),
    );
    h.method = Some(method::PORTAL.into());
    if let Some(s) = since {
        h.since = Some(s.into());
        // Exported whatever the date: the person and the lab.
        h.since_unbounded = vec!["Organization".into(), "Patient".into()];
    }
    h
}

/// The exit code of a stream that ended incomplete: its error's.
fn incomplete_exit(trailer: &Trailer) -> i32 {
    trailer.error.as_ref().and_then(|e| e.exit_code).unwrap_or(1)
}

/// `records export`: to stdout with `--ndjson`, else to `-o DIR`.
fn export(ctx: &Ctx, site: &mut Site, args: &ExportArgs, out: &mut dyn Write) -> Result<i32, CliError> {
    let since = args.since.as_deref().map(iso).transpose()?;
    let people = site.household()?;
    let p = one(&people, &args.who)?.clone();
    let headers = site.headers()?;
    let finished = if args.ndjson {
        stream(site, &people, &p, headers, since.as_deref(), !args.no_pdfs, out)?
    } else {
        let dir = args.output.as_deref().expect("check_args requires --ndjson or -o");
        let mut file = create_export_file(dir)?;
        let finished = stream(site, &people, &p, headers, since.as_deref(), !args.no_pdfs, file.file())?;
        let path = file.commit(&finished)?;
        let report = RecordsExport::new(path.display().to_string(), &finished);
        if ctx.json() {
            output::json(&serde_json::to_value(&report).unwrap_or_default());
        } else {
            ctx.note(&format!("wrote {}", path.display()));
        }
        finished
    };
    Ok(if finished.trailer.complete {
        0
    } else {
        incomplete_exit(&finished.trailer)
    })
}

/// One person's results as records-stream/v1, streamed as they are read:
/// the lab and the person, then each result (with `since`, those reported
/// or collected on or after it, and those with no date) with its values
/// and, with `pdfs`, its report PDF. A result with nothing reported yet is
/// skipped ([`NOT_READY`]); one whose detail or PDF fails is skipped with
/// the failure's code (`forbidden` for a 403). A failure that would fail
/// every later read too ([`ends_run`]: a refused session, a request to
/// wait, no answer) ends the stream incomplete, its error in the trailer.
pub fn stream(
    site: &mut Site,
    people: &[Person],
    p: &Person,
    headers: Vec<Header>,
    since: Option<&str>,
    pdfs: bool,
    out: &mut dyn Write,
) -> Result<Finished, CliError> {
    let unowned = headers.iter().filter(|h| owner(people, h).is_none()).count();
    if unowned > 0 {
        site.warn(format!(
            "{unowned} result(s) name a person this login does not show and are left out \
             (`{BIN} results list` lists them)"
        ));
    }
    let mut selected: Vec<Header> = headers
        .into_iter()
        .filter(|h| owner(people, h).is_some_and(|o| o.id == p.id))
        .filter(|h| since.is_none_or(|s| h.changed_day().is_none_or(|d| d.as_str() >= s)))
        .collect();
    selected.sort_by(|a, b| a.service_day().cmp(&b.service_day()).then_with(|| a.id.cmp(&b.id)));
    let header = stream_header(p, since);
    let pulled_at = header.pulled_at.clone();
    let mut w = RecordsWriter::start(out, &header)?;
    let written = (|| -> Result<(), CliError> {
        w.resource(&RawResource::from_value(&fhir::read_at(
            fhir::organization(),
            &pulled_at,
        ))?)?;
        w.resource(&RawResource::from_value(&fhir::read_at(fhir::patient(p), &pulled_at))?)?;
        for h in &selected {
            let rid = fhir::report_id(h);
            if !h.detail_available {
                w.skip(Skip::resource(
                    "DiagnosticReport",
                    &rid,
                    NOT_READY,
                    "Labcorp has reported no value for this result yet; it is exported once it does",
                ));
                continue;
            }
            let d = match site.detail(p, h) {
                Ok(d) => d,
                Err(e) if ends_run(&e) => return Err(e),
                Err(e) => {
                    w.skip(Skip::from_error("DiagnosticReport", &rid, &e));
                    continue;
                }
            };
            let pdf = if pdfs && h.pdf_available {
                match site.pdf(p, h) {
                    Ok(bytes) => Some(bytes),
                    Err(e) if ends_run(&e) => return Err(e),
                    Err(e) => {
                        w.skip(Skip::resource("Binary", fhir::binary_id(h), skip_code(&e), e.message()));
                        None
                    }
                }
            } else {
                None
            };
            let read = crate::time::now_rfc3339();
            for r in fhir::report_resources(p, &d, pdf.as_deref(), &read)? {
                w.resource(&r)?;
            }
        }
        Ok(())
    })();
    w.finish(crate::time::now_rfc3339(), written.err().as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, first: &str, kind: Kind) -> Person {
        Person {
            id: id.into(),
            kind,
            linked_id: None,
            first_name: Some(first.into()),
            middle_name: None,
            last_name: Some("Example".into()),
            birth_date: None,
            gender: None,
            label: None,
        }
    }

    fn who(q: Option<&str>) -> PatientArg {
        PatientArg {
            patient: q.map(str::to_string),
        }
    }

    /// Every message and remedy an error carries.
    fn text(e: &CliError) -> String {
        format!("{} {}", e.message(), e.remedy().unwrap_or_default())
    }

    #[test]
    fn a_person_is_named_by_id_or_label_never_by_name() {
        let mut sam = person("2", "Sam", Kind::Linked);
        sam.label = Some("kid".into());
        let people = [person("1", "Alex", Kind::Account), sam];
        let sam_id = fhir::patient_id(&people[1]);
        assert_eq!(one(&people, &who(Some(&sam_id))).unwrap().id, "2");
        assert_eq!(one(&people, &who(Some(&sam_id.to_ascii_uppercase()))).unwrap().id, "2");
        assert_eq!(one(&people, &who(Some("KID"))).unwrap().id, "2");
        // Neither a first name Labcorp shows nor Labcorp's own id.
        for q in ["sam", "Alex", "1", "2"] {
            let e = one(&people, &who(Some(q))).err().unwrap();
            assert_eq!(e.exit_code(), 4, "{q}");
            assert!(e.remedy().unwrap().contains("patients list"), "{q}");
        }
        assert_eq!(one(&people, &who(None)).err().unwrap().exit_code(), 2);
        assert!(pick(&people, &who(None), true).unwrap().is_none());
        assert_eq!(one(&people[..1], &who(None)).unwrap().id, "1");
    }

    #[test]
    fn an_error_names_nobody_by_id_and_repeats_no_patient_value() {
        let people = [person("1", "Alex", Kind::Account), person("2", "Sam", Kind::Linked)];
        let ids: Vec<String> = people.iter().map(fhir::patient_id).collect();
        let missing = one(&people, &who(Some("Jordan-Q"))).err().unwrap();
        let unnamed = one(&people, &who(None)).err().unwrap();
        let h = Header::read(&json!({ "id": "r", "pid": "2" })).unwrap();
        let elses = reader(&people, &who(Some(&ids[0])), &h).err().unwrap();
        for e in [&missing, &unnamed, &elses] {
            let t = text(e);
            for id in ids.iter().map(String::as_str).chain(["Jordan-Q", "Alex", "Sam"]) {
                assert!(!t.contains(id), "{id} in: {t}");
            }
        }
        assert!(text(&unnamed).contains("2 people"), "{}", text(&unnamed));
    }

    #[test]
    fn a_result_belongs_to_the_person_it_names() {
        let people = [person("1", "Alex", Kind::Account), person("2", "Sam", Kind::Linked)];
        let h = |v: Value| Header::read(&v).unwrap();
        assert_eq!(owner(&people, &h(json!({ "id": "r", "pid": "2" }))).unwrap().id, "2");
        assert_eq!(
            owner(&people, &h(json!({ "id": "r", "patientName": "SAM EXAMPLE" })))
                .unwrap()
                .id,
            "2"
        );
        assert!(owner(&people, &h(json!({ "id": "r" }))).is_none());
        assert_eq!(owner(&people[..1], &h(json!({ "id": "r" }))).unwrap().id, "1");
        // A `pid` naming nobody here is nobody's: not the only person's,
        // not the one its name names.
        let stranger = h(json!({ "id": "r", "pid": "9", "patientName": "ALEX EXAMPLE" }));
        assert!(owner(&people[..1], &stranger).is_none());
        assert!(owner(&people, &stranger).is_none());
    }

    #[test]
    fn patient_cannot_claim_someone_elses_result() {
        let people = [person("1", "Alex", Kind::Account), person("2", "Sam", Kind::Linked)];
        let h = |v: Value| Header::read(&v).unwrap();
        let sams = h(json!({ "id": "r", "pid": "2" }));
        let (alex, sam) = (fhir::patient_id(&people[0]), fhir::patient_id(&people[1]));
        assert_eq!(reader(&people, &who(None), &sams).unwrap().id, "2");
        assert_eq!(reader(&people, &who(Some(&sam)), &sams).unwrap().id, "2");
        assert_eq!(reader(&people, &who(Some(&alex)), &sams).err().unwrap().exit_code(), 2);
        // With no owner, --patient says whose it is.
        let nobodys = h(json!({ "id": "r", "pid": "9" }));
        assert_eq!(reader(&people, &who(None), &nobodys).err().unwrap().exit_code(), 2);
        assert_eq!(reader(&people, &who(Some(&alex)), &nobodys).unwrap().id, "1");
        // The owner is named by label when they have one.
        let mut labelled = people.clone();
        labelled[1].label = Some("kid".into());
        let e = reader(&labelled, &who(Some(&alex)), &sams).err().unwrap();
        assert!(e.message().contains("`kid`"), "{}", e.message());
    }

    #[test]
    fn a_run_stops_only_on_what_fails_every_later_read() {
        let refused = CliError::Auth("x".into()).because(Reason::CredentialRefused);
        let forbidden = CliError::Auth("x".into()).because(Reason::Forbidden);
        let wait = CliError::Upstream("x".into()).because(RATE_LIMITED);
        let gone = CliError::Upstream("x".into()).because(UNREACHABLE);
        let unrenewed = CliError::Upstream("x".into()).because(RENEWAL_FAILED);
        assert!(ends_run(&refused) && ends_run(&wait) && ends_run(&gone) && ends_run(&unrenewed));
        assert!(!ends_run(&forbidden));
        assert!(!ends_run(&CliError::Upstream("x".into())));
        assert!(!ends_run(&CliError::NotFound("x".into())));
        assert_eq!(skip_code(&forbidden), "forbidden");
        assert_eq!(skip_code(&CliError::NotFound("x".into())), "no_file");
        assert_eq!(
            skip_code(&CliError::Upstream("x".into()).because(VERIFY_FAILED)),
            "verify_failed"
        );
        assert_eq!(skip_code(&CliError::Upstream("x".into())), "upstream");
    }

    #[test]
    fn an_id_is_a_path_segment_or_refused() {
        assert_eq!(segment("A1-b_2.3").unwrap(), "A1-b_2.3");
        for bad in ["", "..", "a/b", "a?b", "a b", "a%2fb"] {
            assert!(segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_report_file_name_is_stable_and_safe() {
        let h = Header::read(&json!({ "id": "r/1", "dateOfService": "2026-03-04T09:00:00Z" })).unwrap();
        let name = file_name(&h);
        assert!(
            name.starts_with("2026-03-04_labcorp-report_") && name.ends_with(".pdf"),
            "{name}"
        );
        assert!(!name.contains('/'), "{name}");
    }
}
