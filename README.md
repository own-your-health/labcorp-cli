# labcorp

Unofficial; not affiliated with or endorsed by Labcorp.

`labcorp` reads a person's Labcorp lab results and their report PDFs from
Labcorp Patient (patient.labcorp.com), signed in as that person, for the
household's medical-records system (`health-records/v1`). It is read-only:
nothing it does writes to Labcorp.

`docs/api.md` records why there is no API route (Labcorp's developer APIs
serve its business partners, with no patient grant), what Labcorp's terms say
about automated access (the household accepted that risk), how the sign-in
works, and every read `labcorp` makes.

## Install

```sh
cargo install --git https://github.com/piekstra/labcorp-cli --locked
# or download a release: labcorp-<target>.tar.gz, then `labcorp self-update` later
```

`auth login --browser` needs Google Chrome.

## Use

One profile per Labcorp Patient account that signs in:

```sh
labcorp profiles add adult1
labcorp auth login --browser --profile adult1   # sign in in the Chrome window it opens
labcorp auth status --profile adult1            # stored? captured when? token good until?

# One read of the portal API, printed as the JSON it answers:
labcorp api GET /protected/patients/current --profile adult1
```

There is no default profile: every command that signs in or reads takes
`--profile <NAME>` (or `LABCORP_PROFILE`), and a lone configured profile is
never picked for you.

`labcorp api` sends `GET` only, to `https://portal-api.patient.cws.labcorp.com`
only; any other method or host is exit 2 before anything is sent.

The session: the sign-in window hands `labcorp` the web app's access token
and Labcorp's sign-in cookies. When the token expires, `labcorp` renews it
the way the web app does, from the sign-in session, with no password. When
that session has ended too, a read is exit 3 naming `auth login --browser
--profile <NAME> --overwrite`.

## Commands

| Command | What |
|---|---|
| `profiles list\|add\|remove` | the configured accounts (`profile-list/v1`) |
| `auth login --browser --profile <NAME>` | sign in in a Chrome window; one read checks the session before it is stored (`auth-login/v1`) |
| `auth status --profile <NAME>` | whether a session is stored and until when its token is good; sends nothing (`auth-status/v1`) |
| `auth logout --profile <NAME>` | remove the session, then the kept browser profile (`auth-logout/v1`) |
| `auth reset` | remove every session and the vault's key (`auth-reset/v1`) |
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
