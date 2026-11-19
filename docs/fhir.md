# FHIR mapping

`records export` maps Labcorp Patient's results to FHIR R4 by hand
(`src/fhir.rs`; no FHIR library). The stream is `records-stream/v1` with
`method: "portal"` (medical-common DESIGN.md §1.4). Every resource carries
`meta.source` `labcorp:portal` and the tag `mapped-from-labcorp-patient`
(system `https://github.com/piekstra/labcorp-cli/fhir/tag`), and
`meta.lastUpdated` (DESIGN.md §1.4):

- the time its page was read for a result's `Observation`s and
  `DiagnosticReport` (when its detail was read), and the header's
  `pulled_at` for `Organization` and `Patient`. A later export's version
  therefore replaces the one filed before, a corrected value included; one
  whose content did not change only refreshes the copy filed.
- for a settled report's PDF (no test still to come or preliminary), its
  `Binary` and `DocumentReference` carry the date Labcorp lists the report
  with (`fhir::listed_at`): its `reportDate` as written when it has an
  offset, else that day, else the collection day, at `00:00:00Z`. A
  re-export uploads that PDF again only when Labcorp reissues it under a
  later date. The PDF of a report still changing, and of one with no date
  at all, carries the read time, so it is uploaded again on every import
  until it settles.

A settled report Labcorp corrects under the same `reportDate` (or on the
same day, when it gives the day alone) has a PDF no later than the one
filed, and medrec keeps the earlier PDF; the corrected values still
replace theirs, being dated by the read. Whether a correction moves
`reportDate` is to confirm against a real account.

## Identity

Every id derives from Labcorp's own ids, so a re-export carries the same
ids and the store merges it with what it filed before.

| Resource | Id | From |
|---|---|---|
| `Organization` | `labcorp` | fixed |
| `Patient` | `lcp-<patient id>` | the account's `id`, a dependent's `id`, a linked account's `patientMasterId` |
| `DiagnosticReport` | `lcr-<result id>` | the result header's `id` |
| `Observation`, an ordered test | `lco-<result id>-<test code>` | `orderedItems[].testCode`, else `i<place>` |
| `Observation`, a value | `lco-<result id>-<test code>-<result code>` | `results[].number`, else `n<place>` |
| `Binary` | `lcb-<result id>` | the report PDF |
| `DocumentReference` | `lcd-<result id>` | the report PDF; also its `documents/v1` id |

A character outside `[A-Za-z0-9.-]` becomes `.`. An id over 64 characters
becomes its prefix and 40 hex digits of the SHA-256 of the rest. An id met
twice in one report (Labcorp repeating a result code) gets its place
appended, so no value is dropped as a repeat.

## Resources, in stream order

1. `Organization` Labcorp, then the `Patient`: name, `birthDate`, `gender`,
   and Labcorp's id as an identifier (`https://patient.labcorp.com/fhir/patient-id`).
2. For each result, oldest collection first:
   - an `Observation` per reported value: `code` Labcorp's result code
     (`https://www.labcorp.com/tests/result-code`) and name; the value as
     `valueQuantity` when it is a decimal, with `comparator` for `<0.5`,
     `>=60` and the like, else `valueString`; `unit` as Labcorp writes it
     (not UCUM); `interpretation` from the abnormal flag (`H`, `L`, `HH`,
     `LL`, `A`, `AA`, `N` in v3-ObservationInterpretation, always also
     Labcorp's own code); `referenceRange` as text, with `low` and `high`
     when it reads as numbers; `note` from the comments. A flag of `T` (test
     not performed) or `Q` (quantity not sufficient) is a
     `dataAbsentReason` instead of a value.
   - an `Observation` per ordered test, a panel whose `hasMember` are its
     values: `code` Labcorp's test code (`https://www.labcorp.com/tests`).
   - the `DiagnosticReport`: category `LAB`, `code` the one test or the tests'
     names, `status` (`final`, `partial` while some tests are still to
     come, `preliminary`, `registered`), `result` the panels, `basedOn` the
     ordering provider and practice as text, `presentedForm` the PDF.
   - the `Binary` (the PDF, base64) and the `DocumentReference` that files
     it (LOINC `11502-2`, category `lab-report`, related to the report).

Every observation and report has `subject` the patient, `performer`
Labcorp, `effectiveDateTime` the day of collection, and `issued` the report
time when Labcorp gives it with an offset. The day of collection is a date
alone: the site's `dateOfService` ends in a `Z` that the web app strips,
because it is the lab's local time, not UTC.

A decimal keeps its written spelling: `1.50` is written `1.50`.

## Left out, with the trailer's skip code

| What | Code |
|---|---|
| A result with no value reported yet (`isDetailAvailable` false) | `not_ready` |
| A result whose detail the API refused (403) | `forbidden` |
| A result whose detail the API did not find (404) or failed (5xx) | `not_found`, `upstream` |
| A report PDF that is not a PDF | `verify_failed` |
| A report PDF the API refused, did not serve or failed | `forbidden`, `no_file`, `upstream` |

A failure that every later read would meet too ends the stream incomplete,
its error in the trailer: a refused session (a 401 after renewal, or Okta's
session ended; exit 3), a renewal that failed (exit 5), a request to wait (a
429 or `Retry-After`; exit 5) or no answer at all (exit 5). The next export starts again from the oldest
result; the ids make it merge with what was filed.

A result whose `pid` names a person the login does not show is nobody's
(docs/api.md, "Reads"): no export carries it, and the export warns on
stderr how many there are.

With `--since`, a result is exported when its report or collection day is
that day or later, so a result Labcorp reports again is exported again. A
result with no date is always exported. `Organization` and `Patient` are
always exported (`since_unbounded`). `results list --since` goes by the
collection day alone. The export has no `--until`: health-records/v1 gives
`records export` a `since` and no upper bound.
