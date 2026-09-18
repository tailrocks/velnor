# E2 fault plan — bastion adversarial execution (spec §8, read-only planning, NO execution)

Status: plan only. Inputs: `plans/bastion-three-provider-ci/spec.md` §8 fault table (11 rows),
`/tmp/d1-design.md` (Rust Scale Set adapter: journal events, permit ledger, 9-step loop).
E2 executes this plan against canaries/fixtures; nothing here runs anything.

Conventions: N = host-wide max_jobs permit ledger (`job_permits`, D1 §2.3).
Ownership identity = `(ownership_id, operation_id)` + container/network/volume labels (D1 §2.3
`scaleset_workers`). Journal = `journal.db` event log (D1 §2.2). "Owned" = carries our
ownership labels; "unrelated" = everything else on the host.

## Canary / fixture targets (shared)

- C-RUNNER: official runner container + paired private DinD from a canary scale set (test org/repo,
  label `velnor-e2-canary`), provisioned via `ScaleSetProvisionIntended` with stable
  `operation_id`/`ownership_id`. Used by F1, F2, F6, F8.
- C-NATIVE: native-lane canary job worker (host-backed mediated container). Used by F1, F5, F8.
- C-QUEUE: fixture scale-set message stream (recorded/sanitized via `fixtures.rs`, redaction
  verified) replayable through `listener::run_once` → `scale::process_message`; plus live canary
  `JobAvailable` offers. Used by F3, F4, F5.
- C-SUPPLY: pinned runner/DinD digests, image manifest fixtures, APT repo fixture (signed
  Release + keyring), package/arch fixtures. Used by F7.
- C-TRUST: fork PR fixture attempting selector spoof + protected-workflow input substitution
  against `TrustClass::derive` → `AdmittedTrust::admit` → `admit_job`. Used by F9.
- C-RESULT: hosted verifier sandbox with fabricated/missing/stale reports per provider. Used by F10.
- C-RES: canary worker container + descendants for cgroup/env inspection. Used by F11.

## Fault rows

### F1 — Kill official runner / DinD / native worker (spec row 1; verifier: Docker/lifecycle)

- F1a target: C-RUNNER runner container. Procedure: `docker kill` (SIGKILL, no remove) mid-job;
  separately `docker kill` the DinD sidecar mid-job; separately SIGKILL the runner process inside
  (container stays up, process dead). Each sub-case on a fresh canary job.
- F1b target: C-NATIVE worker. Procedure: SIGKILL native job worker process mid-step.
- Invariant: no false success — job/runner result is failed/incomplete, never success; diagnostic
  bundle exported before deletion (`supervise.rs` ordering); cleanup removes ONLY owned resources
  (ownership-label match); permit released exactly once after cleanup confirmed
  (`ScaleSetPermitReleased`, ledger `released`; no double-release); next canary job on same host
  starts clean (no workspace/network/name residue).
- Evidence: job/result status; diagnostic bundle path+contents; `docker ps -a`/network/volume
  listing before+after (owned gone, unrelated intact); `job_permits` row history for the permit
  (single `released` edge); journal `ScaleSetWorkerEdge` chain to terminal; next-job success log.
- Safety: kill by exact container ID/name of the canary only; never `docker kill` by pattern,
  never prune; unrelated containers untouched.

### F2 — Kill Velnor at every crash point (spec row 2; verifiers: protocol + recovery)

- Target: C-RUNNER + C-QUEUE, Velnor daemon (SIGKILL the daemon process, restart it).
- Procedure: separate run per crash point, each with a canary offer/job in flight: (1) before
  acquire (after `ScaleSetAcquireIntended` persisted, before HTTP); (2) after acquire response,
  before `ScaleSetAcquireResolved`; (3) after acquire resolved, before JIT provision intent;
  (4) after `ScaleSetProvisionIntended`, during Docker create; (5) after create, before start;
  (6) while running; (7) after completion observed, before cleanup; (8) after `process_message`
  commit, before ACK (`delete_message`). Drive crash points via journal-state-gated kill script
  (poll journal for the marker event, then SIGKILL), not sleeps.
- Invariant: durable reservations reconciled on restart (`reconcile.rs` startup): active canary
  containers adopted (ownership labels match) or explicitly failed — never silently dropped;
  no duplicate execution (stable `operation_id` idempotency, no second runner for same
  `request_id`); no lost job (every acquired ID reaches terminal or re-queue); no leaked permit
  (ledger count consistent: `occupied ≤ N`, uncertain permits resolved, never double-spent).
- Evidence: per-crash-point journal event dump (intent→resolve edges); `scaleset_workers` +
  `job_permits` before/after restart; container list (adopted IDs identical); GitHub-side job
  outcome (single execution); permit ledger `COUNT(*) NOT IN ('released')` reconciliation.
- Safety: SIGKILL only the Velnor daemon PID; canary scale set only; restart promptly to bound
  queue stall; unrelated lanes' permits must show zero state change.

### F3 — Redelivery / duplicate / reorder / partial-uncertain acquire (spec row 3; verifier: protocol)

- Target: C-QUEUE fixture stream + live canary offers.
- Procedure: (1) replay the same `message_id` twice (expect `ScaleSetMessageSeen` dedupe, no
  re-grant); (2) deliver the same `JobAvailable request_id` in two batches (expect original
  `first_seen_at`+`sequence` retained, single demand row); (3) reorder Assigned before Available;
  (4) fixture `AcquireJobsResponse` returning a strict subset of requested IDs; (5) fixture
  transport timeout after send (expect `uncertain=true`, permits stay `acquiring` and counted).
- Invariant: dedupe correct (no double grant/provision); returned-ID set reconciliation
  (`acquired = resp ∩ requested`, `missing = requested − resp`, never assume-all); no statistics
  double-count (step 7 uses authoritative `TotalAssignedJobs`, never batch counts + reservations);
  first-seen age retained across redelivery (no queue-jump reset).
- Evidence: `scaleset_demand` rows (`first_seen_at`/`sequence` immutable across redeliveries);
  `ScaleSetAcquireResolved{acquired_ids,missing_ids,uncertain}` events; stats-observed vs
  population-reconcile logs; missing IDs back to `eligible` with preserved age.
- Safety: fixture replay against canary demand projection (or isolated journal copy where the
  harness supports it); live redelivery only on canary scale set; no production `request_id` reuse.

### F4 — Cancel queued/running incl. upstream reassignment (spec row 4; verifier: queue)

- Target: C-QUEUE + C-RUNNER canary work.
- Procedure: (1) cancel queued-but-unacquired canary offer (GitHub cancel); (2) cancel after
  acquire, before provision; (3) cancel while runner running; (4) upstream reassignment: GitHub
  reassigns the job away (offer withdrawn / assigned elsewhere) while we hold demand/permit state.
- Invariant: correct terminal/withdrawn state per case (declined-terminal vs failed-cancelled,
  never success, never stuck `acquiring`); no new runner provisioned for stale demand
  (withdrawn `request_id` never reaches `ScaleSetProvisionIntended`); permit/accounting correct
  (released once, or retained-visible on cleanup failure — never silently freed, never leaked).
- Evidence: demand row terminal state + reason; worker machine edges; `job_permits` history;
  GitHub job conclusion vs local state (match); absence of provision events post-withdrawal.
- Safety: cancel canary runs only (by run ID); never cancel unrelated runs; reassignment driven
  by upstream API on canary scope, not by local state edits.

### F5 — Permit race: two scopes × both engines (spec row 5; verifier: capacity)

- Target: C-QUEUE (two canary scopes/scale sets) + C-RUNNER + C-NATIVE, host ledger at N−1 and at N.
- Procedure: with one free permit, simultaneously offer oldest-eligible work in scope A (scaleset
  lane) and scope B (native lane); repeat at zero free permits; repeat with sustained contention
  (both scopes backlogged, jobs completing to free single slots repeatedly).
- Invariant: total occupied ≤ N across scopes AND lanes (single ledger, no per-scope/per-lane N);
  oldest observed eligible work not locally overtaken (grant order by `(first_seen_at, sequence)`
  across scopes); no permanent idle-slot starvation (a free permit is granted within bounded polls
  while eligible demand exists); advertisement (`X-ScaleSetMaxCapacity`) never N per listener.
- Evidence: `job_permits` ledger snapshots under contention (max occupied ≤ N); grant order log vs
  demand age order; per-poll advertised capacity values; starvation bound (polls-to-grant per scope).
- Safety: canary scopes only; contention jobs are no-op sleeps (short, bounded); never starve
  unrelated real work — abort contention if unrelated queue age grows past threshold.

### F6 — Docker restart / network loss during poll/acquire/ACK/refresh (spec row 6; verifiers: infra/recovery)

- Target: C-RUNNER + live poll loop; Docker daemon + egress network path.
- Procedure (each in the COORDINATED WINDOW, §Window): (1) `systemctl restart docker` during poll;
  (2) restart during acquire HTTP; (3) restart after provision, before ACK; (4) drop egress
  (iptables rule scoped to queue-API destination, time-bounded) during acquire/ACK/token-refresh,
  then restore; (5) expire queue token (force 401 path) combined with network flap.
- Invariant: visible degraded state (health records show degraded, not healthy-success); no unsafe
  new acquisition while degraded (no acquire without verified ledger + reachable queue); bounded
  retry with backoff (401→refresh+retry-once; 429/5xx per `backoff.rs`, no hot loop); recovery
  without orphan/identity confusion (post-restart containers re-adopted by ownership labels,
  no duplicate provision, permits reconciled).
- Evidence: health-record stream across the fault (degraded markers + freshness); retry/backoff
  log with bounded counts; pre/post container inventory (same owned IDs, no dupes); journal
  session/ACK events (`ScaleSetSessionClosed/Opened`, un-ACKed redelivery deduped).
- Safety: COORDINATED WINDOW ONLY (see below): quiesce unrelated jobs first, preserve SSH
  (never touch port 22 / sshd, never drop loopback or SSH source); network rules destination-
  scoped + auto-expiring (at-job restore + dead-man's timer); Docker restart via service manager,
  never `pkill -9 dockerd` with active unrelated containers unless window quiesced.

### F7 — Bad digest/manifest/signer/ref/key/package/arch/record (spec row 7; verifier: supply-chain)

- Target: C-SUPPLY fixtures; runner/DinD image resolution, APT pipeline (`§7`).
- Procedure: per sub-case feed one bad input and attempt the gated operation: (1) wrong runner
  image digest; (2) manifest referencing unknown/mismatched digest; (3) untrusted signer or ref
  (signer allowlist miss); (4) wrong APT Release key; (5) tampered package hash; (6) wrong-arch
  package; (7) inconsistent signed record (VERSION vs repo metadata mismatch).
- Invariant: rejected BEFORE execution/publication/install (fail closed at the gate, never
  post-hoc); no insecure fallback (no unsigned/tag-floating retry, no downgrade); previous
  trusted state intact (last-good digests/keys still in force, no partial overwrite).
- Evidence: rejection log per sub-case with gate name + reason; proof no container started /
  nothing published / nothing installed (container/image/package absence); last-good pin files
  unchanged (hashes before/after).
- Safety: fixtures local-only; never publish bad artifacts to a real repo/channel; never install
  bad packages on the host (verification happens in sandbox paths); production keyring read-only.

### F8 — Orphans / partial deletion / unknown events / stale generations (spec row 8; verifier: lifecycle)

- Target: C-RUNNER ownership set + journal generation counter + C-QUEUE fixture events.
- Procedure: (1) plant orphan containers/networks/volumes: ours-but-unrecorded (labels match, no
  DB row), recorded-but-half-deleted (container gone, network/volume remain), foreign (no labels);
  (2) inject unknown `messageType` event; (3) inject events carrying a stale control generation;
  (4) simulate partial deletion (remove runner container, leave DinD + network + workspace).
- Invariant: ownership reconciled (adopt-or-fail owned orphans, complete partial deletions,
  foreign resources never touched); never panic, never broad-prune (no `prune`, no label-less
  deletion); unreconciled state never counts as free (permits/ledger stay occupied until
  reconciled; capacity advertisement excludes uncertain).
- Evidence: reconcile run log (per-orphan decision: adopt/complete/ignore with reason);
  foreign-resource presence before/after (intact); no-panic proof (daemon alive, unknown event
  journaled + `reconcile::unknown_event` path); ledger count unchanged by unreconciled items.
- Safety: orphans planted with canary-unique names/labels; foreign control object planted to
  PROVE non-interference; no prune command anywhere in the procedure scripts (grep-gate the scripts).

### F9 — Fork spoofing / input substitution (spec row 9; verifier: trust)

- Target: C-TRUST fixtures through `TrustClass::derive` → `AdmittedTrust::admit` → `admit_job`.
- Procedure: (1) fork PR whose head spoofs a trusted selector (branch/ref/label match attempt);
  (2) protected-workflow run with substituted inputs (actor, ref, input params swapped post-approval);
  (3) replay of a previously admitted decision with mutated payload (binding check).
- Invariant: NO bastion execution in any sub-case (no permit, no provision, no runner); hosted-only
  trust exclusion explicit (decision log names the exclusion rule + mismatched binding field).
- Evidence: admission decision log per sub-case (deny + rule + field); absence of
  `ScaleSetOfferGranted`/permit/provision events for the spoofed `request_id`s; hosted-side routing
  record showing hosted-only handling.
- Safety: fixtures only, no real fork interaction; no execution means no blast radius — verify by
  absence of Docker/ledger effects, not by trusting the deny log alone.

### F10 — Bad/missing reports (spec row 10; verifier: result)

- Target: C-RESULT hosted-verifier sandbox + required-result aggregation.
- Procedure: per sub-case submit: (1) missing test report from one provider; (2) skipped report;
  (3) wrong-provider report (provider field mismatch); (4) stale run attempt (superseded
  sequence/freshness); (5) late success arriving after required result already failed; each with
  remaining providers green.
- Invariant: required aggregate FAILS (or stays failed) in every sub-case — green remainder never
  masks the defect; later reporter/cancellation never overwrites an already-failed required result
  with success; reruns revalidate exact identity + outcome provenance.
- Evidence: aggregate verdict per sub-case (fail) with failing-report citation; overwrite-attempt
  log (rejected, original failure preserved); provenance chain (repo/source/run/attempt/provider/
  sequence binding) for each accepted report.
- Safety: sandbox aggregation only; no writes to real required-status contexts during fault
  injection (record verdicts locally; the only GitHub-visible effect, if any, is the canary run's
  own honest status).

### F11 — Hidden limits / injected budgets (spec row 11; verifier: resource)

- Target: C-RES canary worker + descendants ( DinD inner containers, build children).
- Procedure: (1) inspect real containers/cgroups (`cpu.max`, `memory.max/high`, pids) and
  environment for CPU/RAM ceilings on worker and every descendant; (2) inject a build budget
  (e.g. `MAKEFLAGS`/tool-level `-j`/memory flag, cgroup limit on a fixture subtree) and prove
  detection attributes it as injected, not policy; (3) run quota-free demonstration workload
  (CPU+memory scale-up within host headroom, bounded) showing no throttle/kill from policy.
- Invariant: quota-free policy demonstrated on worker AND all descendants (no CPU/RAM ceilings
  per §4.3); any discovered limit is identified as injected/foreign with provenance, never silent.
- Evidence: cgroup + env dumps for worker and each descendant (before/during/after); injected-
  budget detection record; demonstration workload completion log with throttle/oom counters at zero.
- Safety: demonstration workload bounded well below host capacity (leaves headroom for unrelated
  work + SSH); NEVER deliberate whole-host OOM; abort on memory pressure thresholds.

## Evidence bundle (per row)

Each row returns: procedure script (exact commands), timestamped logs, journal/ledger/demand
dumps (redacted per D1 rule — hashes/fingerprints, no raw tokens), Docker inventory diffs,
verdict vs invariant (PASS/FAIL with citation). Bundle naming: `e2-<F#>-<subcase>-<ts>/`.

## Safety constraints (global, non-negotiable)

- Canary/fixture scope: all faults target identified canaries (C-*) or fixtures; production
  scopes/repos/runs never faulted.
- No destructive disk ops; no broad prune (`docker prune`, label-less delete, `rm -rf` outside
  owned canary paths); no deliberate whole-host OOM; no unrelated network outage.
- F6-class shared-impact faults (Docker restart, host reboot if needed) run ONLY in the
  coordinated window below.
- Every procedure script is grep-gated for forbidden commands before execution.

## Coordinated window plan (F6 + any host-reboot-class fault)

- Scheduling: pre-announced isolated window (off-peak, owner on-call); single window covers all
  F6 sub-cases sequentially to avoid repeated disruption.
- Pre-window: drain/quiesce unrelated jobs (no new acquires for unrelated scopes; let running
  unrelated work finish or checkpoint); snapshot unrelated state (container list, ledger rows);
  verify SSH path + out-of-band access; arm network-rule dead-man's restore timer.
- In-window: execute F6(1)→F6(5) in order with recovery + verification between sub-cases; abort
  criteria: unrelated container dies, SSH unreachable, or ledger shows unrelated-permit mutation —
  abort immediately, restore, post-mortem.
- Post-window: confirm unrelated state matches pre-window snapshot (minus normal completions);
  confirm SSH + daemon healthy; resume normal scheduling; publish window report.
- Non-window rows (F1–F5, F7–F11) need no window: canary-scoped, no shared daemon/network impact.
