//! The commands that read Labcorp Patient as a profile: `patients`,
//! `results`, `documents`, `records export`, `api`.

use std::io::Write;
use std::path::{Path, PathBuf};

use pk_cli_config::profile::ProfileName;
use pk_cli_core::private_file::{self, Replace};
use pk_cli_core::{output, CliError, Paged, Reason};
use pk_cli_documents::{fs_safe, verify_pdf, Document, DownloadBatch, SavedDocument, SkippedDocument};
use pk_med_records::dto::{kind, method, relationship};
use pk_med_records::{
    create_export_file, patient_list, Finished, Header as StreamHeader, Organization, PatientContext, RawResource,
    RecordsExport, RecordsWriter, Skip, Source, Trailer,
};
use serde_json::{json, Value};

use crate::app::Ctx;
use crate::cli::*;
use crate::client::{Client, Endpoints};
use crate::fhir;
use crate::portal::{self, Detail, Header, Kind, Person};
use crate::session::SessionStore;

/// A skip's code for a result Labcorp has not reported any value for yet.
pub const NOT_READY: &str = "not_ready";
/// A report PDF that failed `verify_pdf`.
pub const VERIFY_FAILED: Reason = Reason::Provider("verify_failed");

/// The command's own arguments, checked before the vault or the site is
/// touched.
pub fn check_args(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Records(RecordsCmd::Export(args)) => {
            if !args.ndjson && args.output.is_none() {
                return Err(CliError::Usage(
                    "say where the stream goes: --ndjson for stdout (`| medrec import --from labcorp`), or -o <DIR>"
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
    let mut site = Site { client };
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
        Command::Documents(DocumentsCmd::Download(args)) => documents_download(ctx, &mut site, args, &mut out),
        Command::Records(RecordsCmd::Export(args)) => {
            let code = export(ctx, &mut site, args, &mut out)?;
            if code != 0 {
                let _ = out.flush();
                std::process::exit(code);
            }
            Ok(())
        }
        _ => unreachable!("app::run sends only the commands that read as a profile here"),
    }
}

// ------------------------------------------------------------------ site

/// The reads labcorp makes of the portal API (docs/api.md, "Reads").
pub struct Site<'k> {
    pub client: Client<'k>,
}

impl Site<'_> {
    /// Everyone the account reads. Linked accounts the API does not serve
    /// (404) are none.
    pub fn household(&mut self) -> Result<Vec<Person>, CliError> {
        let account = self.client.get_json("/protected/patients/current")?;
        let linked = match self.client.get_json("/protected/patients/current/linkedAccounts") {
            Ok(v) => v,
            Err(e) if e.exit_code() == 4 => Value::Array(Vec::new()),
            Err(e) => return Err(e),
        };
        portal::household(&account, &linked)
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
        label: None,
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

/// The person `--patient` names: a `patients list` id, Labcorp's own id, or
/// a first name only one person has. Without it, the only person, or with
/// `all`, nobody in particular (`None`).
fn pick<'a>(people: &'a [Person], who: &PatientArg, all: bool) -> Result<Option<&'a Person>, CliError> {
    let Some(q) = who.patient.as_deref().map(str::trim) else {
        return match people {
            [one] => Ok(Some(one)),
            _ if all => Ok(None),
            _ => Err(CliError::Usage(format!(
                "this login shows {} people; name one with --patient <ID> (`{BIN} patients list`): {}",
                people.len(),
                ids(people)
            ))),
        };
    };
    if let Some(p) = people.iter().find(|p| fhir::patient_id(p) == q || p.id == q) {
        return Ok(Some(p));
    }
    let named: Vec<&Person> = people
        .iter()
        .filter(|p| p.first_name.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(q)))
        .collect();
    match named.as_slice() {
        [one] => Ok(Some(one)),
        [] => Err(CliError::NotFound(format!("no person `{q}` on this login"))
            .with_remedy(format!("`{BIN} patients list` shows them: {}", ids(people)))),
        _ => Err(CliError::Usage(format!(
            "more than one person is named `{q}`; give the id: {}",
            ids(people)
        ))),
    }
}

/// [`pick`] for a command that reads one person.
fn one<'a>(people: &'a [Person], who: &PatientArg) -> Result<&'a Person, CliError> {
    pick(people, who, false)?.ok_or_else(|| CliError::Usage("name a person with --patient <ID>".into()))
}

fn ids(people: &[Person]) -> String {
    people.iter().map(fhir::patient_id).collect::<Vec<_>>().join(", ")
}

/// Whose result a header is: the person its `pid` names, else the one its
/// patient name names, else the only person.
pub fn owner<'a>(people: &'a [Person], h: &Header) -> Option<&'a Person> {
    if let Some(pid) = &h.patient_id {
        if let Some(p) = people.iter().find(|p| &p.id == pid) {
            return Some(p);
        }
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
                "relationship": c.relationship,
                "name": c.name.clone().unwrap_or_default(),
            })
        })
        .collect();
    output::table(&output::table_view(&rows, &["id", "relationship", "name"]));
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

/// The person whose result `h` is: `--patient`'s, else its owner.
fn reader<'a>(people: &'a [Person], who: &PatientArg, h: &Header) -> Result<&'a Person, CliError> {
    match who.patient {
        Some(_) => one(people, who),
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

fn documents_download(ctx: &Ctx, site: &mut Site, args: &DownloadArgs, out: &mut dyn Write) -> Result<(), CliError> {
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
            .map_err(|e| CliError::Other(format!("writing stdout: {e}"))),
        Some(path) => {
            write_private(&path, &bytes, args.overwrite)?;
            let saved = SavedDocument::from_document(&doc, path.display().to_string(), bytes.len() as u64);
            if ctx.json() {
                output::json(&serde_json::to_value(&saved).unwrap_or_default());
            } else {
                ctx.note(&format!("saved {}", saved.path));
            }
            Ok(())
        }
    }
}

/// `documents download --all`: every report PDF of the people `--patient`
/// selects into one folder. A file already there is kept (`exists`); a
/// report that cannot be read is skipped with its code; a refused session
/// stops the batch.
fn download_all(
    ctx: &Ctx,
    site: &mut Site,
    people: &[Person],
    headers: Vec<Header>,
    args: &DownloadArgs,
) -> Result<(), CliError> {
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
    for (h, p) in rows.iter().filter(|(h, _)| h.pdf_available) {
        let doc = document(h);
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
            Err(e) if e.exit_code() == 3 => return Err(e),
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
    Ok(())
}

/// documents/v1's and the stream's skip code for a file that failed.
fn skip_code(e: &CliError) -> &'static str {
    if e.reason() == Some(VERIFY_FAILED) {
        return pk_cli_documents::SKIP_VERIFY_FAILED;
    }
    match e.exit_code() {
        4 => pk_cli_documents::SKIP_NO_FILE,
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
/// or collected on or after it) with its values and, with `pdfs`, its
/// report PDF. A result with nothing reported yet is skipped
/// ([`NOT_READY`]); one whose detail or PDF fails is skipped with the
/// failure's code. A refused session ends the stream incomplete, its error
/// in the trailer.
pub fn stream(
    site: &mut Site,
    people: &[Person],
    p: &Person,
    headers: Vec<Header>,
    since: Option<&str>,
    pdfs: bool,
    out: &mut dyn Write,
) -> Result<Finished, CliError> {
    let mut selected: Vec<Header> = headers
        .into_iter()
        .filter(|h| owner(people, h).is_some_and(|o| o.id == p.id))
        .filter(|h| since.is_none_or(|s| h.changed_day().is_some_and(|d| d.as_str() >= s)))
        .collect();
    selected.sort_by(|a, b| a.service_day().cmp(&b.service_day()).then_with(|| a.id.cmp(&b.id)));
    let mut w = RecordsWriter::start(out, &stream_header(p, since))?;
    let written = (|| -> Result<(), CliError> {
        w.resource(&RawResource::from_value(&fhir::organization())?)?;
        w.resource(&RawResource::from_value(&fhir::patient(p))?)?;
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
                Err(e) if e.exit_code() == 3 => return Err(e),
                Err(e) => {
                    w.skip(Skip::from_error("DiagnosticReport", &rid, &e));
                    continue;
                }
            };
            let pdf = if pdfs && h.pdf_available {
                match site.pdf(p, h) {
                    Ok(bytes) => Some(bytes),
                    Err(e) if e.exit_code() == 3 => return Err(e),
                    Err(e) => {
                        w.skip(Skip::resource("Binary", fhir::binary_id(h), skip_code(&e), e.message()));
                        None
                    }
                }
            } else {
                None
            };
            for r in fhir::report_resources(p, &d, pdf.as_deref())? {
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
        }
    }

    fn who(q: Option<&str>) -> PatientArg {
        PatientArg {
            patient: q.map(str::to_string),
        }
    }

    #[test]
    fn a_person_is_named_by_id_or_first_name() {
        let people = [person("1", "Alex", Kind::Account), person("2", "Sam", Kind::Linked)];
        assert_eq!(one(&people, &who(Some("lcp-2"))).unwrap().id, "2");
        assert_eq!(one(&people, &who(Some("1"))).unwrap().id, "1");
        assert_eq!(one(&people, &who(Some("sam"))).unwrap().id, "2");
        assert_eq!(one(&people, &who(Some("Jordan"))).err().unwrap().exit_code(), 4);
        assert_eq!(one(&people, &who(None)).err().unwrap().exit_code(), 2);
        assert!(pick(&people, &who(None), true).unwrap().is_none());
        assert_eq!(one(&people[..1], &who(None)).unwrap().id, "1");
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
