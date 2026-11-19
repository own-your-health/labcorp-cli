# Labcorp: how lab results are reached

Labcorp is a laboratory, not an EHR certified under ONC's §170.315(g)(10), so
no rule requires it to offer patients a FHIR API. Findings below are from
Labcorp's public pages, read on 2026-11-19 without signing in, unless marked
otherwise. Quotes are Labcorp's own wording.

| | Labcorp developer APIs | Labcorp Patient (patient.labcorp.com) |
|---|---|---|
| What | Ordering, order results, scheduling, billing and coverage APIs for Labcorp's business partners | A person's own results with reference ranges, the report PDFs, invoices |
| Who it serves | Partners (health systems, EHR vendors, biopharma) under an agreement with Labcorp | Every person with a Labcorp Patient (MyLabcorp) account |
| Registration | OAuth 2.0 client credentials issued after "Sales, divisional approval, then setup" | None beyond the account |
| Patient access | None: client credentials carry no patient's consent, and results are per partner order | The account holder's own results, and those of people who shared with them |
| `labcorp` source | none | `--profile <NAME>`: a session signed in through `pk-cli-browser` |

## Route

Decided: the member site, Labcorp Patient. Labcorp offers no patient-facing
API: its developer portal serves partners only, ONC's Certified Health IT
Product List has no Labcorp product, and the one patient integration Labcorp
announced (Apple's Health Records, 2018) is Apple's own client, not open to
other apps. `labcorp` signs in to patient.labcorp.com in a Chrome window and
reads the site's own JSON API, the one its web app calls.

## Labcorp's developer portal (developer.labcorp.com)

- The API Library lists: ABN Service, Body Site, Microbiology Source,
  Outreach On-Demand Order Submission, Test Order Modification, Order
  Results, Test Order Tracking, Order Submission, Outreach Order Submission,
  Patient Scheduling, Specimen Management, Test Code Lookup and Pricing, and
  the AccuDraw SOAP service for EMRs.
- Getting started: "Request OAuth 2.0 client credentials for your
  application using the Contact Sales button at the top of the page. New API
  access goes through Labcorp's standard approval process (Sales, divisional
  approval, then setup) before credentials are issued, so expect some lead
  time."
- Authentication: "Your application authenticates to the gateway with the
  OAuth 2.0 Client Credentials flow". Token endpoints:
  `https://auth.aperture.labcorp.com/oauth2/token` (production),
  `https://auth-certification.dev.aperture.labcorp.com/oauth2/token`
  (certification), `https://auth-stage.dev.aperture.labcorp.com/oauth2/token`
  (sandbox). "Tokens are valid for one hour."
- Order Results API: "Order Results service for retrieving test results on
  existing Labcorp orders." A 403 means "The requesting partner is not
  authorized to retrieve results for this order", and a 404 that "the
  partner's subscription is not configured". It answers a partner about its
  own orders; there is no patient grant.

## Terms

Labcorp's Online Services User Agreement ("THIS USER AGREEMENT WAS LAST
UPDATED ON JULY 22, 2026") governs patient.labcorp.com; its §31 adds terms
for "MyLabcorp and/or Labcorp Patient Portal".

- §2(a), the licence: "solely for your personal, non-commercial,
  informational use"; "nor may you decompile, disassemble, or reverse
  engineer (or attempt to derive source code from) any part of the Online
  Services".
- §12(vi): you agree not to "use any automated means to collect information
  or Content from, index, crawl, or otherwise access the Online Services,
  including robots, spiders, or scrapers, without our prior written
  permission, or circumvent or ignore robots.txt or similar headers".
- §12(xii): not to "bypass, disable, or interfere with rate limits, access
  controls, usage caps, or features intended to enforce this User
  Agreement".
- §17 ("Registration and Passwords"): "You agree not to share your credentials or permit any
  other person to access your Account, except where a feature of the Online
  Services expressly allows you to authorize another person." §31(c) is that
  feature: an account holder may share results with another person or make
  them a co-custodian.
- §31(a): "MyLabcorp is provided to individual consumers for personal use
  only."
- `robots.txt` on patient.labcorp.com is the web app's own page (no rules);
  www.labcorp.com's disallows `/patients/labs-and-appointments/results` and
  the login paths, none of which `labcorp` requests.

The household accepted the risk of automated access to the sites whose terms
forbid it (2026-11-19). `labcorp` reads only what the
account's own web app reads, one request at a time, and never writes.

## Labcorp Patient (patient.labcorp.com)

The web app ("MyLabcorp", an Angular app, version 1.6.1 on 2026-11-19) loads
its settings from `https://patient.labcorp.com/assets/environment.json`
(public, no sign-in):

| Setting | Value |
|---|---|
| `api.baseUrl` | `https://portal-api.patient.cws.labcorp.com` |
| `okta.issuer` | `https://login-patient.labcorp.com/oauth2/default` |
| `okta.clientId` | the web app's public client id, `0oaympyx2kMM41A140x7` |

### Sign-in

- Okta Identity Engine at `login-patient.labcorp.com`: the app sends the
  browser to `/oauth2/default/v1/authorize` (authorization code with PKCE,
  redirect `https://patient.labcorp.com/callback`, scopes `openid profile
  email`). The page asks for the email, then the password (`/idp/idx/identify`,
  then `/idp/idx/challenge/answer`). A wrong password answers HTTP 401 with
  Okta's `E0000004` "Authentication failed", shown as "Unable to sign in".
- The scopes carry no `offline_access`, so there is no refresh token. The
  web app renews its tokens with Okta's session cookie: an authorize request
  with `prompt=none` from a hidden frame (`getWithoutPrompt`).
- After the tokens arrive, the app calls
  `POST /protected/patients/current/login` on the API with the access token
  as a bearer.

### The session, as labcorp keeps it

From the web app's code (`main.<hash>.js`, 2026-11-19); the parts marked
"to confirm" wait on the first signed-in capture.

- The Okta client is built with `tokenManager: {autoRenew: false,
  autoRemove: false, syncStorage: false}`. Its default storage is
  `localStorage`, key `okta-token-storage` (to confirm), holding
  `accessToken.accessToken` and `accessToken.expiresAt` (Unix seconds).
  `site::TOKEN_EXPRESSION` reads those two once the app holds them.
- The capture also reads the whole `labcorp.com` cookie jar and keeps two
  `Cookie` headers: the cookies `login-patient.labcorp.com` would be sent
  (Okta's session, `idx` and the device cookie `DT`) and those the API host
  would be sent (`site::cookie_header_for`). Whether the API needs any
  cookie beside the bearer is to confirm.
- The session check is `GET /protected/patients/current` with the bearer:
  a JSON object is a signed-in account (`site::judge`).
- Renewal (`client::Client::renew`): `GET {issuer}/v1/authorize` with
  `prompt=none`, `response_mode=query`, the web app's client id and
  redirect, a fresh PKCE pair, `state` and `nonce`, carrying the Okta
  cookies. Okta answers with a redirect to the callback: `code` while its
  session lasts, `error=login_required` once it has ended. The code is
  exchanged at `{issuer}/v1/token`. How long Okta's session lasts is to
  confirm.

### Reads

The portal API paths the web app calls, from its code. Their answers are
not yet mapped; `labcorp` sends only `GET`.

| Path | What |
|---|---|
| `/protected/patients/current` | the signed-in account |
| `/protected/patients/current/dependents` | people the account manages |
| `/protected/patients/current/linkedAccounts` | accounts shared with this one |
| `/protected/patients/current{scope}/results/headers/all` | every result's header |
| `/protected/patients/current{scope}/results/headers/summary` | the result summary |
| `/protected/patients/current{scope}/results/{id}` | one result with its values |
| `/protected/patients/current{scope}/results/{id}/pdf` | its report PDF |
| `/protected/patients/current/linkedAccounts/results/resultTrends?collectionStartDate=` | trends across results |
| `/protected/patients/current/invoices/summary` | invoices |
| `/protected/patients/current/invoices/pdf?documentKey=` | an invoice PDF |

The web app reads the list for everyone at once, through
`/linkedAccounts/results/headers/all`; each header names its patient
(`pid`, `patientName`). A result's detail and PDF are read under the
person's scope: empty for the account itself, `/linkedAccounts/{id}` for a
linked account (its `recipientLinkedId`, else its `linkedId`), and
`/linkedAccounts` for a dependent the account's record lists
(`dependents[].dependent`). To confirm against real answers.

`labcorp` files a result under the person its `pid` names, and only a
header with no `pid` under the one person its name names (or the only
person). A header whose `pid` names nobody the login shows is left out of
every export with a warning, never filed under someone else; `results list`
shows it with no patient. If the linked accounts answer 404, `labcorp`
warns and reads the account and its dependents alone.

From the web app's models:

- a header: `id`, `pid`, `patientName`, `dateOfService`, `reportDate`,
  `orderedDate`, `orderingProviderName`, `accountName`, `isDetailAvailable`,
  `hasMinimumDataForPdfRetrieval`, `dashboardCategory` (`new`, `upcoming`,
  `updated`, `recently_viewed`), `testCodes`, `totalTests`,
  `completedTests`, `itemStatuses` (per test, a `status`: `FINAL`,
  `PRELIMINARY`, `UPCOMING`);
- a detail: `header`, and `orderedItems[]` of `testName`, `testCode`,
  `summary`, `itemStatus` and `results[]` of `number`, `name`, `value`,
  `abnormalIndicator`, `referenceRange`, `units`, `status`, `comments`;
- the account: `id`, `firstName`, `middleName`, `lastName`, `dateOfBirth`,
  `gender`, `dependents[]` of `{relationshipType, dependent}`;
- a linked account: `patientMasterId`, `linkedId`, `recipientLinkedId`,
  `firstName`, `lastName`, `isMinor`, `linkedAcctToCurrentUserStatus` and
  `currentUserToLinkedAcctStatus` (`accepted` when shared).

## Traps

- `DELETE /protected/patients/current` deletes the account: the web app's
  profile page sends it. `labcorp` sends `GET` only, and `labcorp api`
  refuses every other method before anything is sent.
- `dateOfService` ends in a `Z` that is not UTC: the web app strips it and
  shows the lab's local time. `labcorp` keeps the day alone.
- An abnormal flag of `T` means "Test Not Performed" and `Q` "Quantity Not
  Sufficient": the value is absent, not abnormal.
- A result line whose name starts `Please Note:`, `Comment:`, `Disclaimer`,
  `Interpretation` or `Note:` is a note the web app shows as text.

- The landing page's cookie banner (OneTrust) covers the sign-in button
  until it is answered.
- An email Okta does not know gets the same password page and the same
  `E0000004` as a wrong password, so "Unable to sign in" does not say which.
  Each failure counts toward Okta's lockout: check the stored login before a
  second attempt.
