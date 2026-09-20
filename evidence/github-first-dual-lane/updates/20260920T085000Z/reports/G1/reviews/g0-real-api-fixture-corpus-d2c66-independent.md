# G0 real API fixture corpus — independent integrity review

Target: `G0/real-api-fixture-corpus-20260920T080318Z/`.
Review was read-only. No API request, workflow dispatch, source edit, rollout,
or gate decision was made.

## Verdict

**APPROVE as an immutable, bounded real-API regression corpus only.** It is not
all-32-repository evidence, current live authority, or a G0/hosted gate.

## Identity and raw-byte integrity

- `manifest.json` SHA-256: `d2c66c7bb61dfa6a0ddf4ea336feb8704b1c137aaba6031c2930ca58f5fae2bb`.
- `relationship-report.json` SHA-256: `28b88ef07aa00166e3914f5abe8ff852c98a7fc2dfde2ca618da6d3e50983845`.
- Manifest declares `real-github-api-fixture-corpus.v1`, `read_only_source_corpus_not_gate`, raw-byte copying, and 28 raw files (`manifest.json:1-39`).
- Recomputed manifest joins: 28/28 raw paths present; 0 SHA-256 or byte-count mismatches; filesystem count is exactly 28.
- Manifest request-record references: 28/28 present, unique, and non-duplicated.
- README says the Velnor and Homebrew captures are separate and metadata `org/user` files retain combined response envelopes; no credential material is included.

## Envelope, endpoint, and pagination proof

Ten request records are all HTTP 200 and map to the expected endpoints: one
Velnor run, attempt 1, jobs, artifacts, Velnor check-runs, Velnor statuses,
one separate Homebrew check-runs response, Velnor repository metadata, and
org/user metadata. All eight `.http` files are `HTTP/... 200`, JSON content
type, and have no explicit `Link` header. The two direct metadata envelopes
parse as JSON after their header delimiter. All endpoint body files parse as
JSON.

Complete single-page relationships (report lines 21 onward) are independently
consistent with raw bodies:

| Relation | API total | Array items | Pagination |
|---|---:|---:|---|
| Velnor attempt-1 jobs | 68 | 68 | page 1 complete |
| Velnor run artifacts | 2 | 2 | page 1 complete |
| Velnor commit check-runs | 70 | 70 | page 1 complete |
| Homebrew check-runs | 6 | 6 | page 1 complete |
| Velnor commit statuses | 0 | `[]` | complete empty response |

No page is silently omitted: every manifest record has `page=1`,
`has_next=false`, and `link_header=absent`; relationship totals equal raw array
lengths.

## Nested identity and outcome relationships

- Selected Velnor run is `35493166478`, attempt `1`, head
  `df9fb272c025f76cc8711560209afcdfd6cc4e00`, workflow `347892207`, check suite
  `96108227551`, pull-request event, completed/success, actor and triggering
  actor `donbeave`.
- Run and attempt bodies agree on ID, attempt, head, workflow, and conclusion.
  All 68 jobs have run ID `35493166478`, attempt `1`, the same head SHA, and
  unique job IDs; outcomes are 20 success and 48 skipped.
- Both artifacts bind to workflow run `35493166478`, have non-null SHA-256
  digests, and expose `workflow_run.run_attempt: null`. The corpus preserves
  that API omission; it does not infer artifact attempt 1.
- All 70 Velnor checks bind to the selected head and have unique IDs. They
  contain 69 GitHub Actions checks plus one DCO check. DCO is completed/success;
  the one failed Velnor check is `Policy` (not hidden by the corpus).
- Homebrew's six checks all bind to commit
  `c501e90d014c207234ed94ea41f7a1c9b6ea0c7c`; exactly one is SonarCloud Code
  Analysis, completed/failure. The Sonar record is explicitly a separate
  provider-positive fixture, not a Velnor check (`relationship-report.json:364+`).

## Auth and scope boundaries

Manifest auth principal is `donbeave`; raw run actor fields agree. A scan found
no `Authorization:` header, bearer/access token, or GitHub token value. Raw
headers expose OAuth scopes/client ID and ordinary rate-limit metadata only;
these are not credentials. The corpus explicitly excludes credential material
and live-authority claims.

The report preserves material unknowns: Velnor Sonar is absent from the complete
70-check page, statuses is a complete empty response, artifact attempt is API
null, and captured bytes are historical fixtures. This corpus therefore proves
fixture integrity and relationship handling—not current gates, all-32 coverage,
or execution beyond the captured provider records.
