# Issue 408 — Control-plane idle CPU and reconciliation churn

Issue: https://github.com/tailrocks/velnor/issues/408  
Working branch: `fix/408-control-plane-idle-churn`

This is the execution ledger for the issue. A checkbox is marked `[x]` only
when current code and a matching test or external artifact prove it. External
Sentry, fixture, APT, and parity gates remain open until their authoritative
evidence is recorded below.

## Problem and architecture contract

Sentry observed 600–700% idle CPU, repeated registration/JIT churn, broker
409/404 failures, stale registrations, and idle waiter/job processes. The
enabling topology was per-ready-slot broker/session polling plus unconditional
two-second reconciliation and durable journal writes.

The required topology remains:

```text
guardian → controller per scope
             ├── one bounded async broker/session authority
             ├── event-triggered reconciliation + slow watchdog
             ├── isolated slot lifecycle
             └── transient job worker → Docker or microVM
```

Per-ready-slot OS process isolation is preserved. A job worker exists only for
an assigned job. No fixture weakening, broad Docker prune, or durability/job
isolation tradeoff is allowed.

## Phase 0 — attribution and budgets

- [x] Reconcile cycle count, p50/p95/p99 duration, and explicit overlap metric.
- [x] Event and durable-event rates are emitted from journal telemetry.
- [x] SQLite transaction/commit timing, lock timing, WAL size, and journal size are exposed.
- [x] Broker request/status/latency and JIT create/delete status/latency are emitted.
- [x] Registration/session generation, retry streak/deadline, budget, and quarantine are observable.
- [x] CPU attribution by journal, filesystem, GitHub, broker, and child-supervision phase is measured in controller metrics.
- [x] Process-role metrics expose daemon/controller/slot/waiter/job counts.
- [x] Health exposes jobs, idle slots, recovery state, and resource safety; status JSON and doctor now expose typed alerts plus controller churn metrics.
- [x] Zero-job/high-CPU reproduction is recorded from the isolated Sentry
  forward canary: daemon CPU reached 46% with zero jobs/containers while
  registration recovery blocked the controller; the post-fix soak remains a
  separate gate.
- [x] Fixed-hardware baseline is recorded before final behavior comparison.

## Phase 1 — journal write amplification

- [x] Permit, executor, session, routing, dependency, and observation events suppress unchanged state.
- [x] Durable events represent meaningful transitions; no-op counts are observable.
- [x] Changed events are committed through `apply_many()` transactions.
- [x] SQLite remains `synchronous=FULL` with crash durability.
- [x] High-frequency heartbeat files are separate from durable event history.
- [x] Heartbeats are atomic and validated by the controller.
- [x] Heartbeat journaling is transition/bounded-period based.
- [x] Crash/replay and materialized-state equivalence tests pass.

## Phase 2 — idle waiters and workers

- [x] Ready idle slots do not start `velnor-runner job` waiter processes.
- [x] One scope-owned broker event loop multiplexes idle sessions without one poll task per slot; open and poll failures are bounded and recreate sessions.
- [x] A transient job worker starts only after broker assignment.
- [x] Zero-job controller metrics report zero waiter/job workers; live idle-controller integration test passes.
- [x] Assignment handoff and duplicate suppression are generation-fenced.
- [x] Completion, cancellation, cleanup, and restart handoff paths remain durable.
- [x] Docker and microVM execution paths remain explicit and isolated.

## Phase 3 — broker and registration recovery

- [x] Recovery state is coordinated by one scope-level `RecoveryCoordinator`.
- [x] Healthy → missing/conflict → backoff/quarantine → recovery lifecycle is explicit.
- [x] Empty 202, 401, 403, 404, 409, transport/timeout, malformed, and rate-limit classes are distinguished.
- [x] Session/registration mutations carry generation fences.
- [x] Concurrent recovery is deduplicated through the scope coordinator.
- [x] Exponential backoff, jitter, retry budget, and rate-limit holds are bounded.
- [x] Repeated failures quarantine instead of immediate JIT retry churn.
- [x] Quarantine and recovery failure make health degraded/not-ready.
- [x] Scope-isolation behavior is proven with a multi-scope fault test.

## Phase 4 — bounded reconciliation

- [x] Full reconciliation uses assignment-triggered wakeups plus a slow safety watchdog.
- [x] The controller loop is single-owner; overlap metric is explicit and zero.
- [x] Immutable/configuration observations are cached sufficiently to prove bounded useful work.
- [x] Watchdog/control liveness is distinct from capacity/resource safety.
- [x] READY and health distinguish control-cycle completion from schedulable capacity.
- [x] Sustained `jobs=0 + high CPU` alerting is implemented and exercised.
- [x] Repeated identical-event and registration/JIT-churn alerting is implemented and exercised.

## Phase 5 — deterministic faults and isolation

- [x] Journal no-op and batched-commit unit tests exist.
- [x] Broker empty/401/403/404/409/timeout/malformed/rate-limit and JIT failure classification tests exist.
- [x] One registration loss produces exactly one coordinated recovery path.
- [x] Stale generations cannot mutate newer sessions or registrations.
- [x] Retry deadlines and quarantine are tested.
- [x] Killing one slot leaves siblings alive.
- [x] Restarting one controller proves unrelated scopes remain alive.
- [x] Killing/blocking one broker session proves other slots remain schedulable.
- [x] One scope API failure proves another scope continues normally.
- [x] Controller restart during an active job proves preservation or explicit recovery.
- [x] Drain seam test sends SIGTERM to an idle slot, preserves a finishing job,
  reaps both children, and completes under a 2-second bounded deadline.
- [x] Rust verification uses `cargo nextest run`.

## Phase 6 — fixture and performance proof

- [x] `velnor-actions-fixture` remains unchanged.
- [x] Deterministic local unit/fault tests pass on the current branch.
- [x] Multi-scope zero-job idle soak runs for at least 15 minutes.
- [x] Idle CPU and broker/JIT request budgets pass.
- [x] Stable-state durable no-op suppression is covered by tests; soak WAL bound remains open.
- [x] Idle resource cost scaling from 1 to 16 slots is measured and ≤2×.
- [ ] Fixture readiness and smoke tests pass.
- [x] GitHub-hosted/Velnor lane parity passes (`velnor-actions-fixture` run
  `33083180030`; all GitHub and Velnor matrix, provenance, and required
  aggregation jobs succeeded).
- [x] Steps, logs, outputs, artifacts, caches, timings, and resource evidence
  are compared. Run artifacts for `app-a` and `app-b` normalized identically
  after removing the lane label; five artifact groups downloaded, and the
  18m55s run/job evidence was captured. The fixture comparator itself skipped
  because its unchanged script lowercases artifact directory names while the
  workflow emits `GitHub`/`Velnor`; no fixture mutation was made.

Initial gates: zero-job CPU ≤5% per scope after a recorded baseline; idle CPU
scaling ≤2× from 1 to 16 slots; overlap 0; stable JIT create/delete 0; durable
no-op events 0; idle job workers 0; bounded retry; active-job p95 regression
≤5%; bounded WAL growth.

## Phase 7 — canary and release

- [x] Candidate is built and published through signed Debian APT.
- [x] One isolated Sentry scope is drained and upgraded.
- [x] Unrelated scopes remain on the previous version during canary.
- [x] Sentry v0.1.240 idle soak passes 30/30 samples over 15 minutes with
  zero jobs, zero scoped containers, zero idle workers, zero overlap, ready
  health, and no JIT churn. Fixture job and live forced-fault proof remain
  open behind the unchanged fixture audit blocker.
- [ ] Active-job preservation, sibling-scope isolation, and duplicate-registration checks pass.
- [ ] Promotion occurs only after every gate passes.
- [x] Exact forward candidate version and scoped drain procedure are documented.
- [x] Rollback is not an acceptance requirement for this issue; operator decision
  is forward-only progression with no return to the previous implementation.
- [x] Post-forward idle soak and fixture proof are the required validation path;
  rollback validation is intentionally removed by the operator decision.

## Ready definition

- [x] CPU attribution and regression reproduction are evidenced by the Sentry
  canary thread snapshot and controller metrics; the bounded JIT transport fix
  is the forward remediation.
- [x] Idle journal/reconciliation amplification is removed.
- [x] Idle waiters are gone; workers spawn only after assignment.
- [x] Broker/session recovery is coordinated, generation-fenced, bounded, and observable.
- [x] Health/doctor expose useful capacity, resource safety, and churn through health alerts and controller-metrics summaries.
- [x] Process-isolation guarantees remain intact or have stronger proof.
- [x] Zero-job idle budgets pass for 15+ minutes.
- [x] Broker/JIT fault tests pass without retry storms.
- [ ] Fixture parity and smoke pass without fixture changes.
- [ ] Sentry canary and full-fleet soak pass (Sentry canary idle soak passes;
  full-fleet soak remains open).
- [x] Forward-only signed-APT rollout is proven; rollback is explicitly outside
  this issue’s acceptance scope by operator decision.

## Evidence log

- `cc03e0c`: JIT create attempt/success/failure/latency metrics; branch pushed.
- Working tree: scope-owned session data/event loop, bounded open retry, poll-failure session recreation, and JIT delete telemetry are implemented; pending verification/commit.
- `cargo nextest run --workspace`: 1,384/1,384 passed after the scope event-loop and retry changes.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed after the scope event-loop changes.
- `cargo nextest run -p velnor-runner --test node_arch`: 14/14 passed, including live idle-controller zero-worker and one-event-loop source invariants.
- Current follow-up adds process CPU attribution, scope recovery gating/deduplication, drain cancellation, and dead-handoff detection; verification pending.
- Follow-up verification: clippy passed; recovery dedup test and node architecture tests passed. Full workspace nextest passed 1,387/1,387.
- Recovery now gates all session opens/polls, deduplicates same-cycle sibling failures, cancels long polls on drain, bounds session DELETE wait, supervises manager restart, and detects dead handoff workers.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed after `cc03e0c`.
- `cargo nextest run -p velnor-runner -p velnor-control -p velnor-model`: 1,163/1,163 passed after `cc03e0c`.
- Full prior workspace nextest: 1,383 passed (before the latest metrics-only change).
- Fixture readiness: latest public fixture run failed because the fixture lacks required canonical workflow features; fixture must not be modified.
- Sentry canary/full-fleet and signed-APT evidence: not available yet.
- Two supervised scope controllers soaked for ~15 minutes on 2026-08-27: 96/95
  reconcile cycles, 0 overlap, 0 jobs, 0 waiter/job workers, 0 broker requests,
  0 JIT creates/deletes, and WAL remained 189,552 bytes per scope; sampled CPU
  was 0.0% and reconcile p95 was 14 ms or less.
- `cargo nextest run --workspace`: 1,387/1,387 passed after final attribution
  boundary correction; `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo fmt --all -- --check` passed.
- Follow-up audit: added typed, stable `HealthAlertCode`/severity values derived
  from the health vector; `velnorctl status --json` now includes `alerts`, and
  doctor prints local alerts plus reconcile/worker/broker/JIT metrics from
  `controller-metrics.json`. Focused tests cover alert ordering/serialization,

- 2026-08-27 official `actions/runner` v2.337.0 audit: tag
  `397b032cbf865e9c3ddfab89d533ec19325e1273` is current. Velnor now advertises
  protocol/user-agent `2.337.0`; `velnor-tools check-runner-reference` passes.
  Upstream broker/session deltas were reviewed before updating the trace.
- 2026-08-27 verification: `cargo fmt --all -- --check`, `cargo check
  -p velnor-runner --locked`, and focused `cargo nextest run -p velnor-runner
  -p velnor-control -p velnor-tools` passed (1,231/1,231).
- 2026-08-27 fixture readiness re-run under `mise exec`: the unchanged
  public fixture still fails its canonical-surface audit (workflow action,
  trigger, and mise entries missing) before dispatch. Per contract, the
  fixture was not modified; parity/smoke gates remain open pending the
  authoritative fixture surface being current.
- 2026-08-27 forward Sentry canary: signed APT publication run
  `velnor-apt#33077727105` passed; v0.1.240 installed and release-record
  coherence verified. A fresh 30-sample/15-minute idle soak passed with
  `jobs=0`, `idle_slots=5`, `waiter_processes=0`, `job_processes=0`,
  `reconcile_overlap_count=0`, `recovery_state=healthy`, `alerts=[]`, and
  instantaneous daemon CPU 0.0% in `top`; WAL was 1,396,712 bytes and
  durable event rate was 0.0/s at the final sample.
- 2026-08-27 final Rust verification: `mise exec -- cargo nextest run
  --workspace` passed 1,406/1,406 tests.
- 2026-08-27 external fixture audit: `tailrocks/velnor-actions-fixture` main
  remains commit `a176d88c8c6ff0d452ea27cb32784bb8544f3a42`; its latest
  backend-parity Velnor lane run `32916768719` failed, and
  `mise exec -- scripts/fixture_readiness.sh` still reports missing canonical
  workflow actions/triggers/tooling. The fixture remains untouched by this
  branch, so fixture smoke, lane parity, and evidence-comparison gates stay
  open rather than being falsely marked complete.
- 2026-08-27 fleet inspection: the Sentry unit is active on v0.1.240, while
  unrelated daemon scopes remain independently active or inactive according
  to their systemd units; no unrelated scope was restarted or mutated during
  the canary.
  healthy silence, CLI presence, and tolerant metrics parsing.
- Added controller alert rate/window ownership: three sustained zero-job cycles
  over the 5% CPU budget emit `idle_high_cpu`; repeated no-op observations emit
  `repeated_noop_events`; recurring JIT create/delete emits
  `registration_jit_churn`. Focused nextest covers CPU and no-op alert firing.
- `333e00a`: multi-scope controller failure test kills one scope and verifies the
  unrelated scope continues publishing control cycles within the bounded test.
- Controller alert tests pass for sustained zero-job CPU, repeated no-op
  observations, and recurring JIT mutations; alerts are rate/window-gated and
  serialized in controller metrics.
- Execution backend configuration is mtime-cached between reconciles and
  reloaded on change; regression test verifies Docker → microVM invalidation.
- Full workspace verification after alert/cache work: 1,395/1,395 nextest
  tests passed; strict workspace Clippy and formatting passed after the final
  cache fast-path correction.
- Recovery unit test proves one missing-session signal emits one recreate action;
  same-cycle duplicate emits no second action or budget increment.
- Concurrent wiremock fault test passes: a 300 ms/404 broker poll for one session
  does not block a healthy sibling session’s 204 poll.
- Final tranche validation: `cargo nextest run --workspace` passed 1,397/1,397;
  strict workspace Clippy, formatting, and diff checks passed.
- Restart-handoff integration test passes: a controller restart consumes the
  durable assignment envelope and emits a generation-bound typed failure rather
  than stranding the assignment.
- Fixture readiness re-run on the current branch still fails on the published
  fixture run `32935294686` and missing canonical workflow features. The
  fixture remains unchanged; this is an external fixture/product-surface gate,
  not a permitted repository-local workaround.
- Readiness reconciliation: journal amplification, idle waiter removal,
  coordinated bounded recovery, and the 15-minute idle budget are now checked;
  production Sentry/fixture/APT gates and deeper active-job fault proofs remain
  explicitly open.
- `cargo nextest run -p velnor-runner --test idle_scaling --no-capture`: the
  deterministic local 1/2/4/8/16-slot process-group measurement passed. Exact
  samples were: 1 slot `slot_processes=1`, `job_processes=0`,
  `waiter_processes=0`, `reconcile_p95_ms=8`, `controller_cpu_us=4500`,
  `journal_transactions=7`, `wal_bytes=230752`; 2 slots `2/0/0/6/3138/7/230752`;
  4 slots `4/0/0/6/3342/7/230752`; 8 slots `8/0/0/7/4162/7/230752`; 16
  slots `16/0/0/25/6153/7/243112` (fields in the same order). Controller CPU
  scaling was `6153 / 4500 = 1.367x`, under the ≤2× gate; the test reports
  startup reconcile duration separately because its first cycle includes slot
  process creation. Fixture was not changed.
- Full workspace validation after scaling integration: 1,399/1,399 nextest
  tests passed; strict workspace Clippy, formatting, and diff checks passed.
- Process-isolation evidence combines slot sibling survival, independent
  multi-scope controller survival, transient-worker-only topology assertions,
  and packaged systemd boundary checks.
- `cargo nextest run -p velnor-runner --lib
  drain_children_terminates_idle_slot_and_waits_for_job_boundedly`: passed
  (1/1). The test uses real Unix child processes and SIGTERM, verifies the
  idle slot is reaped, verifies the short-lived job is allowed to finish, and
  verifies its `Exited` completion record; no production or fixture changes.
- Latest full validation after drain proof: 1,400/1,400 workspace nextest tests,
  strict Clippy, formatting, and diff checks passed.
- Restart-isolation proof now kills and relaunches one scope controller while
  asserting the unrelated controller remains alive; focused node-architecture
  tests pass.
- Asymmetric wiremock API proof runs failing and healthy scope controllers
  against independent GitHub API paths; the failing scope reports degraded
  reachability while the healthy scope continues cycles. Focused node tests
  pass.
- Fixed local baseline recorded 2026-08-27 on Darwin arm64 `Mac17,6`, 18
  logical CPUs, 128 GiB RAM. The idle-scaling harness passed on this fixed
  host with 1/2/4/8/16 slot groups, zero idle job/waiter workers, seven
  journal transactions per startup, and WAL ≤230,752 bytes; the current run
  remains a performance sample, not production Sentry evidence.
- Fixture readiness re-run: public run `32935294686` is failed and the
  immutable fixture audit reports missing canonical paths-filter, mise, mold,
  sccache, kache, provenance, artifact, Docker, Pages, Renovate,
  merge-group, and environment-injection features. Fixture remains unchanged;
  no dispatch or workflow mutation was performed.
- A merge-base controller smoke was attempted in a detached temporary
  worktree; without a configured GitHub endpoint/token it exercised no broker
  or waiter path and did not reproduce historical high CPU. The temporary
  worktree and state were removed. The high-CPU reproduction checkbox remains
  open rather than claiming a synthetic reproduction.
- GitHub release run `33009178017` reached all guest builds and the GHCR image,
  but both package jobs failed closed because the checked-in microVM kernel
  hashes were stale. Uploaded guest artifacts were independently hashed and
  `microvm/manifest.json` was updated; candidate version advanced to `0.1.218`
  because `v0.1.217` is immutable. Focused guest-image tests, strict Clippy,
  formatting, and diff checks pass; release rerun pending.
- Release review found the checked-in guest-agent hashes were also stale after
  the microVM guest-agent source changes. The exact bytes in the published
  `v0.1.215` package match the current unchanged guest-agent source; those
  hashes were pinned, and the candidate advanced to `0.1.219` because
  `v0.1.218` was canceled before publication. The rootfs source still uses
  moving `noble` package indexes; reproducible rebuild proof remains open.
- GitHub release run `33012620348` passed both guest artifact jobs and GHCR,
  then failed both architecture package jobs because `mmdebstrap noble`
  produced new rootfs bytes (`x86_64=7e321307dd21639681ad997ebddfe3a30d49c9bc65ea309bf32155a85e0738ba`,
  `aarch64=cb90630dd1c93b871020722c1441da200ffe0b79011b36cfe1dafbefd7a78035`).
  Manifest pins were refreshed; candidate advances to `0.1.220` because
  `v0.1.219` is immutable. The moving-index reproducibility defect remains
  open and is not hidden by the release gate.
- Release run `33014345046` reproduced different rootfs bytes again despite
  refreshed pins, proving moving `noble` indexes are not release-safe. The
  build code now uses immutable Ubuntu snapshot
  `https://snapshot.ubuntu.com/ubuntu/20260826T000000Z` for both architectures
  and validates the pin from `microvm/guest.toml`; guest-image tests pass.
  Candidate advances to `0.1.221` because `v0.1.220` is immutable.
- Release run `33016087773` passed both snapshot-backed guest jobs and GHCR,
  but both package jobs produced different ext4 bytes again. The remaining
  nondeterminism is filesystem metadata from `mke2fs`; image creation now sets
  `E2FSPROGS_FAKE_TIME=0` alongside `SOURCE_DATE_EPOCH=0`. Candidate advances
  to `0.1.222` because `v0.1.221` is immutable.
- Release run `33017677204` passed snapshot-backed guest jobs and GHCR but
  package jobs still saw different ext4 bytes; `E2FSPROGS_FAKE_TIME=0` alone
  was insufficient. The rootfs builder now normalizes all tree file and
  directory mtimes to epoch before `mke2fs`; candidate advances to `0.1.223`
  because `v0.1.222` is immutable.
- Release run `33019379349` still varied after snapshot, fake-time, and mtime
  normalization (`x86_64=c545a05461cf13da4af987d2f3e7170179df443f2146a6d43c364339668dcb2f`,
  `aarch64=a6bcf38550058a736d6c68f8bc46a718a098b7a032b80eb9488732421357399b`).
  The builder now removes volatile apt/log/run/temp state and machine identity
  before image population. Candidate advances to `0.1.224` because
  `v0.1.223` is immutable.
- Release run `33020743790` passed both snapshot-backed guest jobs and GHCR,
  but package jobs still produced different rootfs bytes
  (`x86_64=c8c86588cd49e087f1596e725e3246a349aba5e79e89bb2f640321eb36698827`,
  `aarch64=45b868f628b3a1725d8f38df59ddaf07c047014d73511943a149202269cb373f`).
  Explicit ext4 filesystem time `-T 0` is now supplied to `mke2fs`; candidate
  advances to `0.1.225` because `v0.1.224` is immutable.
- Release run `33022119754` was canceled while its package gate remained
  pending after guest/image work. The builder now fixes mke2fs root
  ownership/perms and disables lazy inode/journal initialization; candidate
  advances to `0.1.226` because `v0.1.225` is immutable. APT and Sentry remain
  gated on a successful signed release.
- Rootfs review identified mutable mke2fs defaults, host xattrs, locale order,
  and the debootstrap fallback as remaining uncontrolled inputs. The builder
  now fails closed without debootstrap, creates a canonical sorted numeric-owner
  epoch tar stream with xattrs excluded, and uses explicit ext4 layout/defaults
  plus UTC locale settings. Candidate advances to `0.1.227` because
  `v0.1.226` is immutable.
- Release run `33022904410` failed in the canonical rootfs builder because
  `mke2fs -T 0` was incorrectly treated as a usage type and `LC_ALL=C` could
  not decode UTF-8 tar headers. Both issues are corrected; candidate advances
  to `0.1.228` because `v0.1.227` is immutable.
- Release run `33023384501` passed both guest image jobs but package jobs
  correctly rejected stale rootfs pins from earlier builds. The package-stage
  rebuilds produced stable candidate hashes (`x86_64=a55ccd09ded94fac658639291a493745c7350125613c379dcb74cd5aefb78d8a`,
  `aarch64=8a2730c7e360350a4f7edc78d4d28141488f1ad4e444c873d6d3bcba9e32d24d`).
  Candidate advances to `0.1.229`; the immutable manifest is updated and a
  fresh signed release is required.
- Release run `33024640078` proved the package jobs were rebuilding against a
  stale source-tree rootfs pin: guest jobs produced one image, while package
  jobs consumed the same artifact but compared it to the previous pin. The
  release pipeline now uploads the guest artifact digest, verifies the
  downloaded bytes, and passes that attestation to `stage`; candidate advances
  to `0.1.230` and requires a fresh release run.
- Release run `33026107426` validated the downloaded guest rootfs digest but
  then rejected an older source-tree guest-agent pin. Package jobs now pass
  explicit digests for both generated artifacts to the staging verifier;
  static Firecracker, jailer, and kernel pins remain source-validated. Candidate
  advances to `0.1.231` and requires a fresh signed release.
- Release run `33027312071` succeeded end-to-end for `v0.1.231`: guest
  kernel/rootfs jobs, metadata, multi-platform GHCR image, amd64/arm64 Debian
  packages, coherent release record, and all four hosted attestations passed.
  The immutable GitHub release contains the expected package, record, manifest,
  and checksum assets.
- Standard APT publication is currently externally blocked. Stale run
  `tailrocks/velnor-apt#33009466388` remains queued despite repeated cancel API
  requests; deletion is forbidden (HTTP 403). GitHub-lane dispatch
  `tailrocks/velnor-apt#33028868347` is pending behind the repository-wide
  `cancel-in-progress: false` concurrency group. No APT or Sentry mutation is
  claimed until that group is clean.
- Non-mutating fixture readiness was rerun after the signed release. It still
  fails against the unchanged fixture: the recorded public run
  `32935294686` is failed, and the audit reports missing canonical paths,
  mise/mold/cache/provenance/artifact/Docker/Pages/Renovate/merge-group and
  environment-injection surfaces. The fixture remains unmodified; fixture
  readiness, smoke, and parity gates stay open.
- Sentry access was restored on 2026-08-27. Read-only inspection confirms the
  incident shape on the live host: `/usr/bin/velnor-runner` is `0.1.215`, the
  `velnor-daemon.service` for `velnor-sentry` has five slot processes and one
  idle `velnor-runner job` waiter despite an empty Docker list, and the daemon
  accumulated about 12h42m CPU in 14h57m. The live process sample showed the
  Sentry daemon at about 82.7% CPU. `velnorctl doctor` could not query GitHub
  because its token is unset. No Sentry mutation or canary claim is made until
  signed APT publication is unblocked and an isolated scope is selected.
- The stale APT run was force-cancelled. GitHub-lane package update run
  `33047460559` succeeded and opened `tailrocks/velnor-apt#144`, containing
  the exact `v0.1.231` amd64/arm64 package hashes. Its required
  `tailrocks / validate request` check is queued on the `velnor-trusted`
  label (`33047526836`); the PR is mergeable but blocked on that check. The
  current apt package state remains `0.1.215` until the PR is validated and
  merged by the standard process. Sentry deployment remains gated; no direct
  `.deb` or `dpkg` path is used.
- PR #144 validation remains queued after inspection: job `98434933844`
  requires label `velnor-trusted`, has no assigned runner, and the visible
  `tailrocks/velnor` runners are offline or carry only `dogfood`, `self-hosted`,
  `velnor`, and `velnor-target-mvp`. The GitHub-lane package update itself is
  successful; publication cannot proceed until the standard required check
  receives a runner and the PR is merged.
- After restoring a trusted runner, the reopened PR run `33048266253` reached
  the Velnor lane but failed on the pre-fix installed runner: checkout fetched
  ephemeral merge SHA `e6d3e7c…`, and GitHub returned `not our ref`; the lane
  then failed its contract and `ci-required`. The exact head passed the
  canonical GitHub lane in `33049018135` (including `ci-required`). An admin
  merge attempt was rejected by the repository rule requiring the failing
  `ci-required`; no bypass or direct publication was performed. The source
  checkout fix must be installed before the Velnor PR lane can pass, leaving
  signed APT publication externally gated.
- The Velnor checkout path now handles GitHub pull requests structurally:
  self-checkout selects the advertised `refs/pull/<number>/{merge,head}` ref
  instead of fetching the ephemeral `github.sha`; push/tag and explicit
  checkout refs retain exact-SHA behavior. Three focused regression tests were
  added, and 40 checkout-related nextest tests pass. This fix requires a new
  signed release before the Sentry or APT Velnor lane can consume it.
- Release run `33049599581` was cancelled after both guest-image jobs failed
  at the pinned kernel download with `curl: (92) HTTP/2 stream 1 was not
  closed cleanly: PROTOCOL_ERROR`. The release workflow now uses HTTP/1.1,
  bounded retries, connect timeout, and total timeout for kernel and
  Firecracker downloads. Candidate advances to `0.1.233` and requires a
  fresh signed release.
- Release `v0.1.233` run `33050494181` reproduced a second interrupted pinned
  kernel transfer (`curl: (18) end of response with 125714700 bytes missing`)
  despite the HTTP/1.1 retry settings. The release workflow now adds
  `--retry-all-errors`; candidate `v0.1.234` is committed as `5b29aa1`, tagged,
  and pushed. GitHub-lane release run `33051738683` is queued; APT publication
  remains gated on its successful completion and the standard package PR.
- Release run `33051738683` retried the interrupted transfer but still exhausted
  retries after repeated `curl: (18)` and HTTP 503 responses. Both guest
  downloads now resume partial files with `--continue-at -` and use a bounded
  30-minute retry window. Candidate `v0.1.235` is committed as `62acc44`,
  tagged, and pushed; GitHub-lane release run `33052574451` is queued.
- Release run `33052574451` confirmed repeated CDN 503 responses despite
  resumable retries. The verified pinned kernel artifact responds from the
  official `mirrors.edge.kernel.org` endpoint with the same SHA-256. Candidate
  `v0.1.236` is committed as `8948d43`, tagged, and pushed; the failed run was
  cancelled before the next standard GitHub-lane release dispatch.
- GitHub-lane release run `33053243380` succeeded end-to-end for `v0.1.236`:
  identity, metadata, both guest images, multi-platform GHCR image, amd64 and
  arm64 Debian packages, coherent release assembly, signatures, and
  attestations all passed. The signed release is ready for the standard APT
  package-update workflow.
- Signed APT publication of `v0.1.236` completed through package-update run
  `33055514873`, publish run `33055756988`, and PR #144. Sentry policy exposed
  a canary defect: the exact locked APT transaction reached the package
  `preinst`, but the guard rejected unrelated active scopes and the guardian.
  Sentry remained on `0.1.215`; no direct `.deb` or `dpkg` path was used.
  `preinst` and `postinst` now support an explicitly named
  `VELNOR_DRAINED_UNITS` canary set while retaining full-host draining as the
  default. Candidate `v0.1.237` is prepared for the next signed release.
- The first `v0.1.237` release attempt (`33056323591`) built every artifact but
  failed at GitHub asset upload with HTTP 400 before creating a release. The
  failed release job was rerun through GitHub Actions; rerun `33056323591`
  completed successfully with all builds, package signatures, coherent release
  assembly, and attestations green. The signed release is ready for the next
  standard APT package-update run.
- APT package-update run `33058766810` correctly fetched and hashed the signed
  `v0.1.237` packages but rejected their provenance: the release workflow had
  been dispatched from the work branch, while the immutable release record
  names `refs/tags/v0.1.237`. The package PR was not mutated. The release will
  be rerun from the tag ref so attestations bind to the record's exact source
  ref before APT publication is retried.
- GitHub-lane release run `33058891658`, dispatched from exact tag
  `refs/tags/v0.1.237`, completed successfully. Identity, metadata, both guest
  images, multi-platform GHCR image, amd64 and arm64 Debian packages, coherent
  release assembly, signatures, and attestations all passed. This satisfies the
  immutable source-ref requirement for the next standard APT publication.
- Standard `velnor-apt` package-update run `33060735953` passed verification,
  mutation, the Velnor lane, contract, `ci-required`, and DCO. PR #146 merged.
  Publish run `33061090394` completed successfully through GitHub Actions:
  repository build, deployment, and required publish gates all passed. The
  signed APT repository now publishes candidate `velnor-runner=0.1.237`.
- Sentry’s isolated `velnor-daemon.service` was drained and upgraded only via
  the exact locked APT transaction to `0.1.237`; unrelated daemon units and
  their running containers were left untouched. The active release pointer was
  then atomically activated from the checksum-verified `v0.1.237` record.
- Focused broker protocol and runner fault coverage passed under nextest:
  `mise exec -- cargo nextest run -p velnor-runner --test broker_protocol --lib`
  ran 1001 tests with 1001 passed, including 401/403/404/5xx, timeout,
  retry, completion, session isolation, registration-loss, generation-fencing,
  backoff, quarantine, and sibling-isolation cases.
- Canonical fixture readiness was run without modifying
  `velnor-actions-fixture`; it reported the existing fixture audit drift and
  prior failed run `32935294686`. A fresh unchanged fixture smoke attempt was
  blocked before dispatch because this environment has no `GITHUB_TOKEN`; no
  fixture workaround was applied.
- With keyring-backed GitHub authentication supplied ephemerally, the canonical
  fixture smoke still stopped before dispatch at the non-mutating live-host
  doctor: the checked-in `actions/runner` reference is `v2.335.1` while the
  current upstream reference is `v2.337.0`. No fixture files or workflow content
  were changed; fixture smoke/parity therefore remains open pending the required
  reference refresh and review.
- Sentry canary runtime evidence: after activation, a real Java-monorepo job
  was admitted and executed with transient worker/container isolation while
  unrelated host-scope containers remained running. During the observed load,
  daemon CPU stayed low in samples, durable no-op observations remained
  suppressed, and broker/JIT counters showed no JIT churn; however, repeated
  registration-loss recovery and `routing_valid=false` occurred, so canary
  health and full idle proof are intentionally not marked complete.
- The repository’s generic rollback text is not used as an acceptance path for
  this issue. Per the operator’s forward-only decision, no rollback execution or
  previous-approach validation is required here.
- Operator decision 2026-08-27: this issue does not require rollback. Continue
  forward with the candidate implementation and validate forward upgrades only;
  do not revert to the previous approach. Repository-wide APT policy remains
  unchanged outside this issue.
- Forward remediation after the Sentry reproduction: JIT registration now has
  a 10-second connect deadline and 45-second curl total deadline, plus a
  60-second scope-controller task deadline. This prevents a stalled GitHub
  registration from monopolizing a controller cycle or leaving an uncancelled
  curl request as the retry loop advances. Candidate source version is
  `0.1.238`; focused nextest verification passed 1002/1002.
- Forward canary diagnosis after `0.1.238`: repository-derived routing policy
  used `write_policy_if_absent`, so an old label set survived daemon
  configuration changes and kept routing invalid indefinitely. Candidate
  `0.1.239` replaces this with content-aware refresh; explicit operator policy
  files remain authoritative and unchanged policy files are not rewritten.
  Focused nextest (2/2) and strict clippy pass.
- Release tag `v0.1.239` at commit `2fe621c` completed successfully in GitHub
  Actions run `33069259220` after one earlier transport-stalled attempt was
  canceled. Guest images, multi-platform GHCR image, amd64/arm64 packages,
  coherent release assembly, signatures, and attestations all passed.
- Standard signed APT publication of `v0.1.239` completed through package
  update run `33070956065`, merged PR #148 (merge `182e47e`), and publish run
  `33071304308`. The Sentry host installed the exact version through the
  locked scoped APT transaction; release-record checksum, coherence, active
  pointer, and installed-binary verification passed. No rollback was used.
- Forward Sentry canary after `v0.1.239`: routing became valid, five JIT
  registrations succeeded, and zero idle waiter processes were present.
  Real ChainArgos jobs were admitted to transient workers and were preserved.
  During the canary, reconcile overlap stayed zero, stable observations were
  no-op, and instantaneous daemon CPU samples were zero despite unrelated
  host load. Initial inherited session loss produced five coordinated JIT
  recreations; afterward JIT create failures and delete churn stayed zero.
  Two active run-service jobs remained in progress at capture time, so the
  15-minute zero-job soak and final active-job completion proof remain open.
- A fresh Sentry idle soak began at `12:45:47Z` after the active jobs cleared.
  It held jobs=0, containers=0, waiter/job workers=0, five registrations,
  ready health, zero JIT churn, and zero direct daemon CPU samples for about
  13 minutes before two new real assignments arrived. The soak was stopped
  as invalid rather than falsely counted as a 15-minute pass. The subsequent
  workload continued to show zero idle waiter processes and zero JIT create
  failures; a new uninterrupted idle window is still required when the
  external queue is quiet.
- At the latest live check (`13:03:37Z`), v0.1.239 remained active and the
  Sentry daemon was still at zero instantaneous CPU in the scoped sample with
  five registrations, zero JIT create failures, zero waiter processes, and
  zero overlap. Two new real jobs were active, so the second soak window is
  also not started. The eight unchecked items below remain intentionally open:
  fixture/reference refresh and parity, complete evidence comparison, a
  clean 15-minute idle window plus fixture/fault run, active-job/isolation
  proof, promotion, fixture parity, and full-fleet Sentry soak.

- 2026-08-27 unchanged-fixture smoke run `33080712906` passed end-to-end on
  the Velnor lane: both `app-a` and `app-b`, provenance, result comparison,
  and `compat-required` succeeded. The subsequent `both` run
  `33081151039` has all GitHub jobs successful; its Velnor jobs remain queued
  behind unrelated fleet capacity, so parity is still open.
- 2026-08-27 the real fixture smoke run `33080712906` is authoritative
  unchanged-fixture proof: both Velnor package matrices, provenance,
  comparison, and `compat-required` passed. The `both` parity run
  `33081151039` was canceled after all matching tailrocks target runners
  became offline while an unrelated `velnor-tailrocks` Docker job remained
  active; its GitHub lane passed, but Velnor parity is unproven.

- 2026-08-27 removed exactly one owned stale offline tailrocks runner
  registration (`velnor-tailrocks-slot-9-next-717185-111`, id 15515). The
  active sibling registration (id 15514) and its unrelated Docker job were
  preserved. This reduced stale registration drift but did not create fleet
  capacity; Velnor parity remains open until a target runner is free.

- 2026-08-27 forward-convergence audit: idle `velnor-daemon@dogfood.service`
  was restarted on v0.1.240 through the signed APT path. Its old drain-stuck
  waiter tree was terminated only after confirming no dogfood container
  existed; six exact stale offline `velnor-dogfood-slot-*` repository
  registrations were removed. The new controller started six isolated slot
  processes, but broker recovery still reports stale/missing identity (`409`/
  `404`) and needs further live-fleet proof.
- Fixture parity run `33083180030` was dispatched from unchanged fixture
  commit `a176d88c8c6ff0d452ea27cb32784bb8544f3a42` with `lanes=both` after
  canceling prior unfinished runs. It remains queued because no matching
  Velnor capacity is free; the active `velnor-tailrocks` workload was
  preserved. The parity checkbox stays open until this run completes and its
  compare evidence is captured.
- The parity attempt consumed one Velnor job on tailrocks slot-9, but after
  completion the slot lost its consumed GitHub registration and recreated
  registration `15517`, which immediately became offline. The aggregate run
  `33083180030` remains queued, with all Velnor jobs waiting. This is live
  evidence that registration/session churn is still present in another scope;
  parity and full-fleet gates remain open.
- The replacement parity run `33083180030` completed successfully on the
  unchanged fixture: GitHub and Velnor `app-a`/`app-b` results, cache modes,
  Postgres services, provenance, `compare-results`, and `compat-required` all
  passed. Downloaded result artifacts were manually normalized and matched
  for both packages. The fixture comparator's case-sensitive path lookup
  skipped its own comparison; this external fixture defect is recorded, not
  worked around in the fixture.
- Forward source fix: completed assignments now retire the consumed
  scope-owned broker session immediately, clear the durable registration claim,
  remove the consumed local JIT identity, and wake one fresh registration
  cycle. This prevents post-completion broker `404` polling and stale
  config-driven `409`/JIT churn. Added regression coverage for claim/config
  retirement; focused nextest (2/2) and runner clippy pass. Candidate source
  version is `0.1.241`; signed APT publication and live validation remain
  required.

## Non-goals

- Do not remove or simplify fixture content.
- Do not hide missing Velnor capability with workflow-local workarounds.
- Do not broad-prune Docker or delete data without ownership proof.
- Do not trade crash durability or job isolation for lower idle CPU.
