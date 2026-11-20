# labcorp

Unofficial; not affiliated with or endorsed by Labcorp.

`labcorp` reads your Labcorp lab results and their report PDFs from Labcorp
Patient (patient.labcorp.com, formerly MyLabcorp), signed in as you. It
writes them as FHIR R4 (`records-stream/v1`) for
[medrec](https://github.com/piekstra/medrec-cli), or as files and JSON you
keep yourself. It is read-only: nothing it does writes to Labcorp.

## Who it is for

Anyone with a Labcorp Patient account. One account reads:

- its own results;
- the results of dependents on its record (children);
- the results of other accounts that chose to share theirs with it.

Each result comes with every value, unit, abnormal flag, reference range
and comment, and its report PDF.

You need:
- macOS, where the release builds are tested. Linux builds are published;
  they keep the session in the desktop's Secret Service keyring.
- Google Chrome, for the sign-in.
- Your Labcorp Patient email and password.

## No developer registration

There is nothing to register and no client id to obtain. Labcorp's
developer APIs (developer.labcorp.com) serve its business partners: health
systems, EHR vendors and biopharma. Their credentials are issued after
Labcorp's sales and divisional approval, carry no patient's consent, and
answer a partner about its own orders. Labcorp offers patients no API of
their own. So `labcorp` signs in the way you do, in a Chrome window on
patient.labcorp.com, and reads the JSON the Labcorp Patient web app reads
with that session. `docs/api.md` has the evidence.

## Labcorp's terms, in plain words

Labcorp's Online Services User Agreement (updated July 22, 2026) governs
patient.labcorp.com. In short:
- Use the site for your own personal, non-commercial purposes only.
- Do not access it by automated means, including "robots, spiders, or
  scrapers", without Labcorp's prior written permission (§12(vi)).
- Do not reverse engineer any part of it (§2(a)).
- Do not get around its rate limits or access controls (§12(xii)).
- Do not let anyone else into your account, except through the sharing
  Labcorp provides (§17, §31(c)).

`labcorp` is automated access, and Labcorp has not given permission for it.
Using it is against those terms, and Labcorp could close or suspend an
account it finds doing so. Decide for yourself whether to accept that risk.
`labcorp` keeps its footprint to what the web app itself does:
- it reads only what the web app reads for your account;
- it sends one request at a time, at least 400 ms apart;
- it stops when Labcorp asks it to wait, and never retries past that;
- it never writes, and `labcorp api` refuses every method but `GET`.

This is a summary, not legal advice. `docs/api.md` quotes the terms.

## Install

```sh
cargo install --git https://github.com/piekstra/labcorp-cli --locked
# or download a release, labcorp-<target>.tar.gz, and use `labcorp self-update` later
```

## Quick start

```sh
# 1. A profile per Labcorp Patient account. The name is yours; it never leaves your computer.
labcorp profiles add me

# 2. Sign in. A Chrome window opens on patient.labcorp.com: sign in there.
#    labcorp checks the session with one read, keeps it, and closes the window.
labcorp auth login --browser --profile me
labcorp auth status --profile me            # stored? token good until? sends nothing

# 3. Who this account reads, and their results.
labcorp patients list --profile me          # ids such as lcp-<id>
labcorp results list --profile me --patient <ID> --since 2026-01-01
labcorp results get <RESULT ID> --profile me            # every value, flag and range
labcorp documents download --all -o ~/Labcorp --profile me   # the report PDFs

# 4. Into medrec: medrec runs the export itself, with your profile.
medrec --profile <FOLDER> import --from labcorp --from-profile me --patient <ID> --person <PERSON>

# Or the stream on stdout, for any reader of records-stream/v1:
labcorp records export --profile me --patient <ID> --ndjson > labcorp.ndjson
```

`--patient` takes an id from `patients list`, Labcorp's own id, or a first
name only one person has. It may be left out when the account shows one
person. `records export --since D` takes results reported or collected on
or after D; `results list --since D` goes by the collection day.
`docs/fhir.md` describes the FHIR the export writes.

There is no default profile. Every command that signs in or reads takes
`--profile <NAME>` (or `LABCORP_PROFILE`), and a lone profile is never
picked for you.

## What needs a person

| Step | Why |
|---|---|
| Signing in, in the Chrome window `auth login --browser` opens | Labcorp's sign-in page: your email and password, its cookie banner, and any code Labcorp sends by text or email. `labcorp` never stores or sends a password. |
| The macOS Keychain dialog, once per new or upgraded `labcorp` build | The session is sealed with a key in your Keychain; choose "Always Allow". A build waiting on that dialog hangs until you answer it. |
| `auth reset` and `profiles remove` | They delete; at a terminal they ask, and elsewhere they need `--force` (exit 6 without it). |

Everything else runs unattended while the session lasts: listing,
downloading, exporting and `medrec import`.

## How long a sign-in lasts

The sign-in hands `labcorp` two things:
- the web app's access token, which expires on Labcorp's schedule
  (`auth status` shows when, as `token_expires_at`);
- Labcorp's sign-in session cookies.

When the token expires, `labcorp` renews it from the sign-in session the way
the web app does, with no password, and keeps the new one
(`renewed_at` in `auth status`). Once the sign-in session itself has ended,
a read is exit 3 and names the command that signs in again:

```sh
labcorp auth login --browser --profile me --overwrite
```

How long Labcorp keeps a sign-in session alive is not yet measured. Expect
to sign in again after a long absence, and see `auth status` for when the
session was captured and last renewed.

`labcorp api GET <PATH> --profile me` prints one read of the portal API as
the JSON it answers. It sends `GET` only, to
`https://portal-api.patient.cws.labcorp.com` only; any other method or
host is exit 2 before anything is sent. `docs/api.md` lists the reads.

## Commands

| Command | What |
|---|---|
| `profiles list\|add\|remove` | the configured accounts (`profile-list/v1`) |
| `auth login --browser --profile <NAME>` | sign in in a Chrome window; one read checks the session before it is stored (`auth-login/v1`) |
| `auth status --profile <NAME>` | whether a session is stored and until when its token is good; sends nothing (`auth-status/v1`) |
| `auth logout --profile <NAME>` | remove the session, then the kept browser profile (`auth-logout/v1`) |
| `auth reset` | remove every session and the vault's key (`auth-reset/v1`) |
| `patients list` | everyone the login reads (`patient-list/v1`) |
| `results list [--patient ID] [--since D] [--until D]` | results, newest collection first (`result-list/v1`) |
| `results get <ID>` | one result with its values (`result/v1`) |
| `documents list`, `documents download <ID>\|--all` | the report PDFs (`documents/v1`) |
| `records export --patient <ID> --ndjson` (or `-o DIR`) | one person's results as `records-stream/v1`; `--since D`, `--no-pdfs` |
| `api GET <PATH> --profile <NAME>` | one read of the portal API |
| `config path\|show` | the config file (profile names only; no secret) |
| `info` | `cli-info/v1` |
| `self-update`, `completions <SHELL>` | |

Every command takes `--json` (one document on stdout) and `-q`. Exit codes:
0 ok, 2 usage, 3 auth, 4 not found, 5 upstream, 6 confirmation needed.

## Where things are kept

- The session, per profile: `labcorp`'s vault, encrypted owner-only files
  under `~/.config/labcorp/vault/` sealed with one key in the macOS
  Keychain (service `piekstra.labcorp`, account `vault-key`).
- The Chrome profile each `--browser` sign-in keeps, so Labcorp remembers
  the device: `~/Library/Application Support/piekstra.labcorp/browser-profile-<NAME>`.
  Its site storage is cleared on each launch; `--fresh-profile` signs in
  without it.
- The config: `~/.config/labcorp/config.json`, profile names only.

## Licence

MIT or Apache-2.0, at your option.
