# Changelog

## v0.2.1

The repository moved to `own-your-health/labcorp-cli`, and medical-common to
`own-your-health/medical-common`. GitHub redirects the old URLs.

- `self-update` reads releases from `own-your-health/labcorp-cli`.
- medical-common is fetched from its new URL, at the same v0.6.10 commit.
- The FHIR coding and identifier systems labcorp writes keep their
  `https://github.com/piekstra/labcorp-cli/...` URIs. They are identifiers
  that stored records already carry, not links.

## v0.2.0

Breaking: a person's patient id, and what `--patient` takes (medical-common
DESIGN.md §1 and §1.3).

- A person's `Patient.id`, the id `patients list` prints and a stream
  header's `patient.id`, is now a one-way digest of Labcorp's patient id:
  `l` and the first 16 hex digits of SHA-256 over `7:labcorp` and the
  patient id, written as its length in bytes, `:` and the id. Before, it
  was `lcp-` and Labcorp's patient id as written (characters outside
  `[A-Za-z0-9.-]` made `.`; one over 64 characters became `lcp-` and 40
  hex digits of the SHA-256 of the id). A store that filed a person under
  the old id files them again under the new one; the two are the same
  person, and `Patient.identifier` (system
  `https://patient.labcorp.com/fhir/patient-id`) carries Labcorp's id in
  both.
- `--patient` takes a patient id or a label. It no longer takes a first
  name, which stays in shell history and in an agent's transcript, or
  Labcorp's own id.
- Errors no longer list the people's ids or repeat the `--patient` value.
  They say how many people the login shows and that `labcorp patients list`
  lists them, and name a person by label when they have one.
- `patients label --profile <NAME> --patient <ID> (<WHO> | --clear)` sets
  or clears the household's own name for one person. `--patient` then
  takes the label. It makes no request: every read of the account keeps the
  people it lists in the profile's settings, by patient id and with no
  name. `info` lists the `patient-labels` capability. `patients list` and
  the stream header carry the label.
- medical-common v0.6.10, with every `pk-cli-*` on cli-common v0.33.5. The
  `--json` error now carries `reason` and `remedy`.

## v0.1.2

The report PDF's `Binary` and `DocumentReference` carry the date Labcorp
lists the report with; every other resource the time it was read
(medical-common v0.6.5 §1.4).

## v0.1.1

`meta.lastUpdated` on every resource (medical-common v0.6.3).

## v0.1.0

The first release: `profiles`, `auth`, `patients list`, `results`,
`documents`, `records export`, `api` and `info` over Labcorp Patient.
