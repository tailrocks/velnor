# Gate E adversarial fault-matrix procedures (DESIGN-ONLY, not executed)

Status: **future procedure design** for campaign gate E (work-plan E1/E2). Read-only prep; no bastion
touches, no execution. All commands below are PLANNED — to be run at execution time by the
infrastructure subagent (bastion) and the CI canary jobs (GitHub), witnessed by the named verifiers.

Sources (read-only):
- `plans/bastion-three-provider-ci/spec.md` §8 (fault contract, 11 table rows) + §2, §4, §5, §6, §7 context
- `plans/bastion-three-provider-ci/work-plan.md` STEP E1 (Velnor full qualification) + STEP E2 (fault matrix)
- `plans/bastion-three-provider-ci/checklist.md` E2 acceptance line

Conventions used in every row:
- **Where**: `bastion-shell` = SSH session on `root@37.27.110.241` (infra subagent window);
  `ci-canary` = step/job inside an identified canary workflow run on GitHub (hosted or local lane as stated);
  `github-api` = `gh api` calls from an operator shell (read-only unless the step says mutate).
- **Canary**: an identified disposable fixture workflow + repo (see OQ-1) — never production Velnor CI jobs.
- `velnorctl …` verbs are NOT invented here: spec §7 requires deriving them from implemented package/help
  at execution. Placeholders are marked `velnorctl <TBD-verb>`.
- Observation channels assumed from spec §8: Velnor journal (`/var/lib/velnor`), outbound health records,
  hosted verifier aggregate (`ci-required`), Docker inspect, cgroupfs, `gh api` run/job metadata, JUnit/test
  reports, cache/timing reports, cleanup receipts. Exact schemas are open questions where the spec does
  not define them.

Global preconditions (before any row runs):
- P0. E1 complete: full unit×provider matrix recorded; D3 package installed; provisional/mixed N set.
- P1. Canary workflow(s) identified and labeled in ledger; canary scope/runner-group/selector names recorded.
- P2. Baseline snapshot: `N`, permit-ledger dump, `docker ps`, journal checkpoint, hosted health-record
  sequence number — recorded per row so "released exactly once / reconciled" is checkable.
- P3. Shared-Docker-restart / host-reboot rows run only in an isolated coordinated window (spec §8):
  unrelated work drained or preserved, SSH session kept, ledger announces window start/end.

Global rollback/cleanup rule (applies to every row unless the row says otherwise):
- Re-enable normal admission after the row; drain canary leftovers; confirm `docker ps` shows no canary
  objects, ledger shows occupied back to pre-row baseline, hosted verifier for the canary run shows the
  EXPECTED terminal state (fail where the row demands fail); record row evidence in ledger.

---

## R1 — Kill official runner / DinD / native job worker (spec §8 row 1)

Spec: "Kill official runner or DinD; kill native job worker" → no false success; diagnostic export;
only owned resources removed; capacity released once after cleanup; next job clean.
Verifier: Docker/lifecycle verifier.

Subcases (each lifecycle point is a separate injection):
- R1a. Official lane, runner connected but job not yet started (idle-assignable / just-assigned).
- R1b. Official lane, job running (steps executing in runner container).
- R1c. Official lane, DinD killed while runner alive (job running).
- R1d. Official lane, runner killed while DinD alive (job running).
- R1e. Official lane, both runner + DinD killed simultaneously.
- R1f. Native lane, native job worker killed while job running.
- R1g. Either lane, kill during terminal diagnostic-export / owned-cleanup phase.

Injection (bastion-shell in every subcase; identify canary-owned container IDs first):
```sh
# Identify canary-owned objects (ownership identity per spec §5.3; exact label TBD at execution, see OQ-2)
docker ps --filter "label=<TBD-canary-run-id>" --format '{{.ID}} {{.Names}} {{.Labels}}'
# Kill variants (one per subcase run):
docker kill <runner-container-id>        # R1a/R1b-partial/R1d
docker kill <dind-container-id>          # R1c
docker kill <runner-id> <dind-id>        # R1e
docker kill <native-worker-container-id> # R1f
# For R1g: kill -9 the Velnor-spawned cleanup helper only after terminal state observed in journal;
# never kill Velnor itself here (that is R2).
```
Where: bastion-shell (infra subagent). Trigger job: ci-canary (start canary run on the targeted lane,
wait for the desired lifecycle point via journal/health-record observation, then inject).

Invariant: canary run terminal state is failure/incomplete, never success; diagnostics exported before
ephemeral removal; ONLY canary-owned objects removed (sibling canary or production objects untouched);
permit released exactly once after cleanup confirmed; next canary job on same lane starts clean
(no poisoned workspace/cache/network).

Observe pass/fail:
- PASS: hosted verifier aggregate for canary run = fail/incomplete; journal shows terminal state +
  diagnostic-export record + cleanup receipt + single permit-release ledger transition
  (occupied count returns to pre-row baseline, never below); `docker ps -a` + `docker volume ls` +
  `docker network ls` show zero canary-owned leftovers; follow-up canary job green-on-purpose (or its
  expected outcome) with fresh workspace.
- FAIL: any success verdict on the killed canary; missing diagnostic export; non-owned object removed;
  ledger shows double-release (below baseline) or leak (above baseline after cleanup); next job inherits
  stale state.

Rollback/cleanup: remove any canary-owned leftovers by explicit ID (never `docker system prune`,
never broad prune); if ledger leaked, reconcile via implemented recovery path (TBD verb) and record as
row failure, not silent fix; re-run one clean canary to prove lane healthy.

## R2 — Kill Velnor / restart at every lifecycle boundary (spec §8 row 2)

Spec: "Kill Velnor; restart before/after acquire, JIT, Docker create/start, running, completion or ACK" →
durable reservations reconciled; active containers adopted or explicitly failed; no duplicate execution,
lost job, or leaked permit. Verifier: protocol + recovery verifiers.

Subcases (restart injection point; one canary run per point):
- R2a. After permit reserved + acquisition intent persisted, before `AcquireJobs` call.
- R2b. After `AcquireJobs` returned, before JIT/provisioning intent persisted (uncertain-acquire window).
- R2c. After JIT intent persisted, before/after Docker create, before container start.
- R2d. After runner connected / job running.
- R2e. After job completion observed, before acknowledgement persisted/sent (lost-ACK window).
- R2f. After ACK, before diagnostic export / cleanup / permit release.

Injection (bastion-shell):
```sh
# Hard kill (crash simulation):
systemctl kill -s KILL <velnor-service-unit>   # unit name from package; see OQ-3
# then restart via the implemented activation path:
systemctl start <velnor-service-unit>
# and wait for reconcile-before-advertise (spec §4.1) to complete before judging.
```
Timing control: watcher loop on bastion-shell polls journal/health records for the target lifecycle
marker, then kills within the window. For R2b/R2e (narrow windows) the procedure may need an
execution-time synchronization hook (see OQ-4); if the window cannot be hit deterministically, record
attempt counts and which windows were actually covered.
Where: bastion-shell for kill/restart; ci-canary provides the in-flight job; github-api observes
GitHub-side assignment/ACK state.

Invariant: after restart, durable reservations reconciled (occupied ≤ N, no reset-to-zero semaphore);
in-flight containers adopted (resume supervision) or explicitly failed with cleanup — never lost, never
false-success; no duplicate local execution from replayed assignment; no leaked permit; scale-set
identity preserved (no set deletion on normal restart).

Observe pass/fail:
- PASS: journal + ledger after restart show reconcile pass, every pre-kill reservation accounted
  (adopted or failed-with-cleanup); GitHub run/job metadata (all pages, attempts distinguished) matches
  local terminal states; no duplicate job execution evidence (single runner/JIT identity per acquired
  request ID); ledger occupied returns to correct value; permit count monotonic ≤ N throughout.
- FAIL: lost job (GitHub says assigned, local state has no record); false success; duplicate execution;
  leaked permit; set deleted/recreated; capacity advertised before reconcile completed.

Rollback/cleanup: explicitly fail + clean up any canary work left in uncertain state via implemented
recovery path; verify no orphan containers (cross-check R8); re-run clean canary to prove recovery.

## R3 — Redelivery / duplicate / reorder / partial-uncertain AcquireJobs (spec §8 row 3)

Spec: "Redeliver/duplicate/reorder events; return partial/uncertain AcquireJobs responses" →
deduplication + correct returned-ID handling; no statistics double-count; first-seen age retained.
Verifier: protocol verifier.

Subcases:
- R3a. Duplicate `JobAvailable` / session message redelivery (same message ID twice).
- R3b. Reordered messages (completion observation arrives before the corresponding acquire record).
- R3c. `AcquireJobs` returns FEWER request IDs than asked (partial grant).
- R3d. `AcquireJobs` response lost/uncertain (timeout after server-side grant — must reconcile, not assume).
- R3e. Redelivered demand for already-granted work retains ORIGINAL `first_seen_at` age (no queue jump).

Injection: this row cannot be injected with plain shell commands against the real upstream API — the
injection harness is implementation-defined (see OQ-5). Planned options, decided at execution:
  (a) recorded/sanitized protocol fixture replay against a test Velnor instance (D1 fixtures);
  (b) a transparent test proxy between Velnor and upstream that duplicates/reorders/drops/truncates
      responses for canary scope traffic only;
  (c) live-canary double-poll using two sessions (only if upstream semantics allow without mutating
      unrelated scopes).
Where: bastion-shell (fixture replay or proxy control) + ci-canary (demand generator); github-api to
confirm upstream-side truth (actual grants) vs local belief.

Invariant: every message applied idempotently; ONLY actually returned IDs treated as acquired (never
assume all requested succeeded, never spend one permit twice); `Statistics.TotalAssignedJobs` used for
convergence, never capped-batch counts, never statistics+reservations double-count; redelivered demand
keeps original first-seen age/sequence.

Observe pass/fail:
- PASS: ledger shows exactly one grant per upstream-granted ID; no phantom acquisition for unreturned
  IDs; queue order by original `first_seen_at` preserved across redelivery (younger canary job never
  granted before older eligible canary job where Velnor controls the choice); no double-count in
  population convergence (local occupied + upstream stats consistent).
- FAIL: phantom permit spend; double grant for one ID; age reset on redelivery causing overtake;
  convergence computed from capped batches.

Rollback/cleanup: stop proxy/replay; reconcile ledger against upstream `TotalAssignedJobs`; explicitly
withdraw phantom grants if any (record as row failure); clean-canary re-run.

## R4 — Cancel queued / running work incl. upstream reassignment (spec §8 row 4)

Spec: "Cancel queued or running work, including GitHub upstream reassignment" → correct
terminal/withdrawn state; no new runner for stale demand; permit/accounting correct.
Verifier: queue verifier.

Subcases:
- R4a. Cancel queued (offered, not yet acquired) canary job via GitHub UI/API.
- R4b. Cancel running canary job (official lane) via GitHub UI/API.
- R4c. Cancel running canary job (native lane) via GitHub UI/API.
- R4d. Upstream reassignment: GitHub reassigns/withdraws an acquired request (e.g. runner timeout,
  set rebalance) — observe Velnor handling of the withdrawn assignment.
- R4e. Cancel during provisioning (acquired, runner not yet connected).

Injection:
```sh
# github-api / operator: cancel the canary run (attempt-scoped):
gh api -X POST repos/<owner>/<canary-repo>/actions/runs/<run-id>/cancel
# or cancel a single job attempt (force-cancel if needed):
gh api -X POST repos/<owner>/<canary-repo>/actions/jobs/<job-id>/cancel
# R4d: reassignment is upstream behavior — trigger per upstream semantics if a deterministic trigger
# exists (see OQ-6); otherwise observe opportunistically and record.
```
Where: github-api (or web UI) for the cancel action; bastion-shell observes journal/ledger/Docker;
ci-canary is the victim job.

Invariant: cancelled work reaches correct terminal/withdrawn state locally; NO new runner provisioned
for stale/withdrawn demand after cancellation observed; permit released exactly once; no false success;
rerun (if any) revalidates identity/provenance as a new attempt.

Observe pass/fail:
- PASS: journal shows withdrawn/cancelled terminal state; no Docker create for the stale demand after
  cancel timestamp; ledger occupied returns to baseline; hosted aggregate reflects cancelled (and the
  required-result contract treats it per spec §2 — cancelled expected result cannot pass); GitHub-side
  and local states agree (all pages, attempts distinguished).
- FAIL: runner provisioned after cancel; permit leaked or double-released; stale demand executed;
  local state says success/running while GitHub says cancelled (or vice versa) after reconcile window.

Rollback/cleanup: owned cleanup of any partially provisioned canary runner; ledger reconcile; clean-canary re-run.

## R5 — Scope/engine race for final permits (spec §8 row 5)

Spec: "Two scopes and both engines race for final permits" → total occupied ≤ N; no independent N per
scope; old eligible work not locally overtaken; no permanent idle-slot starvation.
Verifier: capacity verifier.

Subcases:
- R5a. Two GitHub scopes (e.g. two repos or two runner groups — see OQ-1) submit demand simultaneously
  with only 1 free permit.
- R5b. Native + Scale Set lanes both have eligible demand with only 1 free permit.
- R5c. Sustained overload: more eligible demand than N across scopes/engines; verify oldest-observed
  order and no idle-slot starvation of the official lane over a bounded window.

Injection (no kill needed — load generation):
- ci-canary: enqueue K > N canary jobs nearly simultaneously across the two scopes and both lanes
  (e.g. `gh workflow run` fan-out, or push N+4 canary commits/branches per prior E1 pattern).
- bastion-shell: observe only; optionally set N to a small test value for the window IF the package
  supports safe N change + restore (see OQ-7); otherwise race at production N with K = N+oversubscribe.
Where: ci-canary (demand), bastion-shell (ledger observation), github-api (delivery observation).

Invariant: total occupied (reserved+acquiring+provisioning+assignable+running+cleaning+uncertain, all
scopes, both engines) ≤ N at every sample (monotonic ledger); no per-scope/per-engine independent N;
oldest-observed eligible demand granted first where Velnor controls the choice; pre-registered native
idle slots do not starve the official lane indefinitely.

Observe pass/fail:
- PASS: sampled occupied-total time series never exceeds N; grant order log respects oldest-observed
  among Velnor-controlled grants; both lanes make progress over the window (no starvation); offered-but-
  unacquired demand counted as queue, not occupancy.
- FAIL: any sample > N; younger grant before older eligible (Velnor-controlled); one lane starved for
  the whole window while holding idle assignable capacity.

Rollback/cleanup: let queued canaries drain or cancel them (R4 procedure); restore N if changed; verify
ledger back to baseline.

## R6 — Docker restart / network loss during poll/acquire/ACK/refresh (spec §8 row 6)

Spec: "Docker daemon restart or network loss during poll/acquire/ACK/refresh" → visible degraded state,
no unsafe new acquisition, bounded retry, recovery without orphan/identity confusion.
Verifier: infrastructure/recovery verifier.

Subcases (each phase × each fault = separate runs; combine only if ledger proves coverage):
- Phases: P1 long-poll wait, P2 `AcquireJobs` call, P3 ACK send, P4 App/token refresh, P5 Docker
  create/start of runner or DinD.
- Faults: F-a Docker daemon restart (`systemctl restart docker`); F-b network loss to GitHub API
  (DROP egress to API hosts); F-c network loss to GHES/proxy if applicable (likely N/A — record).
- R6c extra: full host reboot (spec §8 coordinated window) — one run, maximum caution.

Injection (bastion-shell; coordinated window REQUIRED for F-a and reboot):
```sh
# F-a Docker restart:
systemctl restart docker
# F-b network loss (bounded, canary-window only; exact hosts from implemented config — no frozen
# three-host allowlist assumption per spec §6; restore immediately after the phase under test):
iptables -A OUTPUT -d <github-api-host-or-cidr> -j DROP   # record exact rule for removal
# ... observe degraded behavior for the bounded window ...
iptables -D OUTPUT -d <github-api-host-or-cidr> -j DROP   # restore
# R6c host reboot (isolated coordinated window, unrelated work preserved, SSH kept):
systemctl reboot
```
Where: bastion-shell for fault + observation; ci-canary provides in-flight demand at the target phase
(phase targeting via journal watcher as in R2).

Invariant: fault produces a VISIBLE degraded state (health records / status show degraded, not silent);
no unsafe new acquisition during uncertainty; retries bounded with backoff/rate-limit handling; after
recovery, no orphan containers, no identity confusion (container ↔ request-ID mapping intact), permits
reconciled.

Observe pass/fail:
- PASS: degraded state visible in health records/logs within the row's observation window; zero new
  grants during the fault window (ledger grant log); retry cadence bounded (no hot loop — check call
  counts); post-recovery reconcile clean: orphans adopted or removed by ownership, ledger correct,
  GitHub-side and local states agree.
- FAIL: silent hang with healthy status; new acquisition during uncertainty; unbounded retry storm;
  orphaned containers after recovery; request-ID ↔ container mismatch.

Rollback/cleanup: remove DROP rules (verify `iptables -S` clean); confirm Docker healthy
(`docker info`, daemon API responsive); reconcile + clean-canary re-run; for reboot: full post-boot
health check (units, mounts, sockets, ledger reconcile-before-advertise) before judging.

## R7 — Bad digest / manifest / signer / ref / key / package / arch / record (spec §8 row 7)

Spec: "Incorrect runtime digest, manifest, signer/ref or APT key/package/architecture/record" →
rejected before execution/publication/install; no insecure fallback; previous trusted state intact.
Verifier: supply-chain verifier.

Subcases (each rejected artifact is a separate negative test):
- R7a. Runtime product: wrong binary digest (candidate C tampered after attestation).
- R7b. Runtime product: wrong manifest (manifest lists digests that do not match binaries).
- R7c. Runtime product: untrusted signer workflow/ref (artifact from PR fork or wrong workflow).
- R7d. Runtime product: consumer-repo artifact lookup confusion (same name, wrong producer repo).
- R7e. Runtime product: PR-provided checksum offered as trust (must be ignored).
- R7f. APT: wrong repository signing key / fingerprint mismatch.
- R7g. APT: package hash mismatch vs signed metadata (tampered .deb).
- R7h. APT: wrong architecture payload served (e.g. arm64 bytes under amd64 entry).
- R7i. APT: incoherent release record (manifest/record entries missing or mismatched).
- R7j. Daemon package selected via unqualified `releases/latest` (must be impossible in code — static
  + behavioral proof).

Injection:
- R7a–R7e: ci-canary — craft candidate fixtures (tampered copy in a DISPOSABLE test tree / test
  producer, never the trusted feed) and attempt consumer resolution; exact fixture construction follows
  the A2 negative-test suite, re-executed at E2 against the shipped product.
- R7f–R7i: bastion-shell (APT client-side) + disposable staging feed: present tampered metadata/package
  from a test repository URL or staged index; attempt `apt-get update` / install of the bad candidate;
  verify rejection. NEVER mutate the production feed (`velnor-apt.tailrocks.com`) for negative tests.
- R7j: source audit (no `releases/latest` consumer path) + behavioral test (rename/remove pinned
  identity → precise producer-defect error, no fallback fetch).
Where: ci-canary for runtime-product negatives; bastion-shell for APT client-side negatives (test feed
only); repo worktree (read-only audit) for R7j static part.

Invariant: every bad artifact rejected BEFORE execution / publication / install; no insecure fallback
(no source-build fallback in consumer, no `--allow-unauthenticated`, no `dpkg -i` bypass); previous
trusted state (installed package, trusted pin R) intact and still functional.

Observe pass/fail:
- PASS: each subcase fails with a PRECISE error naming the defect (digest/signer/arch/…); consumer
  logs show zero cargo/source-build invocations; APT never installs the bad candidate
  (`dpkg-query` identity unchanged); trusted feed + installed package verify clean after the row.
- FAIL: any bad artifact accepted, executed, published, or installed; vague error that could mask the
  defect; fallback build/install path taken; trusted state clobbered.

Rollback/cleanup: remove test feed entries and disposable trees; `apt-get update` against the trusted
feed; re-verify installed identity (`dpkg-query`, `release verify-installed`); clean-canary re-run.

## R8 — Orphans / partial deletion / unknown events / stale generation (spec §8 row 8)

Spec: "Orphans, partial deletion, unknown lifecycle events, stale control generation" → reconcile
ownership; do not panic or broad-prune; unreconciled state never counts as free.
Verifier: lifecycle verifier.

Subcases:
- R8a. Orphan runner container: canary runner container exists in Docker but its journal/ledger record
  was never persisted (simulate by pausing Velnor mid-provision — see OQ-4 — or by crafting the state
  via the test harness, OQ-5).
- R8b. Orphan DinD data dir / volume / network after runner record deleted.
- R8c. Partial deletion: half of a canary job's objects removed externally
  (`docker rm` of runner but not DinD, or vice versa), then reconcile.
- R8d. Unknown lifecycle event: inject an event with an unknown type/version (fixture replay or proxy,
  as R3) and restart Velnor over it.
- R8e. Stale control generation: submit a mutation (cancel, grant, ACK retry) carrying an old
  generation/fencing token after a newer generation is active.

Injection (bastion-shell):
```sh
# R8b/R8c partial-deletion simulation (canary-owned IDs only):
docker rm -f <canary-runner-id>        # leave DinD + volumes + network behind (or the reverse)
docker network ls --filter "label=<TBD-canary-run-id>"
docker volume ls --filter "label=<TBD-canary-run-id>"
# R8a/R8d/R8e: harness-defined (fixture replay / proxy / generation rollback) — see OQ-4/OQ-5.
```
Where: bastion-shell for Docker-object manipulation; harness (bastion-shell sidecar) for event/
generation injection; ci-canary supplies the original demand.

Invariant: reconciler resolves every object by ownership identity; Velnor never panics on unknown
events; never broad-prunes (no `docker system prune`, no unowned-object deletion); unreconciled/
uncertain state NEVER counted as free capacity (stays reserved/uncertain until proven terminal).

Observe pass/fail:
- PASS: post-reconcile inventory shows orphans adopted-or-removed by ownership, partial sets completed
  (remaining halves cleaned, receipts written), unknown events logged + skipped without crash,
  stale-generation mutations rejected with explicit fencing error; ledger occupied never dips for
  unreconciled objects (before/after ledger diff proves it).
- FAIL: panic/crash; any unowned object removed; unreconciled object counted as free (ledger dip);
  orphan left behind after reconcile window; stale mutation applied.

Rollback/cleanup: remove remaining canary-owned objects by explicit ID; reconcile; clean-canary re-run.

## R9 — Fork spoofing / protected-workflow input substitution (spec §8 row 9)

Spec: "Fork selector spoofing or protected-workflow input substitution" → no bastion execution;
hosted-only trust exclusion explicit. Verifier: trust verifier.

Subcases:
- R9a. Fork PR crafts `runs-on` / selector labels matching bastion's dedicated local selectors —
  must NOT route to bastion.
- R9b. Fork PR calls a reusable workflow with substituted inputs attempting to escalate to a
  protected (privileged / `pull_request_target`) path.
- R9c. Same-repo PR + bot PR + schedule + dispatch + tag + merge-group: each event type either
  explicitly allowed or explicitly excluded per the trust matrix (spec §6); at least one excluded
  event per type attempted against bastion routing.
- R9d. Untrusted checkout executed under privileged `pull_request_target` — must be impossible
  (negative test).
- R9e. Default fork execution lands hosted-only (positive control: fork canary DOES run on
  `github-hosted`, proving the exclusion is routing, not breakage).

Injection (ci-canary + github-api; bastion-shell observes only):
- From a fork of the canary repo (untrusted identity), open PRs carrying: spoofed selector labels
  (R9a), malicious reusable-workflow inputs (R9b), one run per event type (R9c), a `pull_request_target`
  job with untrusted checkout (R9d), and a plain fork job (R9e positive control).
- Exact payload shapes depend on the implemented selector/policy schema (D2) — see OQ-8.
Where: ci-canary (fork side) for attack payloads; github-api to enumerate resulting runs/jobs;
bastion-shell to prove ABSENCE of bastion execution (no journal entries, no containers, no permits).

Invariant: zero bastion execution for any spoofed/substituted/excluded case; every exclusion explicit
in controller-side checks (outside PR-editable YAML) with a recorded reason; fork default stays hosted.

Observe pass/fail:
- PASS: for R9a–R9d, GitHub job metadata shows no job placed on bastion lanes AND bastion journal/
  ledger/Docker show zero corresponding activity; controller logs/policy decisions record explicit
  exclusion reasons; R9e fork job greens on `github-hosted`.
- FAIL: any bastion-side container, permit, or journal entry attributable to an attack run; exclusion
  enforced only by PR-editable YAML (bypassable); silent drop with no explicit reason; R9e broken
  (cannot distinguish denial from outage).

Rollback/cleanup: close attack PRs; cancel any hosted-side attack runs; verify ledger/Docker untouched;
record trust-denial proof in ledger.

## R10 — Missing / skipped / wrong-provider / stale reports (spec §8 row 10)

Spec: "Missing/skipped/wrong-provider test report or stale run attempt" → required aggregate fails,
even with remaining providers green. Verifier: result verifier.

Subcases (one canary run per subcase; provider under test varies across the three):
- R10a. One provider's unit result MISSING (job never reported — simulate by cancelling that lane's
  job post-start, or by fixture omission in a test aggregate — see OQ-9).
- R10b. One provider's unit SKIPPED (conditional skip in canary workflow for one lane).
- R10c. WRONG-PROVIDER report: lane X's report submitted under lane Y's identity (fixture-level
  substitution; must be rejected as identity mismatch, never accepted as Y's green).
- R10d. STALE attempt: report from run attempt N presented for attempt N+1 (rerun scenario).
- R10e. Duplicate-conflicting: two different outcomes for the same result identity.
- R10f. Positive control: all three lanes report correctly → aggregate greens (proves the harness can
  pass).

Injection: ci-canary (canary workflow variants that skip/drop/mislabel one lane's report) + github-api
(inspect runs/attempts/artifacts/JUnit); fixture-level identity attacks (R10c–R10e) run against a test
aggregate harness if the production verifier cannot be fed synthetic reports (see OQ-9).
Where: ci-canary for report manipulation; github-api for run/attempt enumeration; bastion-shell NOT
involved except to confirm local lanes actually executed their share (no lane may be "proven" by
another lane's report).

Invariant: the required aggregate (hosted verifier, `ci-required` preserved) FAILS for every subcase
R10a–R10e even when the remaining providers are green; reruns revalidate exact identity + outcome
provenance; a later reporter/cancellation never overwrites an already-failed required result with
success.

Observe pass/fail:
- PASS: aggregate = fail/incomplete for R10a–R10e with the defect named (missing/skipped/identity-
  mismatch/stale/conflict + the exact result identity per spec §2); R10f aggregate = green; no
  failure→success overwrite observed on rerun/cancel races.
- FAIL: aggregate green on any of R10a–R10e; misattributed report accepted; stale attempt accepted;
  overwrite of failed result by later success.

Rollback/cleanup: none destructive — canary runs are metadata-only after completion; record aggregate
verdicts + run URLs in ledger.

## R11 — Hidden ancestor limits / injected build budgets (spec §8 row 11)

Spec: "Hidden ancestor limits or injected build budgets" → inspect real containers/cgroups and
environment; quota-free policy demonstrated on descendants. Verifier: resource verifier.

Subcases:
- R11a. Real native job container + descendants: HostConfig + cgroup ancestry + env clean.
- R11b. Real official runner + private DinD + nested job containers: all three levels clean.
- R11c. Package units/drop-ins: no quota directives anywhere in the workload ancestry.
- R11d. Negative control (fixture): a deliberately constrained test container IS detected by the
  inspection procedure (proves the inspector is not a tautology).

Injection: none destructive — this row is inspection of REAL E1/E2 job objects, plus one deliberate
fixture for R11d:
```sh
# R11a/R11b — run while a real canary job (each lane) is executing:
docker inspect <job-container-id> --format '{{json .HostConfig}}'   # expect no NanoCpus/CpuQuota/
  # CpusetCpus/Memory/MemorySwap/MemoryReservation/PidsLimit
cat /sys/fs/cgroup/system.slice/.../cpu.max /sys/fs/cgroup/.../memory.max \
    /sys/fs/cgroup/.../memory.high   # expect 'max' at every workload-ancestor level (see OQ-10)
docker exec <job-container-id> env | grep -Ei 'CARGO_BUILD_JOBS|MBX|GRADLE.*WORKER|WORKERS|HEAP|XMX'
  # expect no injected partitions (exact var list from implementation — see OQ-10)
# R11c:
systemctl cat <velnor-units...> | grep -Ei 'CPUQuota|MemoryMax|MemoryHigh'  # expect no match
ls /etc/systemd/system/*.d/  # expect no quota drop-ins; upgrades must not recreate them
# R11d — deliberate constrained fixture (proves detection works):
docker run --rm --cpus=0.5 --memory=256m --name quota-canary <img> true
# ... run the same inspection above against quota-canary, expect DETECTION (limits found) ...
docker rm -f quota-canary
```
Where: bastion-shell for all inspection; ci-canary provides the running jobs to inspect.

Invariant: quota-free execution proven for native jobs, official runners, DinD, and representative
nested descendants — Docker HostConfig, EFFECTIVE cgroup ancestry (inherited limits included, not only
emitted flags), package units/drop-ins, and effective build environment all clean; R11d proves the
inspection actually detects limits.

Observe pass/fail:
- PASS: inspection bundle (HostConfig JSON, cgroup file contents per ancestor level, unit files,
  env dumps) attached to ledger with zero quota findings on real jobs; R11d fixture correctly flagged
  as constrained.
- FAIL: any quota/ceiling/budget found on real workload ancestry; inspection misses the R11d fixture
  (inspector broken); only Docker flags checked without cgroup ancestry.

Rollback/cleanup: remove R11d fixture container; no system changes made by this row otherwise.

---

## Coverage map: work-plan E2 action-1 phrases → rows

| E2 phrase | Row(s) |
| --- | --- |
| kill runner/DinD/native worker/Velnor at each lifecycle point | R1 (a–g), R2 (a–f) |
| redelivery + partial AcquireJobs | R3 (a–e) |
| cancel queued/running incl. upstream reassignment | R4 (a–e) |
| scope/engine permit races | R5 (a–c) |
| Docker restart + network loss during poll/acquire/ACK/refresh | R6 (P1–P5 × F-a/F-b, + reboot) |
| bad digest/manifest/signer/ref/key/package/arch/record | R7 (a–j) |
| orphans + partial deletion + unknown events + stale generation | R8 (a–e) |
| fork selector spoofing + protected-workflow input substitution | R9 (a–e) |
| missing/skipped/wrong-provider/stale reports | R10 (a–f) |
| hidden ancestor limits + injected budgets | R11 (a–d) |

Row count: **11/11 spec §8 table rows covered**, 50 subcases total (R1:7, R2:6, R3:5, R4:5, R5:3,
R6:~11 phase×fault runs + reboot counted as subcases at execution, R7:10, R8:5, R9:5, R10:6, R11:4).

---

## Open questions (spec-underspecified — need author decisions, NOT invented here)

- OQ-1. Canary + scope identity. The spec names no canary repository/workflow, no scope/group/set names,
  no runner-group names, no dedicated local selector strings. Rows R1–R6, R9 need: canary repo(s),
  scope names for the two-scope race (R5), selector strings for spoof tests (R9). Author decision.
- OQ-2. Ownership-identity label schema. Spec §5.3 requires recorded ownership per object but defines no
  label/key format. R1/R6/R8 container-selection filters depend on it. Author decision (D1).
- OQ-3. Service unit names + `velnorctl` verbs. Spec §7 forbids inventing verbs; unit names, health
  commands, ledger-query commands, and recovery verbs come from the implemented package/help at
  execution. All `<TBD-…>` placeholders in R1/R2/R5/R6/R8 block on this.
- OQ-4. Deterministic lifecycle-window targeting. R2b/R2e (uncertain-acquire, lost-ACK) and R8a are
  narrow crash windows. The spec requires reconciliation but no test hook (pause/fault-injection flag /
  deterministic scheduler). Decide: journal-watcher timing loop (best-effort, record hit rate) vs a
  supported test hook in the product. Author decision.
- OQ-5. Protocol-fault harness. R3 (duplicate/reorder/partial-acquire) and R8d/R8e cannot be injected
  against real upstream with shell commands. Decide: D1 fixture replay vs transparent test proxy vs
  live-canary multi-session — including who builds it and how canary-scope-only isolation is proven.
  Author decision.
- OQ-6. Upstream-reassignment trigger. R4d needs a deterministic way to make GitHub withdraw/reassign an
  acquired request, or an explicit ruling that opportunistic observation suffices. Author decision.
- OQ-7. Test-time N mutation. R5 is cleanest at small N, but spec §4.4 treats N as qualified-per-value.
  Decide: is a temporary test N (with restore + re-verify) allowed, or must races run at production N
  with oversubscription? Author decision.
- OQ-8. Trust-matrix + attack payload schema. Spec §6 lists event types but the concrete allowed/excluded
  matrix, selector strings, and reusable-workflow input contracts come from D2. R9 payloads block on
  that schema. Author decision (D2).
- OQ-9. Synthetic-report feeding for R10c–R10e. Wrong-provider/stale/conflicting reports may not be
  constructible through the real GitHub API (GitHub controls job identity). Decide: test-aggregate
  harness feeding synthetic reports vs construction via real API quirks. Author decision.
- OQ-10. Quota-detection inventory. Spec §4.3/§8 name the check classes but not the exhaustive
  HostConfig-field list, the exact cgroup-ancestor chain to walk (unit slice path layout), or the
  forbidden-env-var list. R11's pass/fail checklist needs that enumeration. Author decision.
- OQ-11. Health-record + ledger + receipt schemas. Spec §8 requires health records "bound to"
  fields, permit-ledger entries, cleanup receipts, cache/timing reports — but no format/endpoint/query
  method. Every row's "observe" step blocks on: where to read them, freshness/sequence semantics, and
  the exact "missing = unavailable" threshold. Author decision.
- OQ-12. Outage-detection deadline measurement. Spec §8 sets 180s/120s/5m/10m targets; the measurement
  method (which timestamps, which clock, what counts as "reflected") is undefined. Needed to judge
  R1/R2/R6 recovery rows against targets. Author decision.
- OQ-13. Degraded-state surface. R6 requires a "visible degraded state" but the spec names no status
  surface (health-record field? endpoint? log line?). Decide the exact observable per phase. Author
  decision.
- OQ-14. JIT/runner identity vs request-ID mapping store. R2/R6/R8 identity-confusion checks need the
  defined mapping (stable operation/ownership IDs per spec §5.1 step 5) and where to read it. Author
  decision (D1).

## Explicit non-inventions

- No `velnorctl` verbs, unit names, label keys, selector strings, scope names, or endpoint paths were
  invented: all appear as `<TBD-…>` or OQs.
- No upstream API behavior assumed beyond what spec §5.1 states (e.g. no assumed reassignment trigger).
- No synthetic-report or proxy harness semantics assumed (OQ-5, OQ-9).
- Timing targets quoted from spec §8; measurement method left open (OQ-12).
