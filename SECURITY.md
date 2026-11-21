# Security

Report a vulnerability privately through GitHub's "Report a vulnerability"
(Security tab), not in an issue.

## What labcorp touches

- Contacts no server except GitHub, for `self-update`, and Labcorp's:
  `https://patient.labcorp.com` in the Chrome window `auth login --browser`
  opens, where the person signs in on Labcorp's own page;
  `https://portal-api.patient.cws.labcorp.com` for the reads, every one a
  `GET`, one at a time and at least 400 ms apart, with a 30-second limit and
  no redirect followed; and `https://login-patient.labcorp.com` to renew an
  expired access token from the sign-in session (an authorize request with
  `prompt=none` and PKCE, then the token exchange). `labcorp api` is held to
  the same: `GET` only, on the API's origin only, checked before anything is
  sent.
- Stores one credential per profile, its session: the access token and its
  expiry, the sign-in and API cookies, the browser's user agent and the
  capture time, as the vault entry `<profile>/session`. The vault
  (`pk_cli_auth::vault`) is encrypted owner-only files under
  `~/.config/labcorp/vault/`, with one key in the keychain (service
  `piekstra.labcorp`, account `vault-key`). A session never goes on a command
  line or into a log; errors name a path without its query and never quote
  an answer's body.
- Keeps one Chrome profile per labcorp profile for `auth login --browser`,
  so Labcorp remembers the device:
  `~/Library/Application Support/piekstra.labcorp/browser-profile-<profile>`
  (under `$XDG_DATA_HOME` on Linux), owner-only. Its site storage, where the
  web app keeps its tokens, is cleared on each launch; the cookies stay,
  encrypted by Chrome. `--fresh-profile` signs in without it.
- `auth logout --profile <NAME>` and `profiles remove <NAME>` remove that
  profile's session, then its Chrome profile. `auth reset` removes every
  session and the vault key.
- The config file (`0600`) holds the profile names and, per profile, the
  patient ids and labels of the people a read has listed: no Labcorp id,
  no name, no secret. A patient id is a digest of Labcorp's id and is
  handled as that id, so the file is identifying. `config show` prints how
  many people each profile has listed and labelled, not their ids or
  labels.

## What labcorp never does

- Writes to Labcorp, or stores or sends a password.
- Sends a result or its contents anywhere: the output goes to stdout or a
  file you name.
