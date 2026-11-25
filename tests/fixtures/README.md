# Fixtures

Every fixture here is synthetic: written by hand from the fields the
Labcorp Patient web app's public code reads (2026-11-19), never copied from
a real answer or shaped on a real person's results. A fixture holds one
record per field or rule it exercises: no story, so no sequence of results
and no result with several ordered tests. A test that needs a field to
change sets it in the answer the test server gives. `tests/no_personal_data.rs` checks each identity field against
the dummy shapes below, by key.

## The household

| Who | Labcorp id | How the account reads them |
|---|---|---|
| ALEX EXAMPLE | `SYNP000001` | the signed-in account |
| SAM EXAMPLE | `SYNP000002` | a dependent on the account's record |
| JORDAN EXAMPLE | `SYNP000003` | a linked account that accepted sharing (`linkedId` `SYNL000003`) |
| (a second ALEX EXAMPLE) | `SYNP000004` | a linked account still `initiated`: never read |

## Dummy shapes

| Field | Shape |
|---|---|
| `firstName` | `Alex`, `Sam` or `Jordan` |
| `lastName` | `Example` |
| `patientName` | one of the first names, upper case, then `EXAMPLE` |
| `dateOfBirth` | a January 1st |
| `id`, `pid`, `patientMasterId` | `SYNP` and six digits, or `SYNR` and eight (a result) |
| `linkedId`, `recipientLinkedId` | `SYNL` and six digits |
| `hashId` | `example-hash-` and four digits |
| `orderingProviderName` | `Example ` and a word |
| `accountName` | `Example ` and a word |
| `testName`, `name` | starts `Example ` (or is a note label, `Please Note:`) |
| `testCode`, `number` | six digits starting `0000` |
| `comments[]`, a note's `value` | starts `Synthetic ` |

The PDF the tests serve is built in `tests/site_e2e.rs` and is never a
tracked file.

## Files

| File | Answers |
|---|---|
| `site/account.json` | `GET /protected/patients/current` |
| `site/linked-accounts.json` | `GET /protected/patients/current/linkedAccounts` |
| `site/headers-all.json` | `GET /protected/patients/current/linkedAccounts/results/headers/all` |
| `site/account-alone.json` | `GET /protected/patients/current` for an account with no dependents |
| `site/result-<id>.json` | `GET /protected/patients/current{scope}/results/<id>` |
