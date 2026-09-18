# d1a-protocol verification — CERTIFIED

Branch `feat/d1-scaleset-protocol` @ `966f3623` (fetched from origin, SHA
matches; base `411b8a99`; signed off; 26 files +4127/−7). Scope checked:
design §1 part A, §2.3 migration, §7 fixtures. No edits made. No merges/pushes.

## Audit claim: e6daac70 Go-identical to fb563005 — CONFIRMED

Ran independently in `/tmp/upstream-scaleset` (after `git fetch origin`):
- `git diff fb563005..e6daac70 -- '*.go' | wc -c` → **0 bytes**
- Full diff touches only `.github/workflows/e2e.yaml` + `go.yaml` (CI bumps)
- `git log fb563005..e6daac70` → single commit `e6daac7` (#127)
- `SCALESET_UPSTREAM_COMMIT` = full `e6daac70…` SHA; old `cb0405b2` gone;
  `require_pin` fails closed (fb563005 rejected by unit test).

## Client function vs upstream symbol refs — all mirrored

`client.go` → `client.rs`: NewClientWithGitHubApp/PAT/JWTProvider →
new_with_app/pat/jwt_provider, newClient→new, SetSystemInfo/SystemInfo,
getRunnerScaleSet/List/ByID, GetRunnerGroupByName, Create/Update/Delete,
GetRunner/GetRunnerByName/RemoveRunner, GenerateJitRunnerConfig,
getRunnerRegistrationToken/fetchAccessToken/getActionsServiceAdminConnection,
updateTokenIfNeeded (double-checked + 60s skew), actionsServiceAdminTokenSnapshot,
applyDefaultLabelTypes/ensureLabels, joinURLPath, requestURL (path-query merge,
api-version default), adminTokenExpiresAt (unverified exp). Runner-groups
verbatim path `/_apis/runtime/runnergroups/` + `groupName` query kept.
Error strings verified verbatim incl. the quirky `multiple runner group found`
and `could not decode job started message.`. GetRunnerByName-multi stays a
plain local error (no exception mapping), exactly like upstream.

`session_client.go` → `session.rs`: create (POST …/sessions 200),
PATCH-refresh with stale-session short-circuit, close (DELETE 204),
GET queue + `lastMessageId` iff > 0 + `Accept: application/json;
api-version=6.0-preview` + queue Bearer + UA + `X-ScaleSetMaxCapacity`,
202→None, 401→refresh→retry-once on all three ops, DELETE ACK 204,
AcquireJobs via Actions-Service URL with queue-token override returning the
subset, batch dispatch on messageType with unknown-kind ignore.

`errors.go` → `errors.rs`: all 8 sentinels with exact strings, ActivityId /
X-GitHub-Request-Id capture, Agent*Exception mapping, text/plain passthrough,
400/401/404/409 wrap order, BOM strip applied to every body before decode.

`config.go` → `config.rs`: org/repo/enterprise parse, `enterprises` routing,
FORCE_GHES, www→api.github.com reroute, `.ghe.com`, api/v3, all three
registration-token paths. `jwt_provider.go` → `credentials.rs`: RS256,
iss/iat−60s/exp+9m (unit-tested against decoded claims), PEM eager parse,
closure adapter, KMS-ready trait; both Validate() message sets verbatim.

`types.go` → `scheduler.rs`: every wire field + JSON name checked —
requestLabels, all four job bases, `acquireJobUrl`, statistics 7-tuple,
`value` renames on all three list shapes, `RunnerSetting`/`createdOn` WITHOUT
omitempty (#110), exact `encodedJITConfig` casing, `runner: null` (no skip),
session UUID-as-string. Deliberate non-mirrors (declared in d1a-protocol.md)
confirmed present and documented: timestamps as String, no retry jitter.

`common_client.go` retry → `backoff.rs` + `execute_with_retry`: retryMax=4
(5 attempts), 30s cap, 5min long-poll timeout, 429 + 5xx-except-501 +
transport-error classification, admin-handshake 401/403 extension, UA JSON
with `kind: "scaleset"` and all 8 fields. Retry wraps every request path
(GitHub API, Actions Service, queue), matching `c.do` coverage.

## Migration v21 — matches design §2.3 DDL exactly

`scaleset_demand` (PK request_id, UNIQUE sequence, order index on
state/first_seen_at/sequence), `job_permits`, `scaleset_sessions`
(last_message_id DEFAULT 0), `scaleset_workers` (PK ownership_id, UNIQUE
operation_id + runner_name); `v21_schema_complete` chain + transactional
convergence guard + upgrade/fail-closed tests both present and passing.

## Keys out of jobs — CONFIRMED

`Serialize` appears only for JWT claims (iss/iat/exp); no private-key, PAT,
or installation-id field exists in any model wire type; `ActionsAuth::Debug`
redacts PAT; JIT setting carries name/workFolder only; the encoded JIT blob
is the sole runner-bound credential, as upstream. Fixtures: manifest pins
e6daac70; all 11 sha256 recomputed independently — ALL OK; secrets REDACTED,
hosts `.invalid`, redaction scanner rejects PEM/token prefixes/foreign hosts.

## Test rerun (scratch worktree @ 966f3623, since removed)

- `cargo test -p velnor-runner -p velnor-model`: 2212 lib + all integration
  targets pass, 138 model lib pass, 0 failed
- `--features test-support --test scaleset_protocol`: 8/8 pass (wiremock,
  zero live calls)
- `cargo test -p velnor-control`: 269 lib (incl. both new v21 tests,
  confirmed by name) + all integration pass
- `cargo clippy -p velnor-model -p velnor-runner -p velnor-control
  --all-targets` (+ runner `--features test-support`): 0 warnings
- `cargo fmt --check`: clean

## Verdict: CERTIFIED

Non-blocking observations for part B (none fail the contract):
1. Queue DELETE trims a trailing slash before appending `/id`; upstream
   formats raw `%s/%d` (equivalent on real queue URLs).
2. Error Display renders `status="401"`; upstream renders `status="401
   Unauthorized"` (cosmetic).
3. Exception-name classification (AgentNotFound→runner-not-found) lives in
   the message text; the typed `fault()` keeps the status-code mapping only.
   No consumer in part A; revisit if part B matches on typed faults.
4. Derived `Debug` on `PemJwtProvider`/`GitHubAppAuth` would print PEM if
   ever Debug-logged (not wired into any log path today; `Config`→
   `ActionsAuth` redacts).
5. `github_api_url` preserves config-URL query/fragment where upstream
   rebuilds scheme+host+path only (pathological input; config URLs never
   carry queries).
