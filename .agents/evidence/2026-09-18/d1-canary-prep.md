# D1 live-canary conformance harness — PREP (design only, no live calls, no writes)

Status: read-only design. Inputs: `/tmp/d1-design.md` (Rust Scale Set adapter design),
`plans/bastion-three-provider-ci/spec.md` §5.1 (9-step order) + §5.2/§5.3 + §6 (credentials),
`/tmp/upstream-scout.md` (actions/scaleset @ `fb563005`, actions/runner v2.337.0),
`/tmp/d1-gapmap.md`, D1 done-gate in `plans/bastion-three-provider-ci/checklist.md:18`
("recorded fixtures + LIVE canary conformance (mocks alone rejected)").

Conformance target: the §5.1 9-step processing order as implemented by
`crates/velnor-runner/src/scaleset/` (`listener.rs` poll→Scale→ACK loop, `scale.rs`
`process_message`, `session.rs`, `demand.rs`, `allocator.rs`, `converge.rs`,
`capacity.rs`, `backoff.rs`, `worker/`, `reconcile.rs`) against REAL GitHub
(`_apis/runtime/runnerscalesets`, api-version `6.0-preview`). Mocks/fixtures prove
replay-safety; only live GitHub proves wire semantics (202/401/429 shapes,
redelivery, `TotalAssignedJobs` authority, capacity-header semantics).

---

## 1. Canary scope: org + repo + scale set

- **Org:** `tailrocks` (project org; cf. spec §7 `tailrocks/velnor-apt`). Org-level
  scale set: admin-plane APIs (`CreateRunnerScaleSet`, sessions, `acquirejobs`) are
  org-scoped; matches upstream `config.go` org/repo/enterprise URL parsing.
- **Canary repo (new, dedicated):** `tailrocks/velnor-d1-canary`. Contains ONLY the
  canary workflows (§3). No production workflows, no secrets, branch protection on
  `main`, no fork PRs enabled. Rationale: blast-radius isolation; runner-group
  "selected repositories" allowlist = this repo only; canary traffic never mixes
  with real CI.
- **Runner group:** `velnor-d1-canary-group`, visibility "selected repositories" =
  `velnor-d1-canary` only. Verifies spec §6 "verified runner-group/workflow
  restrictions where available".
- **Scale set:** `velnor-d1-canary`, in that group. Labels: `["self-hosted",
  "velnor-d1-canary"]` (unique suffix defeats label-spoofing cross-talk; no other
  set or static runner may carry `velnor-d1-canary`).
- **Bastion side:** ONE adapter instance, host-wide `N` small (N=2 recommended:
  enough to observe queueing/oldest-first with 3 queued jobs, small enough to keep
  races observable). Single session per set (matches upstream listener: one
  `MessageSessionClient`, `Scale` never concurrent).
- **Prohibited:** reusing any production set/group/label; repo-level set attached to
  a production repo; `latest` images anywhere in the canary path.

## 2. Credential / token requirements

### 2.1 GitHub App (required for the real canary; PAT is dev-bringup only)

| Use | Mechanism | Minimal scope | Stored where |
|---|---|---|---|
| Scale-set admin plane (CRUD set, sessions, `GenerateJitRunnerConfig`, `GetRunner`) | GitHub App, installed on org `tailrocks` | App permissions: **Administration R/W** (scale-set/runner-group management) + **Actions R** (read workflow/job metadata for trust inputs); installed on org, used only against the canary set | App private key in host management credential provider (`/etc/velnor`, mode 0600, root/velnor-mgmt only). NEVER in jobs, images, fixtures, journal, or logs |
| JWT minting (App → installation token) | in-process provider (upstream `jwt_provider.go` equivalent; KMS/HSM later) | — | signed in-process; key never leaves provider |
| Admin-token chain (`getRunnerRegistrationToken` → `getActionsServiceAdminConnection` → `fetchAccessToken`) | `client.rs` | — | `actionsServiceAdminTokenSnapshot` in memory only; refresh before `actionsServiceAdminTokenExpiresAt`; fingerprint (SHA-256 prefix) in logs, never the token |
| Queue token (`MessageQueueAccessToken`) | `session.rs` (`createMessageSession` → PATCH-refresh on 401 → `Close`) | — | in-memory in `MessageSessionClient` only; journal + `scaleset_sessions` store URL/token **hashes**, never raw values (d1-design §2 redaction rule) |
| JIT config (`EncodedJITConfig`) | `GenerateJitRunnerConfig` per acquired job | — | injected into the official-runner container only (env/file); fingerprint in journal (`jit_fingerprint`); never on disk outside the owned container, never in fixtures |
| PAT fallback (local dev bringup ONLY, never the graded canary) | classic PAT or fine-grained PAT | classic: `admin:org` on `tailrocks`; fine-grained: Administration R/W + Actions R on org | same provider path as App key; canary harness FAILS CLOSED if PAT auth is detected in a graded run |

Spec §6 rules enforced by harness assertions: App keys / admin tokens / signing
keys / host SSH keys / telemetry-write creds never appear in a job (assert via
container-env + filesystem scan of the runner container); tokens refresh in the
running provider (assert: 401 → PATCH refresh → retry succeeds with NO process
restart and NO EnvironmentFile rewrite); only ephemeral runner/session material
reaches official runners.

### 2.2 What the canary operator provisions (one-time, manual, audited)

1. Create App `velnor-d1-canary` (org-owned), permissions per §2.1, installed on
   `tailrocks`; record App ID + installation ID + key fingerprint (not key).
2. Create repo `tailrocks/velnor-d1-canary` (+ push §3 workflows), group, set.
3. Install App key + App/installation IDs into bastion provider; verify provider
   reports "App auth, key fingerprint …" with zero secret bytes logged.
4. Record all IDs in the canary run log header (set ID, group, repo ID, App ID,
   upstream pin `fb563005`, image digests).

## 3. Canary workflow shape (repo side + harness side)

### 3.1 Repo workflows (`tailrocks/velnor-d1-canary`)

`canary-min.yml` (`workflow_dispatch` + `push` to `canary/**`, concurrency cancelled
per-run — never cancels siblings):

```yaml
jobs:
  probe:
    runs-on: [self-hosted, velnor-d1-canary]
    steps:
      - run: |
          echo "canary-ok ${{ github.run_id }} ${{ github.run_attempt }}"
          hostname; cat /etc/os-release | head -2; docker info --format '{{.ServerVersion}}'
```

`canary-fanout.yml`: matrix of 3 trivial jobs (forces queue depth 3 > N=2 →
exercises oldest-first + deferral + re-offer). `canary-cancel.yml`: job with a
30s sleep the harness cancels mid-run (exercises `JobCompleted{result=cancelled}`
+ terminal path). All jobs: no secrets, no `pull_request_target`, pinned action
SHAs only (no third-party actions at all in v1).

### 3.2 Harness run shape (bastion side, one graded pass)

```text
SETUP   record IDs/digests/pins → open session (assert §4.1) → drain set to idle
        (TotalAssignedJobs==0, no live workers) → snapshot journal cursor
OFFER   dispatch canary-min (1 job) → expect JobAvailable → oldest-first grant →
        permit reserve → acquire-intent journaled
ACQUIRE POST acquirejobs → assert returned subset recorded exactly (§4.4) →
        GenerateJitRunnerConfig → provision-intent (stable op/ownership IDs)
RUN     DinD ready → runner connected → JobAssigned/Started observed → job green →
        JobCompleted observed → diagnostic export → owned cleanup → permit released
ACK     every consumed message DELETE-ACKed only after journal commit (§4.6);
        lastMessageID advanced exactly-once per ACKed message (§4.1/§4.6)
FANOUT  dispatch canary-fanout (3 jobs, N=2) → assert deferral keeps age, re-offer
        after capacity frees, FIFO-by-(first_seen_at,sequence) completion order
FAULTS  injected series (§4.9 + §5): cancel, 401-refresh, redelivery, restart-crash
CLEANUP §6 guarantees → verdict (all assertions green + no secret leak + N never
        exceeded + journal/store consistent)
```

Each phase emits a signed run-log bundle: dispatched run IDs, adapter journal
events, poll/ACK/acquire HTTP traces (tokens redacted, fingerprints kept), Docker
ownership records, verdict per assertion ID. The bundle doubles as the
recorded/sanitized fixture source (spec §5.1: fixtures AND live canary).

## 4. Conformance assertion list (mapped to the §5.1 9-step order)

IDs `C1.x…C9.x`. Every assertion states stimulus → observable on the REAL wire.

### Step 1 — idempotent observations + durable occupied state (C1)

- **C1.1 initial poll:** fresh session, `lastMessageID=0/unset` → first `GET` carries
  no `lastMessageId` query (upstream: query only when `> 0`); synthetic
  `InitialMessageID=-1` stats-only message processed without acquire/ACK side
  effects; `ScaleSetSessionOpened` + initial `ScaleSetStatsObserved` journaled.
- **C1.2 nil poll:** quiet set → long poll returns **202** → `(None, None)`, NO ACK
  attempted, `lastMessageID` unchanged, idle reconcile runs (→ C9.1).
- **C1.3 Assigned/Started/Completed fold:** after ACQUIRE, `JobAssigned` then
  `JobStarted{runnerId,runnerName}` then `JobCompleted{result}` each produce exactly
  one `ScaleSetWorkerEdge`; occupied state keyed by `runnerRequestId` matches wire.
- **C1.4 replay idempotence:** redelivered `JobCompleted` (same `message_id`,
  §4.6 setup) → terminal replay is a journaled no-op (via `is_terminal` +
  `transition_target`); no second diagnostic export, no double permit release.
- **C1.5 cancel:** cancel mid-run → `JobCompleted{result=cancelled}` drives worker
  `running → terminal → diagnostic export → owned cleanup → permit released`;
  assert no `retained_failed` leak on the happy path.
- **C1.6 unknown event:** unknown `messageType`/shape (real or post-restart
  surprise) → `ScaleSetMessageSeen` recorded, `reconcile::unknown_event()`, NO
  panic, NO ACK-skip of the enclosing batch's other effects (message still ACKed
  only via the normal §4.6 rule).

### Step 2 — oldest-observed queue + trust-before-grant (C2)

- **C2.1 offer insert:** each `JobAvailable` inserts one `scaleset_demand` row keyed
  by `request_id` with immutable `first_seen_at` + journal-counter `sequence`.
- **C2.2 redelivery keeps age:** redelivered offer (same `request_id`) does NOT move
  `first_seen_at`/`sequence` (assert pre/post values equal).
- **C2.3 oldest-first grant:** fanout 3 jobs, N=2 → grants complete strictly in
  `(first_seen_at, sequence)` order; assert via `ScaleSetOfferGranted` order +
  job start order on wire.
- **C2.4 trust gate:** offer for canary repo passes `TrustClass::derive` →
  `AdmittedTrust::admit`; a synthetic ineligible offer (wrong labels/scope
  replayed through `demand::submit_offers` against a fixture, then confirmed live
  by dispatching a job WITHOUT the canary label → never offered to our set)
  yields `ScaleSetOfferDeclined{reason}`, never a blind grant.
- **C2.5 deferred-offer validity:** while capacity is full, the 3rd offer stays
  `eligible` with preserved age across polls; assert it is NOT dropped, NOT
  declined, and NOT re-aged when its message is ACKed (ACK covers the transport
  batch, not the demand row — the "never acknowledge away the only reference to
  deferred work" proof: demand row survives ACK, re-offered next poll).
- **C2.6 generation fencing:** grant issued under generation G is rejected on use
  after a restart bumps to G+1 (assert: stale grant → re-grant, never double
  spend; `ScaleSetOfferGranted{generation}` matches current).

### Step 3 — permit reserve + acquire-intent before AcquireJobs (C3)

- **C3.1 ledger cap:** at no instant do non-`released` `job_permits` rows exceed N
  (assert by journal replay + live `COUNT(*)` sampling through fanout).
- **C3.2 intent-before-HTTP:** `ScaleSetAcquireIntended{batch_id,request_ids,
  permit_ids}` journal commit timestamp strictly precedes the `POST acquirejobs`
  request (assert from correlated journal + HTTP trace).
- **C3.3 exhaustion queues, not drops:** 3rd job with N=2 full → `CapacityExhausted`
  internally, offer keeps age and queue position (→ C2.5), no failed run on GitHub
  side caused by us.
- **C3.4 cross-lane sharing:** (when native lane active) native + scaleset permits
  counted in ONE ledger; pre-registered native idle must not starve the canary
  (spec §4.1; assert canary job starts within T while native idle slots exist).

### Step 4 — returned-ID subset + partial/uncertain reconcile (C4)

- **C4.1 exact-subset recording:** `acquireJobsResponse{count,value}` →
  `ScaleSetAcquireResolved{acquired_ids == resp.value ∩ requested,
  missing_ids == requested − resp.value}`; assert `acquired ∪ missing ==
  requested` and `acquired ∩ missing == ∅`. Never assume all succeeded.
- **C4.2 missing path:** missing IDs → permits released via the single release path,
  demand rows back to `eligible` with PRESERVED age (re-offerable, not re-aged).
- **C4.3 uncertain path:** forced transport error/timeout AFTER send (harness kills
  the poll TCP flow once, or proxy-injects a 502 on first attempt) →
  `uncertain=true`, permits stay `acquiring` (still counted toward N), demand
  `uncertain`; resolution arrives via later `JobAssigned` observation or bounded
  idle reconcile (`GetRunner` oracle), never by re-spending the permit.
- **C4.4 no double-spend:** across C4.2/C4.3 + redelivery, each `request_id` maps to
  ≤1 live permit at all times (journal-replay invariant).

### Step 5 — JIT/provision intent + idempotent creation (C5)

- **C5.1 provision-intent first:** `ScaleSetProvisionIntended{operation_id,
  ownership_id, runner_name, digests, jit_fingerprint}` committed before ANY
  Docker call (journal-vs-Docker-event timestamp order).
- **C5.2 stable IDs:** `operation_id=f(set,request,attempt)`,
  `ownership_id=f(set,runner_name)`, `runner_name=<set>-<journal-seq>` — kill the
  adapter between intent and `docker create`, restart → provision resumes with
  IDENTICAL IDs (assert no duplicate runner/DinD pair, existing containers
  adopted by ownership label).
- **C5.3 JIT least-privilege:** `EncodedJITConfig` reaches ONLY the official-runner
  container; assert App key/admin token/queue token absent from runner env,
  filesystem, and job logs (§2.1 scan).
- **C5.4 digest pins:** runner + DinD images created with validated digests (never
  `latest`); runner version recorded separately from Velnor version; assert
  `docker inspect` digests == pinned.

### Step 6 — durable ACK + offer-validity/session boundaries (C6)

- **C6.1 ACK-after-success:** `DELETE {MessageQueueURL
...[truncated 6262 chars]
