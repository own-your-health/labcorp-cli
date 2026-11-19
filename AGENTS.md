# AGENTS.md — labcorp-cli

`labcorp` is a read-only CLI for a person's Labcorp lab results and report
PDFs, read from Labcorp Patient (patient.labcorp.com) signed in per profile,
for the household's medical-records system (`medrec import --from labcorp`
reads its `records-stream/v1`). It conforms to piekstra-cli/1 and the
`health-records/v1` and `documents/v1` profiles, on the `pk-cli-*` crates from
[cli-common](https://github.com/piekstra/cli-common) and `pk-med-records` and
`pk-med-fhir` from [medical-common](https://github.com/piekstra/medical-common).
Every `pk-cli-*` stays on the cli-common tag medical-common pins.

## Layout

| File | Owns |
|---|---|
| `src/cli.rs` | the clap tree |
| `src/app.rs` | dispatch, and the commands that need no sign-in: `profiles`, `config`, `info`, `completions`, `self-update` |
| `src/source.rs` | which configured profile a command reads as (`resolve_profile`); `--profile` refused where no login is used |
| `src/auth.rs` | `auth login`, `status`, `logout` of one `--profile`, `auth reset`; the browser-profile seams |
| `src/session.rs` | `SessionStore`: a profile's session in the vault; the only code that knows where it lives |
| `src/site.rs` | the sign-in: what the capture reads from the Chrome window, and the read that confirms it (`SESSION_CHECK_PATH`, `judge`) |
| `src/client.rs` | the portal API reads: bearer, pacing, no redirects, answers to exit codes (`judge`), token renewal from Okta's session (`renew`) |
| `src/commands.rs` | the commands that read as a profile (`check_args` before anything is read) |
| `src/time.rs` | the clock as Unix seconds and RFC 3339 |
| `docs/api.md` | how Labcorp data can be reached, Labcorp's own wording, the session, the reads, the traps |

## Medical data rules

These come from the household's plan and are not negotiable.

- The repository carries no personal data, in code, fixtures, docs, commits,
  pull requests or issues. Fixtures are synthetic; `tests/no_personal_data.rs`
  scans every tracked file for the shapes personal data takes.
- Records and their contents are never sent to an outside service: no
  health-data aggregator, cloud OCR or other company's API. A Claude session
  is not one of these. A session may read real results when the work needs it
  (mapping the site, checking an export), but real data stays out of tracked
  files, commits, pull requests and issues, and out of messages to other
  sessions unless they need it. A raw capture is deleted from `/tmp` once it
  has served.
- No FHIR, SMART or DICOM libraries, no aggregators, no cloud OCR or AI. The
  household's own crates are not third party.
- Read-only. Nothing writes to Labcorp; `api` sends `GET` only. A write would
  need SPEC §1.3's confirmation gate and a change to this paragraph in the
  same commit.

## Profiles and the session

A profile is cli-common's (`pk_cli_config::profile`, DESIGN.md §2.5):
configured in labcorp's config (`profiles add`), resolved by
`profile::resolve` from `--profile` or `LABCORP_PROFILE` (missing, malformed
or unknown is exit 2 listing the configured names; a lone profile is never
picked). A command that uses no login refuses `--profile` given as a flag.

A session is the vault entry `<profile>/session`
(`pk_cli_auth::vault::Vault::for_binary("labcorp")`): the access token and
its expiry, the Okta cookies (`login-patient.labcorp.com`) and the API
host's cookies as `Cookie` headers, the browser's user agent, and when it
was captured and renewed. `auth login --browser` runs Chrome in the
profile's own kept browser profile (`Launch::profile_name`) with its site
storage cleared on launch, so no token sits on disk between sign-ins.

`client::Client` renews an expired token (or one answered 401) with Okta's
session: an authorize request with `prompt=none`, the Okta cookies and a
fresh PKCE pair, the code read from the redirect's `Location` without
following it, then exchanged at the token endpoint. The renewed session is
written back at once. No password is ever stored or sent by `labcorp`.

Sign-in credentials for the household's accounts live in 1Password. A
session filling a sign-in reads them with `op read` and pipes them into the
page over stdin, never on argv or in output; the person still clicks every
consent, macOS dialog and texted code.

## The site

Be gentle with it: one request at a time, at least `client::PACE` apart, no
bulk crawling, no retry beyond the one after a renewal. Every URL is built
from the fixed API origin; `api` refuses another host. A 401 after renewal
and a 403 are exit 3, 404 is 4, the rest 5.

## Tests are offline, always

`cargo test` reads no real keychain item or browser profile, stores no
session, and contacts nothing beyond loopback. Every test that runs a
credential-reading command sets
`LABCORP_KEYCHAIN_SERVICE=piekstra.labcorp.hermetic-tests` (a service that
holds nothing) with a temp `XDG_CONFIG_HOME`: under it the vault is
`$XDG_CONFIG_HOME/labcorp/vault`, and labcorp leaves the kept browser
profiles alone. Unit tests use an in-memory vault and fill the profile seams
with `named_profile_exists_in` and `forget_named_profile_in` under a temp dir.

- `tests/cli_surface.rs`: every `--help`, exit codes, JSON errors, argument
  checks before anything is read, profile selection, `NO_COLOR`, a per-test
  `XDG_CONFIG_HOME`.
- `tests/no_personal_data.rs`.

`make verify` = fmt-check, clippy, test, smoke. Every `make` target that
builds a binary signs it (cli-common's `dev-sign.sh`).
