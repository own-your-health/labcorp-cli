# FHIR mapping

`records export` maps Labcorp Patient's results to FHIR R4 by hand
(`src/fhir.rs`; no FHIR library). The stream is `records-stream/v1` with
`method: "portal"` (medical-common DESIGN.md §1.4). Every resource carries
`meta.source` `labcorp:portal` and the tag `mapped-from-labcorp-patient`
(system `https://github.com/piekstra/labcorp-cli/fhir/tag`).

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
| A result whose detail the API refused or did not find | the error's (`upstream`, `not_found`, `forbidden`) |
| A report PDF that is not a PDF | `verify_failed` |
| A report PDF the API did not serve | `no_file`, `upstream` |

A refused session (exit 3) ends the stream incomplete, its error in the
trailer. With `--since`, a result is exported when its report or collection
day is that day or later; `Organization` and `Patient` are always exported
(`since_unbounded`).
