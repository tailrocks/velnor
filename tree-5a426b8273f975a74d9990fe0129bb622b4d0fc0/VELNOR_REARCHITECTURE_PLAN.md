# Velnor Rearchitecture — Master Plan

Living engineering source of truth for the Velnor rearchitecture program. Temporary
document for the duration of this effort; it is deleted when the work lands.

## 0. Immutable starting points

| Repository | Branch | Starting SHA (resolved live) |
| --- | --- | --- |
| `tailrocks/velnor` | `perf/docker-rust-mbx` | `2858e92df0eb78df4f1a6fe2ad4cbf86f1d56355` |
| `tailrocks/velnor-actions-fixture` | `codex/verifier-completion-fixes` | `5c8b57aa64dcbfd8fe6b2f6edae625ae344fc496` |

Both branch heads were re-resolved against `origin` at program start and matched the
SHAs recorded in the goal, so no newer head substitution was required.

Upstream semantic source of truth: `actions/runner`, resolved live to the latest
stable release **v2.337.0** (published 2026-08-26), source commit
`397b032cbf865e9c3ddfab89d533ec19325e1273`. Velnor pins the same version in
`crates/velnor-runner/src/protocol.rs:30`, and the pin is drift-gated in code.

## 1. Coordination protocol (two concurrent leads)

Two leads run independent multi-agent efforts against the same branch lines. To keep
that safe:

1. **Shared branches.** All work lands on `perf/docker-rust-mbx` (Velnor) and
   `codex/verifier-completion-fixes` (fixture). No private long-lived forks.
2. **Isolated working trees.** Each lead uses its own git worktree so nobody's
   checkout is switched or dirtied underneath a running agent. Never `git checkout`
   a different branch in a checkout you do not own.
3. **Small, frequent, focused commits, pushed immediately.** Rebase on
   `origin/<branch>` before every push; never force-push a shared branch.
4. **Ownership table.** Before starting implementation on an architectural boundary,
   claim it in §7. Do not write to a boundary another lead has claimed; escalate to
   the coordinator instead.
5. **Read-only investigation is unclaimed.** Any lead may investigate anything.
   Only *writes* need a claim.
6. **This file is append-mostly.** Add rows and sections rather than rewriting
   existing ones, so concurrent edits merge cleanly.

## 2. Method

Per the program rules:

- Opus agents own investigation, architecture, review, and final verification.
- Sonnet agents own bounded implementation tasks with explicit invariants.
- Every substantial implementation batch gets an independent Opus code review.
- No work package is accepted while an Opus reviewer holds an unresolved
  correctness or architectural finding.
- Work is judged by correctness, coherence, and goal fit — never by effort or diff
  size. A known-wrong state is never deferred as low-value, marginal, or rare.
- Every bug is treated as architectural evidence: identify the missing invariant and
  the boundary that permitted the bug class before proposing a fix; prefer removing
  the enabling condition over a symptom patch.

## 3. Investigation matrix (wave 1, in flight)

| ID | Scope | Report |
| --- | --- | --- |
| I-01 | Runner slot + job lifecycle state machines | `01-lifecycle.md` |
| I-02 | GitHub V2 broker/run-service protocol, completion durability, crash recovery | `02-protocol-completion.md` |
| I-03 | GitHub workflow semantic model: expressions, conditions, outcome/conclusion, contexts | `03-semantics.md` |
| I-04 | Cancellation and timeout semantics | `04-cancellation.md` |
| I-05 | Docker runtime architecture; CLI vs Engine API; per-job call counts | `05-docker.md` |
| I-06 | Rust acceleration: Mr. Boxington, Cargo persistence, fingerprint stability, trust | `06-rust-mbx.md` |
| I-07 | Toolchain and dependency freshness; supply chain | `07-deps.md` |
| I-08 | Admission serialization, resource scheduling, async/blocking architecture | `08-concurrency.md` |
| I-09 | Verifier coverage architecture and Rust migration | `09-verifier.md` |

Wave 2 (queued): job image architecture, BuildKit lifecycle, checkout/filesystem
performance, cache and trust model, disk ownership and GC, action resolution and
JS/Docker/composite execution, results/artifact/timeline publishing, crate and module
organization, security and cross-job isolation, observability cost, benchmark
methodology, fault injection and soak design, production operability.

Wave 3: independent Opus architecture synthesis, then an independent Opus red-team
review of the synthesized target architecture, before any broad implementation.

## 4. Current architecture

Described through the bug classes in §8 and consolidated in
`.rearch/reports/20-synthesis.md`. The sixteen investigation reports are in
`.rearch/reports/`.

## 5. Target architecture

See §5 (synthesized) below, and `.rearch/reports/20-synthesis.md`.

## 6. Architectural invariants

28 invariants with their enforcing mechanism are stated in
`.rearch/reports/20-synthesis.md`; 11 are compiler-checked. Summarized in §5 below.

## 7. Ownership claims

Claim a boundary here before writing to it. Read-only investigation needs no claim.

| Boundary | Lead | Status |
| --- | --- | --- |
| `rust-toolchain.toml` (toolchain channel pin) | opus-lead | claimed — T-001 |
| `protocol.rs` run-service/broker error contracts | opus-lead | claimed — T-002 |
| `deny.toml` + `mise.toml` deny task | opus-lead | claimed — T-003 |
| `crates/velnor-runner/Cargo.toml` tokio feature set | opus-lead | claimed — T-003 |
| `crates/velnor-runner/src/docker_lease.rs` (job Docker request authorization and unlabeled cleanup) | codex-lead | complete — T-004 (`3749065`) |
| `crates/velnor-runner/src/cache.rs` (explicit storage context for reclaim enumeration) | codex-lead | complete — T-017 (`dd93963`) |
| `crates/velnor-runner/src/manifest.rs` (Mr Boxington action-specific input literals) | codex-lead | complete — T-018 (`8434d05`) |
| `crates/velnor-runner/src/action.rs` + `manifest.rs` (approved remote action runtime-kind classification) | codex-lead | claimed — T-019 |
| `crates/velnor-workflow/**` + workspace registration (workflow generator/client) | codex-lead | claimed — T-020 |
| `crates/velnor-tools/src/workflow_monitor.rs` + approved `velnor-tools → velnor-client` read edge | codex-lead | claimed — T-020 |
| `.github/workflows/velnor-workflow-policy.yml` (immutable reusable workflow-policy gate) | codex-lead | claimed — T-020 |
| `.github/workflows/ci.yml` (manual-dispatch runner admission) | codex-lead | claimed — T-020 |
| `.github/workflows/docs.yml` (manual-dispatch runner admission) | codex-lead | claimed — T-020 |
| `.github/workflows/renovate.yml` (manual-dispatch runner admission) | codex-lead | claimed — T-020 |
| `crates/velnor-bench/src/record.rs` (derived-summary integrity) | codex-lead | claimed — T-021 |
| `crates/velnor-bench/src/sys.rs` + `drivers/cargo.rs` (Cargo measurement environment) | codex-lead | claimed — T-022 |

## 8. Discovered bug classes

### BC-1 — Wire contract copied from C# property names, not `DataMember` names

`crates/velnor-runner/src/protocol.rs:1814-1823` parses the run-service error body as
`{"source", "code"}`. Upstream `src/Sdk/RSWebApi/Contracts/RunServiceError.cs` at
v2.337.0 serialises `{"source", "statusCode", "errorMessage"}` — the C# *property* is
named `Code`, but the wire name is `statusCode`. Velnor copied the property name.
Verified directly against upstream source, not documentation.

Consequence: `CompletionAcknowledgement::RemoteObservedTerminal` is unreachable in
production, so a genuine "job already terminal" 404 is classified as a permanent
failure instead of being absorbed.

The bug class is broader than this one field: any contract transcribed from upstream
C# property names rather than `DataMember` names has the same defect, and Velnor's own
tests fabricate the same wrong shape (`protocol.rs:7717`, `:7764`), so the test suite
structurally cannot catch it. The invariant that was missing: *wire field names must be
derived from the upstream serialisation attribute, and fixtures must be built from the
upstream contract rather than from Velnor's own parser.*

### BC-2 — Outbox rows have no terminal state other than success

Chained from BC-1. A completion that can never be acknowledged leaves the outbox row at
`send_started=1, remote_acked=0`. `pending_outbox_blocks_admission`
(`crates/velnor-control/src/journal.rs:195`) then rejects every slot-admission event
forever, and the controller's `reconcile` propagates the replay error every cycle
(`crates/velnor-runner/src/node/controller.rs:1551`). The `Event` enum
(`journal.rs:366-482`) has no abandon/expire variant. One unacknowledgeable completion
permanently wedges a slot.

Missing invariant: *every durable in-flight record must have a bounded path to a
terminal state.*

### BC-3 — Lost-completion window between acquire and durable marker

`acquirejob` returns 200 at `crates/velnor-runner/src/runner.rs:5145`; the durable
marker is written at `runner.rs:5328`. A crash in that window leaves no local record at
all: no renewal, no completion, and the job is lost until lease expiry.

Missing invariant: *ownership must be durably recorded no later than the moment it is
acquired.* Target fix is a provisional marker written **before** `acquirejob`, using the
409 "already acquired" reply as the ownership oracle.

### BC-4 — Toolchain pinned to a `.0` release with no channel freshness manager

`rust-toolchain.toml:2` pins `1.98.0`. The actual latest stable is `1.98.1`
(2026-09-01, released 2026-09-03) whose entire content is a fix for a **vtable
miscompilation** (rust#161441). Velnor dispatches execution backends through trait
objects, so this is on the hot path, and every artifact currently shipped is built with
the defective release. `1.97.1` was also a miscompilation backport, so `.0` pinning is a
repeating structural exposure — and Renovate has no manager covering the toolchain
channel. The class fix is the missing manager, not just the bump.

## 9. Observed bottlenecks and benchmark baseline

_Pending benchmark architecture._

## 10. Task graph

_Pending synthesis._

## 11. Status log

| Date | Event |
| --- | --- |
| 2026-09-04 | Branch heads re-resolved; both matched the recorded starting SHAs. |
| 2026-09-04 | Wave 1 investigations launched (I-01 … I-09). |
| 2026-09-04 | Wave 2 investigations launched (I-10 … I-16). |
| 2026-09-04 | I-02 (protocol/completion) and I-07 (dependency freshness) reported; bug classes BC-1 … BC-4 recorded. |
| 2026-09-04 | T-018 narrowed Mr Boxington backend admission to the action's accepted `github`/`server` values and added a rejection proof for `local` (`8434d05`). |
| 2026-09-04 | T-017 removed ambient storage-layout coupling from cache reclamation; the full 1,474-test runner suite now passes in parallel (`dd93963`). |

### BC-5 — Four disjoint lifecycle models, none of which is the control flow

`ActorPhase` (`crates/velnor-model/src/node.rs:313`) is a real generation-fenced machine.
`SlotPhase` (`node/phase.rs:33`) and `JobState` (`node/lifecycle.rs:167`) are write-only
projections — `SlotPhase::{Acquiring,Running,Finalizing,WaitingForCapacity,…}` are never
emitted, so `is_busy()` is structurally always false and the durable projection can never
show a busy fleet. What actually executes a job is an 826-line function
(`crates/velnor-runner/src/runner.rs:5286-6111`) driven by booleans, `Option`s, atomics,
flocks and file markers.

Consequences already proven from this structure:

- **Fencing an executing slot loses the job.** `fence_stale_slot_actor`
  (`node/controller.rs:2121`) emits `Event::SlotStale` without checking for an active job,
  and kills the *heartbeat* pid rather than the worker. The generation bump then makes
  `CompletionIntended` fail `outbox_owner_is_proven`
  (`crates/velnor-control/src/journal.rs:783`, `:211`), so the completion is never sent and
  the job hangs at GitHub.
- **Slot liveness is proven by the wrong process** (`node/slot.rs:64-96`): the heartbeat
  process only writes a heartbeat; the process that runs jobs has no heartbeat of its own.
- **The two modules that model fencing and recovery correctly — `node/handoff.rs` and
  `node/recovery.rs` — have zero call sites**, while `controller.rs` reimplements weaker
  logic inline.

Missing invariant: *there is exactly one lifecycle model, and it is the one the control
flow is expressed in.* Target is a typestate `Job<S>`
(`Notified → Acquired → Admitted → Reserved → Prepared → Running → Publishing → Staged →
Sent → Acked → ToreDown`) where `JobCredentials` is constructible only from a
`DurableAdmission`, `Running` requires a `Reservation` and scope leases by value, the send
claim is non-`Clone` and consumed by `send()`, and `fence()` refuses an occupied slot *by
signature* — making the fencing defect a compile error.

### BC-6 — Cancellation is a container assassination side channel, not a model

Upstream v2.337.0 models cancellation as an in-process token: `StepsRunner.cs:146-187` sets
`JobContext.Status = cancelled` and re-evaluates every step's `if:`, so `always()` and
`cancelled()` become true while `success()` and `failure()` become false; post steps run
under **fresh unlinked** cancellation sources (`ExecutionContext.cs:436`, `:1384-1395`);
processes are terminated SIGINT → 7.5 s → SIGTERM → 2.5 s → SIGKILL
(`ProcessInvoker.cs:32-33`, `:443-447`).

Velnor's entire mechanism is `runner.rs:6593-6595` → `kill_job_container`
(`runner.rs:7146-7171`): an unbounded `docker kill` (SIGKILL) against exactly two container
names. `execute_script_job` (`runner.rs:7189-7205`) takes no cancellation token at all; the
flag is read only *after* the executor has returned (`runner.rs:5930-5952`).

P0 consequences: a cancelled job keeps running ordinary subsequent steps (deterministic
with `continue-on-error`, `executor.rs:2174-2176` → `:11341-11350` → `:11725-11727`);
`cancelled()` is hardcoded `false` (`executor.rs:11729-11731`) so `if: cancelled()` never
runs and `if: failure()` wrongly does; there is no graceful termination anywhere, although
`node/controller.rs:46`, `:567-596` already implements SIGTERM → 5 s → SIGKILL for another
purpose; MicroVM jobs are entirely uncancellable while GitHub is told `Canceled`; the whole
typed cancel API (`ExecutionSession::cancel`, both backend `cancel()`s,
`VsockMessage::Cancel`) is dead code reachable only from `execution/tests.rs:542`; and job
`timeout-minutes` is never a wall clock (`executor.rs:12828-12837`) — a 10-minute job with
20 steps can run 200 minutes.

Three documentation statements overclaim and must be corrected with the fix:
`content/docs/guides/execution.mdx:187`, `content/docs/guides/integrations.mdx:98` and
`:119`.

Missing invariant: *cancellation is a first-class input to execution, and every process,
container, daemon and child it spawned is reachable from one termination ladder.*

### BC-7 — Three Docker access paths coexist, and the fast one is the subprocess

Velnor talks to Docker three ways simultaneously: the `docker` CLI as a subprocess for the
whole job lifecycle; a hand-rolled ~2600-line HTTP/1.1 Engine API client over the Unix
socket (`docker_lease.rs:916-2600`) aimed at the *guest*, not the runner; and nested
CLI-in-CLI, where buildx, login and build-push run as
`docker exec <job> sh -c "docker buildx …"` (`executor.rs:3193-3227`) — two processes per
logical operation.

Measured call counts: a minimal job spawns **12 + N** host `docker` processes (N = script
steps); a representative job (2 services, 15 steps, buildx + login + push, 1 action) spawns
**52-72**, plus up to 64 lease threads. A transient daemon restart replays the sequence up
to five times: **150+ processes for a job that ran zero steps.**

P0 items within the class: no cancellation of the runner's own Engine work (a *client* is
SIGKILLed and container names are recovered by argv archaeology,
`executor.rs:12852-13030`, even though the lease already has the right primitive at
`docker_lease.rs:1711-1719`); control-plane calls inherit a **360-minute** timeout so a
stalled dockerd parks a slot for six hours; `docker info` — the Engine's heaviest endpoint
and a daemon-generation fact — runs once per job uncached (`execution/docker.rs:124`); the
cgroup-proof cache is invalidated by *any* non-zero docker exit including an ordinary
failing user step (`executor.rs:406-410`), so the cache key and its invalidation trigger
are unrelated facts; no typed lifecycle ownership, with go-template `--format` strings
re-parsed by at least 11 functions; and unbounded output buffering that buffers full output
even on streaming calls.

Missing invariant: *one typed Docker client owns the host control plane, every call has an
operation-appropriate deadline, and every fact is cached against the generation that can
invalidate it.*

### BC-8 — Unsupported checkout inputs are accepted and silently ignored

Velnor replaces `actions/checkout` with a native implementation intercepted on action
identity at any version (`crates/velnor-runner/src/checkout.rs:679-687`), and a second,
divergent shell implementation exists for the MicroVM guest
(`execution/guest_actions.rs:63-122`). Admission never gates unknown inputs
(`admission.rs:590-596`).

`submodules` is silently ignored — there is no `git submodule` call anywhere in
velnor-runner — while the step log asserts `submodules: false` even when the workflow asked
for true (`executor.rs:12183`), and the capability auditor lists it as *supported*
(`crates/velnor-tools/src/main.rs:2110-2117`). `sparse-checkout`, `filter`, `ssh-key`,
`github-server-url` and `set-safe-directory` are likewise accepted and ignored;
`github-server-url` being ignored makes an external-repo checkout on GHES fetch from
github.com.

Missing invariant: *an input Velnor does not implement is rejected at admission, never
accepted and dropped.* This is the fail-early requirement of the capability model, and it
is also why the capability manifest currently certifies behavior that does not exist.

### BC-9 — Credentials outlive their workspace

The credentialed `.git/config` survives on the host if the runner dies between the last
step and cleanup (`runner.rs:8126`) or if `remove_job_workspace` fails; cleanup failure is
downgraded to a stderr warning (`checkout.rs:311-318`) and there is no stale-job-dir
reaper. The MicroVM guest checkout writes the token into workspace config with **no cleanup
path at all** (`execution/guest_actions.rs:89-92`).

Missing invariant: *a credential's lifetime is owned by a guard that cannot be skipped by a
crash path, and cleanup failure is an error, not a warning.*

### BC-10 — Checkout does network and byte work it does not need to

The daemon-wide bare mirror fetches `+refs/*:refs/*` at full depth on every job
(`git_mirror.rs:49-57`) — on GitHub that includes every `refs/pull/*` — so cold start is
strictly worse than upstream's `--depth=1` single-ref fetch. There is no "sha already
present" short-circuit, and an exclusive `flock` is held across the entire network fetch
(`checkout.rs:161-177`, `git_mirror.rs:41`), so an N-way matrix on one commit performs N
*serialized* full fetches. Workspace objects are then **physically copied** out of the
mirror every job (verified empirically: loose objects land in `ws/.git/objects`) with no
alternates, `--shared`, `worktree add` or reflink — despite `fs_copy.rs:1254`/`:1266`
already providing `ioctl_ficlone`/`fclonefileat` and using them for the Cargo `target/`
restore. The mirror key omits the host (`git_mirror.rs:64-83`), so the same `owner/repo` on
two hosts share one mirror and force-update each other, and there is no corruption
detection beyond `HEAD` existence (`git_mirror.rs:44`), so a broken mirror silently
degrades every future job forever.

Missing invariant: *no duplicate fetch without a correctness reason, no byte copied that
could be shared, and no lock held across the network.*

### BC-11 — Expression evaluation is string rewriting, not a typed evaluator

Velnor rewrites expression source over `Option<String>` with a fail-open tail instead of
parsing to a typed value union as upstream does. The divergences are therefore a family,
not a list: `"0"`/`"false"` are falsy where upstream makes them truthy
(`executor.rs:12762-12768` vs `Sdk/DTExpressions/EvaluationResult.cs:50-75`); there is no
null coercion, so an unknown expression evaluates to its own source text
(`executor.rs:11817`); the relational operators `<`, `<=`, `>`, `>=` do not exist;
`startsWith`, `endsWith`, `join` and `fromJSON` do not exist
(`Sdk/DTExpressions/ExpressionConstants.cs:10-20`); and a condition that fails to evaluate
is fail-open where upstream fails the step (`Runner.Worker/StepsRunner.cs:237-242`).

Each of these silently changes control flow. `if: startsWith(github.ref, 'refs/tags/')` on
a branch push is skipped by GitHub and **runs the release step** on Velnor.

Missing invariant: *expressions are parsed and evaluated over the upstream value model, and
an evaluation error fails the step.*

### BC-12 — `(outcome, conclusion)` is not a value

There is no `StepResult` type carrying outcome and conclusion as a pair. `failure_ignored`
is threaded beside `exit_code` through five structs, and each hop re-derives the
conclusion. That is why the same defect keeps reappearing at new sites: parent
`continue-on-error` is pushed into composite inner steps
(`runner.rs:9334`, `:9459`), so a composite keeps running past a failure GitHub would stop.

The originally-reported `continue-on-error`/outcome/conclusion incident is genuinely fixed
on this branch end-to-end (`executor.rs:11334-11352`, `:2172-2176`, `:2209-2232`,
`:11667-11691`, `runner.rs:11193-11201`, regression test at `runner.rs:17702-17706`), and
job-level `continue-on-error` is correctly ignored because upstream has no worker-side
consumer either. The *instance* was fixed; the *class* was not.

### BC-13 — JavaScript actions are not executed

`ExecutableStep::JavaScript` is never constructed by the planner; every JavaScript action
reaches `bail!` at `runner.rs:9474-9479`. The Node runtime, action `pre`/`post`, and the
`GITHUB_STATE` → `STATE_*` round trip exist but are exercised only by tests. Velnor instead
emulates an allowlist of well-known actions through `native_*` adapters. Since GitHub
Actions compatibility requires executing third-party JavaScript actions, this is a missing
capability being reported as a supported one.

### BC-14 — Rust acceleration can accept stale artifacts as fresh

`checkout.rs:200-263` pins every file *and directory* mtime to
`git log -1 --format=%ct`. Cargo decides workspace freshness by comparing source mtime
against dep-info mtime and never hashes workspace sources, so the pin inverts the direction
Cargo's model depends on: committer dates are not monotonic across jobs sharing a store, and
no store key contains a ref. A branch switch to an older commit, an old-ref re-run, a tag
build, and a rebase or force-push each yield **stale artifacts accepted as fresh**. On
GitHub-hosted runners this cannot happen because checkout leaves sources at wall-clock now.
Directory pinning additionally defeats `rerun-if-changed=<dir>` add/remove detection.

A second, unconditional path: `actions/cache` on `target/` is admitted
(`executor.rs:5292-5307`, `:5671-5675`) and the restore preserves no mtimes, so restored
dep-info is newer than every pinned source.

The enabling condition is an ephemeral workspace (`runner.rs:8471-8482`) combined with a
persistent target directory. The persistent-target layer gates on the exact HEAD SHA
(`executor.rs:6488-6510`); the default mbx managed target has no such gate.

### BC-15 — Trust class is a daemon flag, not a property of the job

`trust_scope` is a daemon-level flag defaulting to `trusted` (`service.rs:192-194`) and
nothing derives it from the GitHub event. A fork PR and a release build for the same
repository therefore share one writable mbx store with no authenticity layer — digest
verification binds a blob to its own digest only. `content/docs/guides/execution.mdx:131-137`
claims a per-job "admitted trust class" that does not exist.

### BC-16 — Admission fails acquired jobs because another slot is busy

`runner.rs:8944` takes a **process-global one-permit** semaphore (`:118-119`) with
`try_acquire_owned()` — no wait, immediate `Err` — and the failure path is not a retry:
`runner.rs:5591-5605` calls `complete_acquired_job_failure(…, "action_admission", …)`, so
the user gets a red ❌ on their PR purely because another slot was admitting at that
instant. The guarded work (`admit_job_closure_sync`, `:8958-8983`) is **entirely
read-only**: blocking `reqwest` GETs of `action.yml` from the Contents API
(`admission.rs:367`, `:388-392`, `:406-460`) recursed over the action closure, holding the
single permit for up to the 65 s admission timeout. Nothing in it requires mutual exclusion.

SQLite admission has the identical fail-closed shape (`runner.rs:164-169` →
`InfrastructureFailure`, category `operational_store` at `:5400-5420`), and the repo's own
test asserts the behavior (`runner.rs:14795-14812`). `Store` already guarantees a single
writer through `Mutex<Connection>` + WAL + `busy_timeout` (`store/mod.rs:81`, `:41`), so
the semaphore adds no correctness — only failure.

Root cause of both: **one undifferentiated blocking pool.** `main.rs:16-20` builds the
runtime with default `max_blocking_threads`, and `runner.rs:5883` submits the entire job
execution — minutes to hours — to the same pool as millisecond SQLite writes. The authors
correctly feared queueing behind long work and chose fail-closed permits instead of pool
partitioning. Removing the permits without partitioning the pool would trade job failures
for job stalls, so the two must be fixed together.

The wait is also completely unobservable: no span, timestamp, duration or contention
counter exists around `runner.rs:5399` or `:5591`.

### BC-17 — No coherent resource model

The job cgroup slice is provisioned at `host_cpus × 95%` regardless of slot count
(`execution/docker.rs:172-176`); Velnor never injects `CARGO_BUILD_JOBS` or `-j`; and the
only CPU reading is `getconf _NPROCESSORS_ONLN` (`execution/docker.rs:155`), which ignores
cgroup quota and cpuset. `--slots 4` on a 16-core host therefore runs roughly 64 compiler
processes against 15.2 cores of quota: everything is runnable, nothing progresses, and
nothing explains why. Mysterious idleness in its most literal form.

### BC-18 — The verifier cannot fail, so it certifies nothing

Every field the fixture compares across lanes is a literal copied out of workflow YAML
(fixture `crates/.../evidence.rs:51`, `:60-65`; `write-result.py:14-24`), so the
GitHub-hosted lane and the Velnor lane emit byte-identical JSON *by construction*. The
comparators can fail only when a lane is missing entirely; `backend-parity.yml:166-167`
even removes its single environment-derived field before comparing. Normalization then
drops `runner` and `lane` as recursive subtrees at any depth
(`compare-results.py:16`, `:70-79`; `workflow_evidence.py:15-17`, `:20-30`), so an entire
namespace can conceal a mismatch, and a single-lane compare returns success without
comparing anything (`:118-123`).

Readiness has no freshness gate either: `audit()`
(`audit_capability_coverage.py:1139-1171`) reads three JSON files and some workflow text,
and evidence validation is `is_file()` (`:274-286`) — no run id, no timestamp, no API call.
Evidence from any earlier run, or from a different Velnor build, certifies the current one.

Missing invariant: *evidence is a collected fact carrying provenance the workflow author
cannot fabricate, comparison is over observed behavior rather than echoed literals, and the
oracle has tests proving it rejects bad input.*

This is a program-level P0: until it is fixed, no other result in this effort can be
called verified.

### BC-19 — The capability baseline is pinned, and the pin cannot detect drift

The fixture pins manifest v10 / `2fad3ffb…` (`audit_capability_coverage.py:25-26`,
`coverage/velnor-capabilities.json:2-3`) while the runner under test is v11
(`crates/velnor-runner/src/manifest.rs:18`). `crate_version` reads `0.1.250` on both sides,
so the version pin gives a false all-clear, and `validate_manifest` (`:167-232`) compares
checked-in JSON against Python constants without ever reading Velnor.

Kache was removed from Velnor at v11, yet `compat.yml:239-260` still executes
`kunobi-ninja/kache-action` and `audit_capability_coverage.py:666-693` still enforces it as
a contract. The admitted-action count stayed exactly 30 across that breaking change — a
one-for-one swap with mbx — so `EXPECTED_ACTION_COUNT = 30` (`:27`) passed. Cardinality
cannot detect identity drift. Seven further rows record coverage citing `_rust-suite.yml`,
which never mentions the action being cited.

Missing invariant: *the baseline is derived from the Velnor commit under test, and identity
is compared, not cardinality.*

### BC-20 — The two repositories have locked each other into testing the fallback path

All 11 Rust jobs in the fixture set `RUSTC_WRAPPER: sccache` and invoke
`mozilla-actions/sccache-action` (`_rust-suite.yml:32-41` and ten others), which forces
Velnor's `MBX_DISABLE=1` branch (`github_adapter.rs:78-83`, `container.rs:71-95`);
`mbx_store_host` is `None` in every fixture job. The default Velnor Rust path — the single
most important workload in this program — has **zero** coverage.

It is not a fixture-side accident. Velnor's own verifier *requires* the arrangement:
`crates/velnor-tools/src/main.rs:803-830` asserts that the fixture contains
`sccache-action@` and `cache-sccache:`. Each repository now enforces the other's use of the
fallback. Job-level `RUSTC_WRAPPER` additionally makes every Rust job microVM-ineligible
(`manifest.rs:1372-1385`), while coverage still claims `microvm: supported` for sccache.
`jdx/mr-boxington-action` has 16 admitted inputs (`manifest.rs:274-291`, `:423-432`) and no
coverage row, no surface row, and no workflow reference.

Missing invariant: *the default path is the primary scenario; compatibility modes are
separate scenarios; and neither repository asserts the other's use of a compatibility mode.*

### BC-21 — Fork pull requests can write state the next trusted job executes

**[RESOLVED in `dc06705` — see §15. Retained for the record.]**
`cargo_target_trust_scope()` read the daemon environment variable `VELNOR_TRUST_SCOPE`
(`github_adapter.rs:211-213`) and the shipped unit set it to `trusted`
(`debian/velnor.env:42`). `validate_job_trust_policy` then returns `Ok(())` immediately for
a trusted scope *before it looks at the job at all* (`runner.rs:6155-6157`). There is no
`event_name`, `head_repository` or fork check, and no repository allowlist, anywhere in the
tree. A fork pull request therefore shares the `trusted/` namespace with default-branch
builds.

Ten fork-to-trusted write paths were enumerated. Three of them are **code execution on the
next job**, because the directories are mounted read-write onto its `PATH`:
`$CARGO_HOME/bin` (`container.rs:176-180`), `/opt/mise/installs` and
`/opt/velnor/mise-binaries` (`:186-194`).

Separately, Cargo's `registry/{cache,index}` and `git/db` and mise's `cache` are shared
daemon-wide across every repository and owner, read-write (`container.rs:157-171`,
`:195-199`). Rewriting a `.crate` together with its index entry defeats Cargo's checksum
verification, which makes this a cross-repository supply-chain path.

The native `actions/cache` implementation also has no ref scoping at all
(`executor.rs:9296-9322`): GitHub's branch-scoping rule is simply absent.

Missing invariant: *trust class is derived from the job's own event and repository
relationship, is not `Default`-constructible, fails closed to fork-PR, and no cache
namespace crosses a trust boundary read-write.*

### BC-22 — Disk pressure has no terminal state, and GC deletes live state

Below 2 GiB free, the slot loops `sleep(60); continue` forever on both branches
(`runner.rs:2570-2581`): no deadline, no escalation, no terminal state. Queued jobs are only
resolved by a separate `velnor-doctor.timer` unit that does not exist outside Debian
packaging. This is precisely the indefinite "waiting for disk" state the program forbids.

Two garbage collectors delete state that is in use. The leftover reaper determines liveness
from `docker ps` alone (`leftover_disk.rs:103-144`), so a job is invisible to it during
checkout, artifact upload, target publish and deferred BuildKit teardown — it takes no lock
and reads no lease, unlike `cache gc` (`cache.rs:153-169`). Emergency reclaim deletes
unleased stores under live jobs, because leases cover eight scopes
(`runner.rs:5498-5507`) while `mbx`, `sccache` and `artifacts` are `emergency_managed` with
no lease class (`cache.rs:438-446`, `:473-482`).

There is no host disk budget, and Docker is entirely unaccounted: `CacheStore` has no
Docker class (`cache.rs:921-929`) and `system`/`builder` prunes are hard-refused
(`leftover_disk.rs:355-364`), so images consume the headroom the reservation ledger
believes it holds. On the shipped four-slot configuration the artifact store is invisible
to every GC: `artifact_store_dir` (`executor.rs:9287`) omits the `daemon_shared_root` wrap
that `cache_store_dir` applies twenty lines later (`:9313`), so artifacts are written to
`<work>/slot-N/_velnor_artifacts` while GC registers `<work>/_velnor_artifacts` — unbounded
growth. Routine `cache gc` is manual-only with no timer unit, so all 310 GiB of class
budgets are dead letters, and the GitHub Actions cache service is namespaced by the
*per-job* `ACTIONS_RUNTIME_TOKEN` (`gha_cache.rs:222-227`), so it can never produce a
cross-run hit while each tenant still accumulates 10 GiB outside `store_roots`.

Missing invariants: *one catalog is the sole constructor of store paths, so a path built
two ways is unrepresentable; one capacity model derived from `statvfs` rather than summed
constants; one lease-honouring bounded reclaimer that never touches a foreign class; and
disk pressure is a state machine with a deadline and a terminal state, with admission
evaluated before acquisition.*

## 12. Open decisions requiring evidence

### D-1 — One C crypto stack, and which one

The dependency graph builds **two** complete C crypto stacks on every target: vendored
OpenSSL (via `native-tls-vendored`) and `aws-lc-sys`. The second is unavoidable while
`sigstore-sign` is used: `sigstore-crypto 0.11.0` declares `aws-lc-rs` as a plain
non-optional, non-feature-gated dependency and has no `[features]` table at all, and
`sigstore-tsa 0.11.0` hardcodes `rustls-webpki` with the `aws-lc-rs` feature. The
`native-tls`/`rustls` features on those crates forward to `reqwest` only — transport, not
the crypto provider. So no manifest-level selection removes it.

The reverse direction is available: drop vendored OpenSSL and reuse the `aws-lc-rs` already
in the graph, by moving `reqwest` to `rustls` (which pulls `rustls-platform-verifier`, so
system-root behavior is preserved) and `tokio-tungstenite` to `rustls-tls-native-roots`.

It is blocked on a **behavioral** question, not a packaging one:
`crates/velnor-runner/src/protocol.rs:1522` calls `.use_native_tls()`, and the comment at
`:3956` records the reason — GitHub has throttled by TLS fingerprint under concurrent load,
silently dropping step records. Changing the stack changes the fingerprint.

Decision requires measurement: run the concurrent-load step-record scenario against both
TLS backends and compare dropped-record rates. Until that evidence exists, neither stack is
removed. The root fix for the enabling condition — a published crate with an ungated crypto
backend — is an upstream feature request to `prefix-dev/sigstore-rust`, with a
`[patch.crates-io]` fork in the interim.

Nothing above weakens signature verification; all three options verify the same signatures.

## 13. Completed work packages

| ID | Change | Commits |
| --- | --- | --- |
| T-003a | Licence gate: `deny.toml` had no `[licenses]` section, so `cargo deny check licenses` failed with 367 rejections; the `deny` mise task ran only `advisories`, which hid it. Allow-list enumerated from the real graph (MIT, Apache-2.0, BSD-3-Clause, ISC, Unicode-3.0, Zlib — no blanket, no exception needed) and the task now runs every section. Gate now reports `advisories ok, bans ok, licenses ok, sources ok`. | `5644bfd` |
| T-003b | `tokio` `fs` feature declared (four `tokio::fs` uses in `gha_cache.rs` compiled only through transitive feature unification); unused `process` feature removed after confirming zero `tokio::process` uses in the workspace. | `d72dd73` |

### BC-23 — Velnor is an allowlist emulator, not a general Actions runner

`crates/velnor-runner/src/manifest.rs:917-975` allowlists 28 repositories with every ref
pinned to a 40-hex SHA; `action.rs:173-203` reimplements 25 of them as native Rust
adapters; and `runner.rs:9475-9479` hard-`bail!`s any JavaScript action that is not one of
them. The generic JavaScript, Docker and composite execution paths exist but are exercised
by roughly two allowlisted entries, so defects there are currently latent.

This is a legitimate architecture *if it is what Velnor claims*. It is not what the
capability model claims today, which is the actual defect: the surface must state that
third-party JavaScript actions outside the allowlist are unsupported and must fail early
and explicitly, rather than being presented as supported.

Latent defects in those paths, to be fixed on merit rather than blast radius: post steps are
kept as two separately-reversed lists (native, then JavaScript) where upstream maintains a
single LIFO stack (`Runner.Worker/StepsRunner.cs:169-172`), so a mixed job runs them in the
wrong order; `runs.using` node version selection is dead code because
`node_action_image` (`executor.rs:382-394`) always returns the `node:24-bookworm` fallback;
three composite depth limits disagree (upstream 9, admission 10, local 16) and remote
composite recursion in `runner.rs:9443-9560` is unbounded with no cycle detection; composite
step-output references are rewritten with a naive `str::replace` (`action.rs:1682-1691`),
which clobbers prefix ids and literal text; and Docker actions mount `/__a` — every
downloaded action's source — plus `/__w`, `/tmp` and `/__tool`, where upstream mounts six
specific paths.

### BC-24 — Durable sinks bypass the secret masker

`GITHUB_STEP_SUMMARY` content is uploaded without masking
(`crates/velnor-runner/src/script_step.rs:858` → `protocol.rs:4313-4415`), where upstream
scrubs every line first (`Runner.Worker/FileCommandManager.cs:256-267`). A secret echoed
into a step summary is therefore written to a durable blob in cleartext. The redaction logic
is triplicated in the tree, which is the enabling condition: three copies cannot stay in
agreement, and only one of them is on this path.

Related upstream limits are also absent, and they matter because these payloads ride the
durable `CompleteJob` call: no 4096-character annotation truncation, no 10-per-type
annotation cap, no 1 MiB step-summary cap, and `step_number` is always `None`.

Missing invariant: *every sink that leaves the process passes through one masker, and there
is exactly one masker.*

### BC-25 — The wire-name class recurs in the cache and step-result contracts

Three further instances of BC-1 were found. `RunServiceStepResult` is serialised snake_case
inside an otherwise camelCase `CompleteJob` payload (`protocol.rs:3276-3298`). The GitHub
Actions cache v1 response returns `cacheDownloadUrl`/`cacheId` where the client expects
`archiveLocation`/`cacheKey`, and the v2 `GetCacheEntryDownloadURL` response omits
`matchedKey`, so a restore-key hit is reported as non-exact and the entry is re-saved every
run.

Note for future work: the artifact-v4 and cache v1/v2 protocols are **not** defined in
`actions/runner` — they live in `actions/toolkit`, which must be pinned as a second
upstream reference. Several of these contracts therefore had no authoritative oracle in the
repository the project treats as its only source of truth, which is itself part of why the
class persisted.

Independently, every cache v1 lookup misses unconditionally: `query_param`
(`gha_cache.rs:662-669`) uses `find_map` and then filters, so only the first query parameter
can match. `lookup_v1` asks for `version`, receives `""`, and `version` participates in the
entry hash — so BuildKit's `type=gha` cache never restores.

### Worth preserving

Three things this codebase does better than upstream, which the rearchitecture must not
discard: path-traversal guards on action paths (upstream has none); SHA-pinned action refs,
which make the missing download-integrity check moot; and the completion outbox — intent and
payload checksum journaled before send, crash-replayable — where upstream simply retries
five times.

### BC-26 — Four uncoordinated output channels, none authoritative, none redacted

Velnor emits diagnostics through four unrelated mechanisms: 8 tracing spans and roughly 8
tracing events; 74+ free-form `SlotForensics` text lines in `runner.rs`; **189
`eprintln!`/`println!` calls in `crates/velnor-runner/src`**; and a well-designed typed
telemetry contract that declares 23 events of which about 7 are ever emitted. There are zero
`#[instrument]` attributes across 183k lines, and no metrics facility of any kind.

Two of these are P0:

- **Neither of the two highest-volume sinks is redacted.** `telemetry.rs:101-132` and
  `slot_log.rs:74-90` apply no masking, while three redactors exist elsewhere in the tree.
  Same enabling condition as BC-24: multiple redactors, and the ones that matter are not on
  the path.
- **The only durable timing analytics re-parses a text log line** (`runner.rs:11330`) and
  sorts by RFC3339 *string* (`:11410`). A log-format change silently zeroes the SLOs rather
  than failing.

`teardown_ms` is hardcoded to `0` (`runner.rs:6059`) and then compared against a 2 s
`teardown_p95` SLO, so that SLO cannot fire under any circumstances. `queue_ms` is a
cross-clock difference `saturating_add`ed into an internal total with no skew estimate.
Compile telemetry emits `unit_count`, `hits` and `misses` as `0`, while `tools/unit-collector`
computes exactly those values and is wired to nothing.

Structurally: five of the eight spans use `.entered()` across an `.await`, so the busy/idle
timings are wrong for precisely the phases that matter; the 189 print sites bypass every
bound, filter and redactor, with journald as an unbounded disk sink; `SlotForensics` does an
open-write-close per line, synchronously, from async code, and mirrors each line into a
globally-mutexed writer whose rotation runs *inside* the lock and whose poisoned-lock path
silently drops output; and `recent_job_timings` reads 64 MiB per slot into memory to answer a
status query.

Nothing today answers "why didn't my job start?": `status` prints static configuration and
`doctor` requires a PAT and answers a different question. Of the 19 lifecycle stages the
diagnostic timeline requires, 3 are durations, 8 exist only as text, and 8 are unrecorded —
including every checkout sub-phase and all of Docker preparation.

One point is in better shape than expected: the shell-string heuristics
(`executor.rs:88-168`) are telemetry-only today and correctly scoped, and the
correctness-affecting decisions use structured job fields (`declares_sccache` matches an
action reference, not script text). The risk is that they return a bare
`Option<&'static str>` with nothing preventing a future change from routing a decision
through them. One layer out, `crates/velnor-tools/src/audit_ci.rs` makes 120 policy
decisions by grepping shell text — that one is the bug class already realised.

### BC-27 — There is no benchmark of the product

The benchmark is a ~301-line bash script with five embedded Python heredocs that **never
invokes Velnor, Docker, or a job**. Every performance claim currently available in this
project is a claim about `cargo`. It runs n=5, so its reported "p95" is its maximum, and it
records `platform.processor()` — the architecture, not the CPU model — as environment
identity. Fault-injection coverage is 6 HTTP cases in one file, 21 of 27 fault classes are
untested, and there is no soak suite or resource-growth monitor at all.

This is why the program cannot yet believe any performance number, including its own.

Design leverage found while auditing: `CommandRunner` (`executor.rs:199-290`) is the single
choke point for every host process spawn, so one decorator there yields per-job Docker
process counts and per-call latencies with no call-site changes — and is the same seam for
Docker and git fault injection. The Docker subcommand census taken through it
(`exec` 29, `rm` 19, `network` 13, `inspect` 9) independently corroborates the 12+N minimal
and 52-72 representative per-job figures in BC-7. `TelemetryLane { Github, Velnor }` already
encodes the internal-versus-external split the benchmark needs.

### Correction to BC-15 / BC-21 — the trust model is pool-level, and nothing enforces that

Two investigations reached opposite conclusions about trust isolation. Resolved from source
rather than by preferring either report:

- `crates/velnor-runner/src/github_adapter.rs:223-225`
  (`github_trust_scope_allows_host_docker`) accepts only the exact value `trusted`, and it
  gates `mount_docker_socket` (`:63-64`). QEMU, registry login and BuildKit secrets are
  separately gated (`executor.rs:3088-3093`, `:3446-3453`, `:3499-3505`), and stores are
  namespaced by trust class first (`:227-234`). So the isolation *code* is real and, where
  it applies, correct.
- `cargo_target_trust_scope_from` (`github_adapter.rs:215-221`) falls back to
  **`untrusted`**, i.e. the code fails closed — the earlier note that it defaults to
  `trusted` was wrong about this function.
- But `service.rs:194` declares the daemon flag with
  `default_value = "trusted"`, the shipped unit sets `VELNOR_TRUST_SCOPE=trusted`
  (`crates/velnor-runner/debian/velnor.env:42`), and the quick-start documentation instructs
  operators to set exactly that (`content/docs/getting-started.mdx:88`).

The accurate statement is therefore: **Velnor's trust boundary is the daemon/pool, not the
job** — which `velnor.env:40-41` states as the intended design ("one value per daemon/pool
trust boundary"). Nothing derives a trust class from the job's event or head repository, so
a single pool that accepts both fork pull requests and trusted builds has no isolation
between them at all — and that is precisely the configuration the quick start produces.

That reframes the defect rather than removing it. Two things are wrong, and both are real:

1. **Nothing rejects the unsafe configuration.** A pool configured `trusted` will happily
   accept a fork-PR job, and no admission check refuses it. A pool-level trust model is
   defensible, but only if the runner enforces the boundary it claims — a `trusted` pool
   must reject jobs from outside its trust class rather than run them as trusted.
2. **`content/docs/guides/execution.mdx:131-137` claims a per-job "admitted trust class"
   that does not exist.** The documentation describes job-level trust; the implementation is
   pool-level. One of the two must change, and the documentation is the one that is wrong
   about the code.

The fork-to-trusted write paths recorded under BC-21 remain valid **within a single
mixed-use pool**: `$CARGO_HOME/bin` (`container.rs:176-180`) and `/opt/mise/installs` plus
`/opt/velnor/mise-binaries` (`:186-194`) are mounted read-write onto the next job's `PATH`,
and Cargo's `registry/{cache,index}` and `git/db` are shared daemon-wide across every
repository and owner (`container.rs:157-171`). Those are the paths that make a mixed pool
dangerous rather than merely unisolated.

Target: derive trust class from the job, keep it non-`Default` and failing closed, and make
a pool refuse a job whose derived class does not match the class the pool was configured to
serve. Then the pool-level model and the job-level model agree instead of contradicting.

### BC-28 — The job image is 3.0 GB, and BuildKit throws its cache away every job

Measured against the pinned base with a live Docker Engine 29.4.0: base 40.7 MB; the apt
layer installs 290 packages, 363 MB downloaded and **1628 MB unpacked**; the mise layer adds
roughly 350 MB compressed and 1.3 GB unpacked. Total around **3.0 GB unpacked**.

Two P0 items:

- **BuildKit cache is destroyed on every job.** `cleanup_job_buildkit`
  (`executor.rs:3907-3943`) force-removes the buildkitd container *and* its `_state` volume
  on every terminal path (`:3781`, `:3851`, `:3898`), so a workflow's `keep-state: true` or
  `cleanup: false` cannot survive it. Every containerized build in every job starts cold.
  BuildKit is expensive persistent infrastructure being treated as per-job scratch.
- **The emergency BuildKit reclaim is dead code.** `cache.rs:726` inspects the literal
  `"velnor-builder"`, but `job_scoped_buildx_builder_name` (`executor.rs:10786-10790`) always
  appends `-<scope>`, so the inspect never succeeds and the disk-pressure reclaim
  (`cache.rs:678-680`) is a silent no-op. Same class as the artifact-store path defect in
  BC-22: a path or name constructed two different ways in two places.
- **Builder identity is keyed by runner slot, not repository** (`executor.rs:10785-10791`,
  `job_scope_from_temp` `:10822-10832`). Harmless only because nothing persists today. This
  is the enabling condition that must be removed *before* persistent builders are
  introduced, or trusted-class build cache will leak across repositories.

Waste with no consumer: roughly 541 MB of clang/LLVM with nothing using it (no `bindgen` in
`Cargo.lock`, and the mold adapter explicitly avoids clang, `executor.rs:4928-4930`,
asserted at `:26629-26633`); roughly 261 MB of `docker.io` + `containerd` + `runc` inside a
container that uses the *host* Engine through the lease proxy; cosign at 133 MB and hadolint
at 61 MB baked for single-adapter use; a browser and font stack baked into every job; and
sccache baked in while off the default path.

Layer invalidation is inverted: `rust-toolchain.toml` (`:106`) gates the entire ~1.3 GB
toolchain layer, so a Rust patch bump — exactly the P0 upgrade this program is performing —
reinstalls Node, Python, cosign, hadolint, gh, mold and protoc.

Per-job waste on top of that: `seed_mise_store` copies the full ~1.2 GB baked store per
(image × trust × repo × **slot**) (`executor.rs:4109-4148`, `container.rs:291-315`,
`:792-808`); `mise self-update` hits the network per store for a version already baked
identically; two full `find` walks of the multi-gigabyte tool store run **every job**
(`executor.rs:4614`, `:4658`); and the QEMU binfmt privileged container is re-run per job for
host-global state that already persists, with no `/proc/sys/fs/binfmt_misc` check
(`executor.rs:3123-3133`). Orphan BuildKit reclaim runs only at daemon startup or via
`doctor` (`runner.rs:3826-3841`, `:11513-11527`), so a crashed worker leaks a buildkitd
container and a multi-gigabyte volume indefinitely.

### Correction to BC-8 — checkout admission is already fail-closed

The claim that `submodules`, `sparse-checkout`, `filter`, `ssh-key`, `github-server-url` and
`set-safe-directory` are accepted and silently ignored is **wrong**, and the corresponding
implementation task was withdrawn before it produced code.

Verified in source: `crates/velnor-runner/src/manifest.rs:435-448` declares the
`actions/checkout` capability with exactly nine permitted inputs — `repository`, `ref`,
`token`, `persist-credentials` (literal `true`/`false`), `path`, `clean`, `fetch-depth`,
`fetch-tags`, `lfs` — and `validate_inputs` (`manifest.rs:1507+`) raises a
`CapabilityViolation` for any input that matches no rule. Capability validation is strictly
enforced: `args.rs:297-301` errors on any `VELNOR_CAPABILITY_VALIDATION` value other than
`strict`. An independent security audit reached the same conclusion, and additionally
confirmed that `persist-credentials: false` is correctly honoured on both lanes.

What *is* wrong is the inverse: `crates/velnor-tools/src/main.rs:2110-2117` advertises
`submodules` as supported when admission rejects it. The capability surface over-claims.
That is the defect to fix, and it belongs with the capability-surface work rather than with
checkout.

Two process notes worth keeping. First, an audit finding is a hypothesis until the
coordinator confirms it in source; two audits disagreed here and the source settled it.
Second, the earlier claim was reached by reading the checkout implementation and observing
no handling of those inputs — true, but the rejection lives at a different boundary. Reading
one side of a boundary is not evidence about the other.

### Investigation hazard — uncommitted work is visible to investigators

A later investigation reported `crates/velnor-runner/src/expression/` as a complete
3,000-line evaluator with zero call sites, and concluded that three expression
implementations ship simultaneously with the correct one unreachable. That conclusion is
wrong: the directory does not exist at the starting SHA (`git ls-tree 2858e92` returns
nothing for that path) and is untracked in the working tree. It is the in-progress output of
the expression-engine work package, observed mid-flight.

Consequence for this program: investigators share a worktree with implementers, so a
"dead code" or "duplicate implementation" finding must be checked against the starting SHA
before it is believed. Recorded here rather than silently dropped, because the same mistake
is easy to repeat.

### BC-29 — Controls exist at one construction site while other sites bypass them

The dominant structural pattern across the security audit: **20 of 34 findings are a control
that exists and is correct at one call site, while the same operation has other sites that
never route through it.** The remedy is to delete the unsafe overload, not to patch each
site.

Highest-severity instances, all verified at `0bfe740`:

- **Docker argv has no `--` separator and no image-grammar check.** `runs.image:
  docker://--privileged` combined with `runs.args` yields full control of a host
  `docker run` — that is host root (`action.rs:738-742`, `container.rs:644-645`,
  `executor.rs:2665-2677`). Admission only length-checks those fields.
- **Path-based `fs::write`/`read_to_string` into the `1777` `RUNNER_TEMP`.** One step plants
  a symlink at the predictable `/__t/<step_id>.sh`; the next step then gets arbitrary host
  write, read and truncate as the runner user. There is no race to win
  (`script_step.rs:781-786`, `:825-861`).
- **Secrets on argv.** Job and service environment reach `docker run` argv
  (`container.rs:231-236`, `:977-979`), and the entire MicroVM guest exec including the
  resolved `run:` script does the same (`guest_runtime.rs:583-620`). `/proc` makes those
  world-readable to co-tenants. Note the `docker exec` path *was* hardened with a `0600
  --env-file` — this is exactly the one-site-fixed pattern.
- **`::set-env` and `::add-path` are honoured, and parsed from stderr as well as stdout**,
  behind a three-name denylist that omits `LD_PRELOAD`, `BASH_ENV`, `PYTHONPATH`,
  `RUSTC_WRAPPER` and `MISE_*` (`workflow_command.rs:40-45`, `script_step.rs:968-970`).
  Upstream v2.337.0 rejects both commands and never parsed stderr. This is CVE-2020-15228's
  class, re-enabled and widened.
- **Shared Cargo state across trust classes.** `registry/{cache,index}` and `git/db` are
  mounted read-write into every job through `cache_class_path` — the one store-path API
  without trust scoping (`container.rs:1122-1127`, `storage.rs:102-109`). The persistent
  `target/` bucket is keyed on base repository, workflow and job name, and is excluded from
  `git clean` (`checkout.rs:501-540`), so a poisoned build-script binary survives into the
  trusted run.
- **The Docker lease has no route allowlist** — only payload filtering on three create
  routes, and no ownership check, so `/exec` is reachable on any container
  (`docker_lease.rs:1145-1147`); a `Connection: Upgrade` request becomes an unfiltered raw
  tunnel (`:2073-2093`).

The redaction copies number **five**, not three, with three different sentinels (`***`,
`[REDACTED]`, `[redacted]`), and they disagree materially: `records.rs:1773` accepts only
`[REDACTED]`, so a runner-masked string fails the store validator. The copy facing
attacker-controlled output is the weakest of the five.

### BC-30 — Dead architecture is invisible because it is allowed to be

Ten `#![allow(dead_code)]` attributes cover 46,612 lines, 26% of the workspace, and
`cargo check --all-targets` is clean — which is the problem, not the reassurance.

Provably unreachable, with call-site evidence: 16 of 19 `DistributedTaskClient` methods (12
of them public) plus 12 types and `GitHubRunnerProtocol`, which has no implementations —
roughly 950 lines of V1 Azure-DevOps protocol whose entry point `bail!`s with no V1 branch
(`runner.rs:4279-4286`). `node/handoff.rs` and `node/recovery.rs` have zero external
references to any public item, while `controller.rs` reimplements both inline and **more
weakly**: `RecoveryCoordinator` has a retry budget and quarantine state that
`GithubPacing:74-235` lacks, and `AssignmentHandoff` has generation fencing that
`maybe_spawn_job:2248` replaces with bare CLI arguments and no validation. That is the
mechanism behind BC-5's fencing defect: the correct implementation exists and is not used.
`velnor-cas` has zero dependents; `velnor-action-journal` is reachable only through a
one-line glob re-export that nothing consumes. The deletion ledger totals roughly 11,900
provably unreachable lines.

Related structural findings: `velnorctl` links the 117k-line daemon crate and consumes 12 of
its 360 exported items, so 96.7% of that public surface is crate-internal; `DaemonArgs`'
32 fields are declared three times (`args.rs:212`, `service.rs:93`,
`velnorctl/runtime.rs:21`) with hand-written `From` bodies as the only guard against the two
shipped binaries diverging; and `ReadyProof` (`velnor-model/node.rs:390`) is a proof type
whose `try_new` hard-codes all four fields to `true` and exposes them as `pub` +
`Deserialize`, so a valid value carries no information at all. Twenty-eight types have a
validating constructor that a struct literal bypasses; the correct pattern — private fields,
a wire mirror, and `TryFrom` — already exists in this workspace on `JobSummary`, `Slug`,
`Digest` and `TelemetryEnvelope`.

Panic policy is in better shape than expected: 103 production sites, and the class reachable
from external input is **empty** — every untrusted boundary returns a typed error. Two live
defects remain: lock-poison `expect` inside a spawned task (`controller.rs:334`, `:344`,
`:373`) in a workspace that uses `PoisonError::into_inner` everywhere else, and a
`debug_assert` on a Docker chunked-HTTP cursor (`docker_lease.rs:2388`) where a release build
hangs instead of panicking.

Error taxonomy is bimodal: seven crates are anyhow-free and typed, while `velnor-runner`
carries 673 `bail!`, 1,055 `.context` and 181 public anyhow signatures. All nine
string-matched error decisions trace to one line — `executor.rs:4227` formats a Docker exit
code and stderr into an anyhow message with no typed carrier — so a single `DockerCliError`
removes six of the nine. The worst consumer, `docker_start_error_is_transient:4346`, chooses
between a five-attempt backoff and a two-attempt zero-delay budget by matching seven string
literals against the whole error chain. Of 20 retry sites, five are unbounded (worst:
`runner.rs:8504` teardown — fixed 1 s, no growth, no cap, no drain check, 1 Hz forensics
forever on a detached thread), 13 have no wall-clock deadline, and three retry
non-idempotent operations. `executor.rs:8769`'s "55 s deadline" is fake: it is checked only
after a nested three-attempt retry has already completed.

## 14. Evidence provenance rule

A second process failure, caught by an agent's own sanity check, produced a rule that now
binds every agent in this program.

The shared worktree moves and is dirty while agents work in it. An investigation that runs a
build there — `cargo check`, `clippy`, `udeps`, a benchmark — is measuring whatever mixture of
committed and uncommitted work happened to be present at that moment, not the commit it
believes it is auditing. In one instance this produced an apparent broken lint gate
(`error[E0425]: cannot find function docker_cli_timeout`) and a set of dead-code warnings that
were entirely another agent's in-flight refactor. The finding was withheld pending
verification and so never entered the record, which is the only reason a false P0-class claim
did not ship.

Therefore:

1. **Static evidence** — reading source, manifests, lockfiles, and external registries — is
   sound when taken with an explicit working directory at a known SHA, and should be
   byte-verified against that SHA if it was collected earlier.
2. **Build-derived evidence** must be produced against an immutable snapshot:
   `git archive <sha> | tar -x -C <scratch>`, never a live worktree and never a
   `git worktree add` on a shared branch.
3. Every reported measurement must state which of the two classes it belongs to, and against
   which SHA.
4. A claim that something is dead, missing, or duplicated must be checked with
   `git ls-tree <starting-sha>` before it is believed, because untracked work in progress is
   visible to investigators.

Rules 1-3 exist because build output in a shared tree is unattributable. Rule 4 exists
because two separate findings in this program were artifacts of reading another agent's
uncommitted work.

### BC-31 — Automation that matches nothing looks exactly like automation that works

The reason a known-defective `.0` toolchain pin survived is not that nobody configured
Renovate. Renovate *appeared* to cover the toolchain: a custom manager described as "rust
toolchain pin in the job image Dockerfile" matched `\brust@([0-9.]+)` against
`docker/job-ubuntu.Dockerfile`. That string does not occur in that file — its only `@`
occurrences are the pinned `ubuntu:26.04@sha256:…` digest and a `printf`. The manager matched
nothing, produced zero updates, and presented as coverage. `1.97.1`, also a miscompilation
backport, was uncovered for the same reason.

**Nine of the remaining ten custom managers have the identical defect.** They match
`docker/job-ubuntu.Dockerfile` for strings such as `'cargo:cargo-nextest@…'`, `\bjust@`,
`\bprotoc@` and `\bgh@`, none of which exist in that file — those pins now live in
`mise.toml` and `docker/job-mise.toml`. Only the `sccache`, `hadolint`, `cosign` and
`MISE_VERSION` matchers correspond to real strings.

Missing invariant: *a matcher that matches nothing is a failure, not a pass.* A regex manager
with zero matches must be an error in CI, exactly as an empty test suite should be.

A related discovery from the same work: the mise lockfiles are **hard pin sites**, not
derived artifacts. mise resolves Rust from `rust-toolchain.toml` via
`idiomatic_version_file_enable_tools = ["rust"]` and then exports `RUSTUP_TOOLCHAIN` from the
resolved lock entry, so a stale `mise.lock` silently keeps the old compiler after the channel
moves. This was observed live: after the channel was edited, `rustc --version` still reported
1.98.0. Five content sites had to move together — `rust-toolchain.toml`, `mise.lock`,
`docker/build-mise.lock`, `docker/job-mise.lock`, and the prose in
`content/docs/guides/development.mdx`.

### Confirmed: `recursion_depth_exceeding_limit` is real at the starting SHA

Verified against an immutable `git archive` snapshot of `2858e92`, per the evidence rule
above: `cargo +nightly-2026-09-01 check -p velnor-runner --locked --features test-support`
emits exactly one warning — `overflow evaluating the requirement … Send` for the
`tokio::spawn(async move { … })` at `crates/velnor-runner/src/runner.rs:2592`. Proving `Send`
walks the nested async chain (`configure` → `remove_existing_jit_config_for_replace` →
`prewarm_daemon_slot_successor` → `prewarm_successor_after_job` → onward) until the trait
solver overflows.

It does **not** fire on stable 1.98.0 or 1.98.1, so `-D warnings` cannot currently see it:
the gate is green by luck. rust#159228 makes it a hard compiler error regardless of lint
level, so it must be budgeted into any upgrade beyond the 1.98.x series. `#![recursion_limit]`
is de-risking, not the fix; the enabling condition is the depth of that async chain, and
boxing a link in it is the structural remedy.

The "77 warnings" and the dead code in `expression/value.rs` observed alongside it were
entirely another agent's work in progress — at the audited SHA the crate generates exactly
one warning.

## 13. Completed work packages (continued)

| ID | Change | Commits |
| --- | --- | --- |
| T-001 | P0 toolchain: Rust 1.98.0 → 1.98.1 (vtable miscompilation, rust#161441) across all five pin sites, and a Renovate manager that actually matches them, grouped into one PR with no stabilization delay so a correctness backport is not held back. Gates on 1.98.1: fmt pass, workspace build pass, clippy pass on the nine crates untouched by concurrent work. | `e1bc67b` |
| T-007a | Job execution given its own thread, control-plane pool bounded — the prerequisite for removing the fail-closed admission permits. | `239c695` |
| T-006a | Workflow commands: leading whitespace trimmed before matching, closing the `add-mask` bypass that let a whitespace-prefixed mask directive be ignored and the secret printed. | `766b214` |

### Correction to BC-25 — `RunServiceStepResult` snake_case is correct, not a defect

The claim that `RunServiceStepResult` (`protocol.rs:3276-3298`) is wrongly snake_case inside a
camelCase payload is **wrong**. Upstream `src/Sdk/RSWebApi/Contracts/StepResult.cs` at
v2.337.0 declares those names snake_case explicitly — `[DataMember(Name = "external_id")]`,
`"started_at"`, `"completed_at"`, `"completed_log_lines"`, and so on.

The mechanism is worth recording because it is what makes this contract family hard to audit:
upstream serialises through `VssJsonMediaTypeFormatter` with
`VssCamelCasePropertyNamesContractResolver`, and `ToCamelCase` is a no-op on a name that
already starts lowercase. So an explicit `DataMember` name passes through unchanged whatever
its casing, while a bare `[DataMember]` property is camelCased. `CompleteJobRequest`
therefore genuinely carries a camelCase envelope containing a snake_case `stepResults`
element. The asymmetry is real on the wire, and Velnor matches it.

That rule was applied to every serde struct in `protocol.rs`. The full sweep found **exactly
one** wire-name mismatch — the `RunServiceError` envelope already recorded as BC-1 — and it
is fixed. `CompleteJobRequest`, `StepResult`, `Annotation`, `Telemetry`, `AcquireJobRequest`,
`RenewJobRequest`/`Response`, `RunnerJobRequestRef`, `TaskAgentMessage`, `TaskAgentSession`,
`TaskAgentReference`, `TaskAgent`, `JobEvent`, `TaskAgentJobRequest`, `TimelineRecord` and
`TimelineRecordFeedLinesWrapper` all match upstream exactly. The Twirp/results-service structs
and the GitHub REST structs are protobuf-JSON and REST snake_case respectively, so no
`DataMember` oracle applies to them.

The cache contracts in BC-25 remain valid — their oracle is `actions/toolkit`
(`packages/cache`), where v1 `ArtifactCacheEntry` is `{cacheKey, scope, archiveLocation}` and
v2 `GetCacheEntryDownloadURLResponse` carries `matched_key`.

One genuine divergence was found and deferred: upstream classifies a run-service failure purely
on the parsed body's `statusCode` and ignores the HTTP status, whereas Velnor additionally
requires HTTP 404 before consulting the body. The two agree in practice, but the divergence is
real and belongs in the completion-recovery package.

### Coordination defect found in this program's own process — shared staging area

The BC-1 fix was authored correctly but landed inside an unrelated documentation commit
(`5178c06`), with no Conventional Commit message and no signoff of its own.

The cause is mine. In a shared worktree the *index* is shared too, so `git commit -m …` — even
without `-a`, and even when the author only ran `git add` on their own file — commits every
path any agent has staged. My documentation commits swept another agent's staged
`protocol.rs`.

Rule for every agent in this program, added to §1: **commit with an explicit pathspec** —
`git commit -s -o <paths> -m …` — never a bare `git commit`. And never run `git stash` in the
shared worktree: one agent did so to test whether a lint error was pre-existing, which stashed
other agents' uncommitted work; it was restored file by file and verified, but the next
occurrence may not be.

The fix itself is correct and is on the branch; it is not being re-committed, because that
would require rewriting a shared branch's history to gain nothing but a tidier message.

## 5. Target architecture (synthesized)

The full synthesis is in `.rearch/reports/20-synthesis.md`, produced by an independent agent
that read all sixteen investigations, verified the load-bearing claims against source, and
resolved fourteen contradictions between them. A red-team review of it is under way; nothing
below is settled until that returns.

### Root causes

The roughly thirty recorded bug classes collapse to seven causes plus one amplifier:

- **RC-1 — the executing control flow is not the modelled state machine.** An 826-line
  function drives execution while four write-only projections model it. Accounts for BC-2,
  BC-3, BC-5, and the missing stage timeline.
- **RC-2 — cancellation is not an input to execution.** One missing edge:
  `execute_script_job` takes no token. Accounts for BC-6 entirely, plus the missing job wall
  clock and the dead typed cancel API.
- **RC-3 — contracts are transcribed rather than derived, and fixtures are built from the
  transcription.** Wire names taken from C# property names; expressions as string rewriting
  over `Option<String>`. Accounts for BC-1, BC-11, BC-12, BC-25 and the semantic divergences.
- **RC-4 — a control exists at one construction site while the same operation has others.**
  Measured at 20 of 34 security findings. Accounts for BC-7, BC-24, BC-26, BC-29 and the
  artifact-store path defect. The remedy is always to delete the unsafe overload.
- **RC-5 — trust, capacity and resource are ambient process facts rather than derived job
  properties.** BC-15, BC-17, BC-21, BC-22.
- **RC-6 — durable in-flight records have no bounded terminal state.** The completion outbox,
  the disk-pressure park, the teardown loop, five unbounded retries.
- **RC-7 — verification is self-referential.** BC-18 through BC-20 and BC-27. It blocks
  nothing, which is exactly why it goes first and in parallel: until it lands, no other result
  can be believed.
- **Amplifier** — a 117k-line crate with dead-code linting disabled over 40% of itself, an
  `anyhow` interior, and a `pub` surface sized for a consumer that uses 12 of 360 items. Not a
  cause; the reason the seven stayed invisible.

### Shape of the target

Typestate `Slot` and `Job<S>` with the generation held *inside* each state and `fence()`
refusing an occupied slot by signature; cancellation as a required field of `Running`, with
job timeout as just another `CancelReason`; the completion outbox kept — it is better than
upstream — plus a typed error module, a `CompletionUnresolvable` event, a `JobTerminalResult`
event and deletion of the unjournaled branch; a typed synchronous Docker facade owning
`JobResources` and a daemon-generation fact cache; one evaluator over the upstream value union
and one `StepResult` carrying outcome and conclusion as a pair; `TrustClass` derived from the
job's event, failing closed to fork-PR, with `CacheNamespace::path()` as the sole path
constructor; a `StoreCatalog`, one `HostCapacity` from `statvfs`, one lease-honouring
reclaimer, and disk pressure as a total state machine; a `HostBudget` from cgroup quota
propagated *into* the job as `CARGO_BUILD_JOBS`, with per-slot execution threads; a typed
`FailureClass` with `anyhow` banned from signatures; and `JobStage` rows with a
machine-readable `wait_reason`, redacted at the subscriber.

The pieces compose because each subsystem's invariant becomes a *field of a job state* rather
than a convention. 28 invariants are stated with their enforcing mechanism; 11 are
compiler-checked. The load-bearing ones: fencing an occupied slot is a signature error; the
send claim is non-`Clone` and consumed by `send()`; the cancellation token is a required field
of `Running`; `TrustClass` has no `Default`; there is one path constructor and one masker;
evidence whose fields are all literals is rejected as unfalsifiable; and a manifest entry with
no adapter and a JavaScript runtime is a build failure.

### Two findings the synthesis produced on its own

**The triplicated clap argument tree has already diverged, on the field that gates mounting
the host Docker socket.** `service.rs:194` declares the trust scope defaulting to `trusted`
while `velnorctl/runtime.rs:71` declares it `public`. The organization audit had recorded the
three copies as "currently in sync", which the synthesis names as the single most consequential
over-reach in the whole set — an over-reach toward optimism. Two binaries ship with different
defaults for a security-relevant gate.

**`jdx/mr-boxington-action` is admitted with no native adapter**, and the repository's own test
asserts this, so it hard-fails at planning: admitted but unexecutable. That is the same shape
as BC-23's capability-surface over-claim, seen from the manifest side.

### Ordering

P0: the verification oracle (parallel, and first, because it unblocks belief in everything
else) · toolchain (landed) · completion durability · lifecycle typestate · cancellation ·
admission and pool partitioning (landed) · trust derivation · the one-site security controls.
P1: Docker facade · image and BuildKit · Rust acceleration · checkout · semantics · capability
surface. P2: storage, capacity and GC · resource model · observability · benchmark, fault and
soak. P3: deletions, error taxonomy, decomposition, hygiene.

Seven package pairs must be serialized because they touch the same architectural boundary;
five can run fully in parallel.

### Corrections the synthesis made to this plan

- **BC-25 over-recorded.** Two cache findings were entered here as confirmed when the
  underlying report stated they could not be resolved without an `actions/toolkit` oracle.
  They have since been resolved against that oracle and fixed (`1a49c0c`), but the record was
  ahead of its evidence at the time it was written.
- **BC-8** is confirmed withdrawn: only the capability-surface over-claim survives.
- The organization audit's deletion ledger is inflated by the 3,021 lines of the expression
  module, which does not exist at the starting SHA. The real figure is roughly 8,900 lines,
  not 11,900.
- Several report recommendations preceded their own evidence: the Docker client verdict was
  reached before the measurements that report itself said were required; the persistent-target
  layer was called "inert" when `publish_persistent_target` in fact `bail!`s on a symlinked
  target, making it an active failure mode; the BuildKit trust isolation was called "proven"
  on the assumption of a per-job untrusted class that does not exist; and the claim that no
  acceleration opt-out exists is wrong, since `MBX_DISABLE` is workflow-settable.

### Not worth fixing structurally

BC-8 as recorded; the CAS TOCTOU (the crate is being deleted); zip handling in maintainer-only
tooling; the `recursion_limit` warning (raise the limit — the real fix falls out of
decomposition); the benchmark script's defects (it is deleted, not repaired); and
`audit_ci.rs` grepping shell text (CI-only, and fixed as a side effect of the verifier work).

## 13. Completed work packages (continued)

| ID | Change | Commits (fixture branch) |
| --- | --- | --- |
| V-1 | **BC-18/BC-19 resolved: the oracle can now fail.** The `Evidence` type and its binary are deleted, along with `write-result.py` and `compare-results.py`, so a workflow can no longer author an evidence value — the root fix, not a patch. A new dependency-free `crates/verifier` reads provenance from the job environment only and cross-checks the lane against `RUNNER_ENVIRONMENT`; its collectors measure exit statuses, files, environment effects, command files and runner-computed step outcomes, and there is no collector for a literal. Readiness now binds to a real `velnor-runner capabilities export` and **fails without one**, comparing admitted actions by set identity rather than cardinality. Normalization is a closed provenance allowlist whose totality and disjointness are unit-tested; a single-lane compare is an error. Proven by 16 mutation tests — including the exact kache↔mr-boxington swap at constant cardinality that the old cardinality check passed — plus 8 baseline-binding mutations and 3 citation tests. | `78d8413`, `ee4b81f`, `ba075b4`, `e9cc336`, `58b58ba`, `c256f16` |

Two findings from that work enlarge the record: the false-coverage rows were **21**, not the 7
first reported — each citing a workflow that never mentions the action; and the two repositories'
admitted-action sets differ by exactly one substitution in each direction
(`kunobi-ninja/kache-action` only in the fixture, `jdx/mr-boxington-action` only in the runner)
at identical cardinality of 30, which is precisely the drift the old check could not see.

Not yet verified in CI: the workflow conversions pass actionlint and the local gate, but no
live dual-lane run has executed them. The two things most likely to surface there are the
`velnor-runner capabilities export` lookup in `collect-evidence` — if that binary is absent from
`PATH` in a Velnor job, Velnor-lane records will lack their build identity and the comparator
will fail the run, which is correct behavior but is a failure — and any observation that
legitimately differs between lanes now that nothing is silently dropped.

## 8. Discovered bug classes (continued)

### BC-32 — The teardown SLO is compared against a constant zero

`runner.rs:6215` builds every `JobTimingRecord` with `teardown_ms: 0`. On the
path where a `TeardownHandle` exists, the teardown thread overwrites the field
with the real duration before emitting (`runner.rs:8507`, currently
`start_post_completion_teardown`). On the path where it does not, the record is
emitted verbatim at `runner.rs:6226` with the zero still in it, and that record
is what `timing_percentiles` (`runner.rs:11584`) feeds into the
`DEFAULT_SLO_TEARDOWN_MS = 2_000` comparison in `print_doctor_slos`
(`runner.rs:11674`). A zero can never exceed a 2 s budget, so that SLO cannot
fire from those records, and it drags the percentile down for every mixed
sample.

The enabling condition is that the record is constructed complete before one of
its fields is knowable, using a sentinel that is indistinguishable from a real
measurement. The class fix is to make unknown unrepresentable: `teardown_ms:
Option<u64>`, `None` at construction, `Some(duration)` only from the teardown
thread, and a percentile function that excludes `None` the same way it already
excludes the absent `queue_ms`. The SLO then reports "no teardown samples"
rather than a satisfied budget. A same-shape audit is owed for `finalize_ms`
and `container_boot_ms`, which are built at the same site.

Related, and the same class as BC-27: `timing_percentiles` computes a p95 from
any non-empty sample (`runner.rs:11584`), so with one completed job the
reported p95 is that job. `percentile` (`runner.rs:11575`) has no minimum-n
guard. `crates/velnor-bench/src/stats.rs` states the rule the runner needs —
quantile `q` requires `1/(1-q)` samples, so p95 requires 20 — and the runner's
`DEFAULT_SLO_SAMPLE_SIZE = 100` window is large enough to satisfy it once the
guard exists.

### BC-33 — The only durable timing analytics is a text re-parse

`load_recent_job_timings` (`runner.rs:11630` onward) recovers every timing
record by reading `lifecycle.log`, splitting each line on the literal
`"job-timing "` (`parse_job_timing_line`, `runner.rs:11564`) and parsing the
remainder as JSON. The writer is a `format!("job-timing {json}")` at two
unrelated call sites. Nothing binds the writer's format to the reader's
expectation: change the prefix, wrap the line, or add a field before it, and
every line fails to parse, `records` is empty, and `print_doctor_slos` prints
"no completed job-timing records yet". The SLOs do not breach — they disappear,
which reads as health.

The records are then ordered by comparing the timestamp prefix as a string
(`runner.rs:11647`). That is chronologically correct only because
`unix_now_iso8601` emits fixed-width UTC with seven fractional digits, and that
format is maintained for an unrelated reason: GitHub's log UI strips a
timestamp prefix only in the .NET round-trip shape (`runner.rs:9917-9932`). The
sort correctness of the analytics is therefore load-bearing on a rendering
constraint of the GitHub web UI, with no test asserting the coupling.

Missing invariant: *durable analytics must read a durable record, not a
rendering of one.* The class fix is a typed sink — the timing record appended
as JSONL to its own file, written and read through one serialiser — with the
lifecycle log line kept only as human-readable forensics. Until that exists, a
round-trip test that writes through the real emitter and reads through
`load_recent_job_timings` would at least make a format change fail loudly.

Both defects invalidate measurement rather than execution, which is why they
are recorded here by the benchmark work (T-010) rather than patched in place:
`runner.rs` is under concurrent ownership.

## 13. Completed work packages (continued)

| ID | Scope | Outcome |
| --- | --- | --- |
| T-010 | Benchmark system (closes BC-27) | `crates/velnor-bench` replaces `scripts/benchmark/benchmark.sh`, which is deleted. Declarative 33-scenario matrix across lifecycle, Rust, Docker and persistent-host families; NDJSON `velnor.bench.result.v1` output; mandatory environment identity enforced by the schema; `TelemetryLane` reused for the internal/external split; percentiles emitted only where the sample supports them (p50 n>=2, p95 n>=20, p99 n>=100) and single runs refused. Proven end to end on `docker/existing-image` at n=20 against a real Docker daemon. Registered-runner scenarios are declared and reported as unrun on hosts without one; nothing is simulated. |

Two instrumentation hooks remain owed to the harness and are recorded in
`crates/velnor-bench/README.md`: a counting decorator around `CommandRunner`
(`crates/velnor-runner/src/executor.rs`) for the per-job process and Docker
invocation census, and per-phase checkout spans in
`crates/velnor-runner/src/checkout.rs` matching `stage::CheckoutPhase`. Both
live in files under concurrent ownership, so they were reported rather than
applied. Git byte and ref counters need no runner change for harness-driven
scenarios: `GIT_TRACE2_EVENT` is set and its documented event JSON parsed. For
runner-driven jobs the runner must set the same variable on its git children.

### Correction to BC-16 — the admission gate was self-poisoning, not merely contended

The recorded mechanism ("another slot is busy") is wrong in its literal form and understates
the defect.

Each slot is its own OS process (`runner.rs:2223`, `:2543`; `node/job.rs:171` launches one
`run_daemon_slot` per worker process), so a *process-global* semaphore cannot be contended
across slots at all.

The real mechanism: a `spawn_blocking` task outlives the `tokio::time::timeout` that gave up
on it, and the permit is released only when the closure finally ends. So the first
Contents-API read that overran the 65 s admission budget kept the single permit **forever**,
and every subsequent job in that slot process failed admission instantly and permanently. The
same held for a stalled SQLite worker. One slow dependency therefore became an unbounded run
of failed jobs on that slot — a red ❌ on every pull request routed to it until the process was
restarted.

That is a better example of the program's thesis than the original description: the gate was
not a bottleneck, it was a latch, and nothing in the design gave it a path back to the
unlatched state. It is RC-6 (durable in-flight records with no bounded terminal state) wearing
a different costume.

Resolution, in `239c695`, `91dff09`, `b08d592`, `179df7a`, `0ae9ba1`:

- The Tokio blocking pool is now explicitly sized at 16 and belongs to the control plane
  alone; the job body runs on a dedicated named OS thread per slot, with the result returned
  over a `oneshot` so a panicked or killed thread arrives as a dropped sender rather than a
  hang. The previous 512 was the absence of a budget rather than a budget.
- Both one-permit semaphores and both `try_acquire` gates are deleted outright. The
  operational store now has **no limiter** — `Store` already serialises writers and the pool
  is bounded — and is instead bounded by a 30 s deadline surfaced as a distinct
  `AdmissionPersistenceOutcome::DeadlineExceeded`. Previously there was no timeout there at
  all, so a hung writer hung the job indefinitely.
- Closure admission uses a waiting, concurrency-bounded limiter (4). Callers wait; they never
  fail. Converting `admission.rs` to async was explicitly *not* done and the reason is sound:
  `admit_job` is a synchronous recursive walk whose children are discovered from each fetched
  `action.yml`, so there is no root-level fan-out to parallelise without restructuring the
  security-critical closure engine and its ~40 tests. That is its own package.
- `JobStage` and `WaitReason` are emitted on all three non-executing stages as `&'static str`
  fields, with no hot-path formatting.

Deferred and named: the three new telemetry fields are not yet declared in the `passive_wait`
contract in `crates/velnor-model/src/telemetry.rs` and `schemas/velnor.telemetry.v1.json`.
Both permit them today (`additionalProperties: true`), so nothing is broken, but the contract
should name them.

### Coordination incidents, resolved

An agent ran `git stash` in the shared worktree to test whether a lint error was pre-existing,
which stashed every other agent's dirty files. Two agents recovered their own work by
extracting individual diffs. I have since verified the leftover `stash@{0}` against `HEAD`
file by file: the Renovate rust manager, `mise.lock` at 1.98.1, the bounded blocking pool and
the `statusCode` fix are all present on the branch. **Nothing was lost**; the stash is fully
redundant and is retained only as a safety copy.

Separately, a deletion of `scripts/benchmark/benchmark.sh` staged by the benchmark package
landed inside another agent's commit for the same shared-index reason. The deletion is
intended, so it stands where it is rather than being rewritten out of a shared branch's
history.

Both incidents are consequences of many agents sharing one worktree, and both are now covered
by the §1 rules: explicit pathspec on every commit, and never `git stash` in a shared tree.

### BC-31 resolved, and its root cause was one naming rule

Empirical match counts against the files each manager targeted: **eight of the ten custom
managers matched nothing** — `cargo-nextest`, `rust-script`, `casey/just`,
`protocolbuffers/protobuf`, `cli/cli`, `mozilla/sccache`, `hadolint/hadolint` and
`sigstore/cosign`. Only `jdx/mise` (1 match) and the `rust-lang/rust` manager added in
`e1bc67b` (7 matches) were live. My earlier note that the sccache, hadolint and cosign
matchers corresponded to real strings was wrong: those shell assignments no longer exist in
`docker/job-ubuntu.Dockerfile`, having moved to `docker/job-mise.toml`.

The root cause is a single naming rule. Renovate's **native** mise manager defaults to
`**/{,.}mise{,.*}.toml`, which requires the basename to *start* with `mise` — so
`docker/build-mise.toml` and `docker/job-mise.toml` matched nothing, the entire job-image
toolchain was unmanaged, and the eight regex managers were imitating the coverage that
absence created. The fix is therefore not eight replacement regexes but an override of
`mise.managerFilePatterns` to include both files (repeating the six defaults, since an
override replaces rather than extends), verified against the manager source in
`renovate@44.61.6` — its `backendDatasources` covers the `aqua:`, `cargo:`, `github:` and
`core:` backends those files actually use.

One hazard was established from Renovate's source rather than assumed:
`mise/lockfile.js::getLockFileName` derives the lock name from the *directory*, not the
basename, so Renovate computes `docker/mise.lock`, finds no such file, and returns `null` —
it will not refresh `docker/*-mise.lock`, and it does not create a spurious file either.
Rather than regex-bumping checksummed lock entries, the gate below fails the build when a
declaration and its lock disagree, which turns the silent stale-lock hazard loud. That matters
because a stale mise lock silently pins an old toolchain regardless of the declared version.

**The class fix** is `scripts/pin_integrity.mjs`, wired into the `ci` and `check` mise tasks,
built in the style of the `actions/runner` pin — an assertion, not a note. It asserts three
properties in one gate: every custom manager, every individual `matchString` and every
selected file must produce at least one match and capture `currentValue` (and a
`managerCoverage` rule requires every file matching `(^|/)[^/]*mise\.toml$` to be reachable,
so a future `docker/foo-mise.toml` fails the build instead of going unmanaged); every
`[tools]` entry must equal its adjacent lockfile entry and agree across configs,
backend-insensitively; and `config/version-pins.json` names the one authoritative site for
each literal repeated outside mise, failing also when a mirror's pattern stops matching, so a
moved pin cannot quietly drop out of the comparison.

It was proven to fail rather than merely to pass: reintroducing the original `\bjust@` manager
verbatim, reverting the nextest pin, and reverting `MISE_VERSION` each produced a specific
error and exit 1, and the clean tree exits 0. The Renovate config validator was likewise
proven non-vacuous by injecting a bogus key.

| ID | Change | Commits |
| --- | --- | --- |
| T-014 | Eight dead custom managers deleted; the native mise manager pointed at the image configs that were never covered; `MISE_VERSION` coverage widened to the root `Dockerfile`, which was unmanaged entirely; pin-integrity gate added and proven to fail. | `6bb40d0`, `8c84237` |
| T-012a | The two environment divergences reconciled: `cargo-nextest` 0.9.140 → 0.9.143 in the root `mise.toml`, `MISE_VERSION` v2026.8.14 → v2026.9.1 in `Dockerfile`. | `f1e272f` |

### Correction — the Docker instrumentation the benchmark needs already exists

The benchmark package reported that no `CommandRunner` counting decorator exists and
specified one for a future package. That was true when it looked and is stale now: the Docker
deadline work landed the instrumentation at exactly that seam.

At `origin/perf/docker-rust-mbx`: `crates/velnor-runner/src/docker/metrics.rs` exists, and
`executor.rs` calls `docker_deadline` and `record_docker_result` from every `CommandRunner`
method (`:593`/`:645`, `:676`/`:737`, `:798`/`:838`, and the stdin variant). Per-call fields
are `docker_op`, `docker_latency_ms`, `docker_exit_code`, `docker_timed_out`,
`docker_invocation`; per-job totals are emitted from a drop guard so they report even on an
early return. Field names are a closed vocabulary — an unclassified subcommand logs
`"unclassified"` rather than the raw token — so no argv content reaches the sink.

Consequence: `crates/velnor-bench/README.md` still asks for a decorator that now exists, and
`sys::Runner` still counts only the harness's own processes. Wiring the benchmark to consume
the runner's per-job Docker counters is a small follow-up, and it is what makes the 12+N and
52-72 per-job process figures in BC-7 measurable rather than estimated.

Still genuinely missing, as that package reported: the per-phase checkout spans (mirror lock
wait, mirror fetch, workspace fetch, workspace checkout, mtime normalization). Those belong
with the checkout package.

### BC-32 and BC-33 — the runner's own timing analytics cannot report a breach

Recorded by the benchmark package while building the harness, and they matter because they
are the numbers the runner reports about itself.

**BC-32.** Every `JobTimingRecord` is constructed with `teardown_ms: 0` (`runner.rs:6215`).
The teardown thread overwrites it at `:8507` only on the path that has a `TeardownHandle`; the
path without one emits the zero at `:6226`, and that feeds `timing_percentiles` (`:11584`)
into the `DEFAULT_SLO_TEARDOWN_MS = 2_000` check at `:11674`. A zero can never breach a two
second budget, so the teardown SLO cannot fire. The fix is the class fix rather than the
assignment: make it `Option<u64>`, `None` at construction, filtered out of the percentile
exactly as `queue_ms` already is, so the SLO reports "no samples" instead of "satisfied".
`finalize_ms` and `container_boot_ms` are built at the same site and owe the same audit.

In the same function, `percentile` (`:11575`) has **no minimum-n guard**, so a single
completed job reports itself as the p95 — the identical defect to the benchmark script that
was just deleted for printing a p95 at n=5. The runner's 100-record window is large enough to
satisfy a p95 rule once a guard exists.

**BC-33.** `load_recent_job_timings` (`:11630`) recovers records by splitting `lifecycle.log`
lines on the literal `"job-timing "` (`:11564`). Change that prefix and the SLOs do not
breach — they *disappear*, reporting "no completed job-timing records yet", which reads as
health. The sort at `:11647` compares the timestamp prefix as a string, which is
chronologically correct only because `unix_now_iso8601` is pinned to fixed-width UTC with
seven fractional digits — and that format is maintained for an unrelated reason, GitHub's log
UI strip rule (`:9917-9932`). Analytics sort correctness is load-bearing on a web-UI
rendering constraint, and nothing tests the coupling.

Both are RC-3 in miniature: a contract recovered by re-parsing a rendering rather than read
through the serialiser that wrote it.

## 15. Red-team review of the target architecture

Full review in `.rearch/reports/21-redteam.md`. It changes the plan materially, and the
central finding invalidates the shape of the proposal rather than a detail of it.

### The typestate cannot enforce a cross-process invariant

Velnor is a three-tier **process** fleet: a controller spawns one slot process per slot
(`node/controller.rs:2208` spawns `current_exe() slot --slot-index N --generation G`), and each
slot process admits one job at a time (`main.rs:15`). The state the synthesis wants to hold in
Rust types does not live in one address space — it lives on disk as `Clone + Serialize +
Deserialize` records with **all fields public** (`journal.rs:300-362`), rehydrated through
`materialized_state()` (`journal.rs:1045`).

**Seven of the eleven compiler-checked invariants therefore do not hold**, and the failures are
not incidental:

- **I-1 (fencing an occupied slot is a signature error)** — fencing is a controller decision
  taken over a *deserialised* `SlotRecord`; `Occupied` exists in a different process. BC-5 is
  narrowed to a staleness window, not eliminated.
- **I-3 (`SendClaim` non-`Clone`, consumed by `send()`)** — there are two send paths in two
  process roles (`runner.rs:10564` versus `:827`/`:10677`); the real uniqueness token is the
  `OutboxRecord`, which is `Clone + Deserialize` with public fields.
- **I-5 (`From<Job<Acked>> for JobState`)** — a trait implementation cannot make a disk write
  happen.
- **I-11 (no fail-open path)** — false at head; see below.
- **I-13 (`TrustClass` has no `Default`)** — the derivation it depends on is not implementable
  as specified; see below.
- **I-20 (`Redacted<String>`)** — a newtype cannot redact a `String` nested inside a
  `Serialize` payload such as `trace.jsonl` or a job dump. This needs a serializer, not a type.
- **I-23 (validator-only construction)** — `ReadyProof::try_new` (`journal.rs:335`) validates
  four caller-supplied booleans taken off a public-field `Deserialize` record. It is a claim,
  not evidence.

The correction is not to abandon the typestate but to stop expecting the compiler to carry an
invariant across a process boundary. Anything crossing that boundary needs a durable mechanism —
schema-versioned events, fencing tokens checked on read, and validators that derive rather than
accept. Where the type system genuinely does apply, it still should.

### Live defects the red team found

**A trust split-brain, reachable through the documented hardening step.**
`github_adapter.rs:223` reads the trust scope from the CLI argument, while
`cargo_target_trust_scope()` (`:211`) reads `std::env::var` directly and feeds six store paths.
The shipped unit sets `VELNOR_TRUST_SCOPE=trusted` (`debian/velnor.env:42`). So
`velnor-runner service --trust-scope public` — the documented way to harden a pool — yields the
Docker socket correctly withheld and the compiler stores still **trusted**, which is precisely
the fork-to-trusted poisoning the flag exists to prevent. There are three defaults, not two:
`service.rs:193-195` declares `trusted` with an `env =` binding, `velnorctl/runtime.rs:71-72`
declares `public` with none, so `velnorctl` cannot observe the variable at all. The gate also
governs container options (`:504`), **privileged service containers** (`:514`) and host port
publishing (`:536`, `:568`) — wider than the socket alone. Dispatched as T-015.

**Expression evaluation is still fail-open at head.** `executor.rs:11577 resolve_expressions`
does `Err(_) => rendered.push_str(&rest[start..…])`, emitting the raw `${{ … }}` span into the
shell. Its own comment assigns the fix to "the lifecycle work package" while the synthesis
assigns it to the semantics package — it is in neither. A **third** expression implementation
also survives: `action.rs:1513 render_composite_expression` and `:1538` are textual and never
call the new evaluator.

**`TrustClass` cannot be derived as specified.** `head_repository_id` does not exist in the job
message. Failing closed on its absence would make every `push` job a fork PR, every executable
store read-only, and would destroy the Rust cache hot path — the opposite of this program's
goal. The proposed enum is also non-disjoint (a release, on a tag, on the default branch matches
four variants, and the namespace key would depend on match order), and `pull_request_target`
and `workflow_run` have no representation at all. This is now an open design question, not a
task.

**The migration itself can lose a completion.** `journal.rs:1236` silently `continue`s on an
undecodable event. An older controller replaying a new `CompletionUnresolvable` event skips it,
materialises the outbox row as still pending, and re-drives the completion — a duplicate
terminal send on a fenced generation, defeating the very invariant the change was meant to add.
Terminal-affecting events must ride a `JOURNAL_SCHEMA_VERSION` bump (`journal.rs:28`, `:997`
already hard-fails), not be added silently.

**The single Docker I/O thread deadlocks cancellation.** A forty-minute `docker exec` occupies
the facade's one I/O thread, so the cancellation path's `kill` cannot be driven until the exec
it targets returns. Not a global bottleneck — the process model prevents that — but an
intra-job one, landing on the hot path and the cancellation path simultaneously.

**Deleting "one of the two `actions/cache` implementations" would delete a wire protocol.** They
are not substitutes: `action.rs:176` and `executor.rs:5095` handle the *step*, while
`gha_cache.rs` serves the v1 Twirp and v2 Results protocols that BuildKit's `type=gha` requires.

### Corrections to the deletion list

**Must not be deleted as written:** the second `actions/cache` implementation, above; the
`velnorctl → velnor-runner` edge, because velnorctl runs *every* command through runner handlers
(`runtime.rs:14`, `:16`, `:647`; `lib.rs:294-1309`) — deleting it deletes the CLI, and the
motivation behind that proposal is fixed by unifying the clap trees instead;
`GuestPlan::cancel_requested`, since `guest_plan.rs:140-165` is `deny_unknown_fields` and
removing the field is a vsock wire break — delete it *after* the cancellation package, not
before; `RunServiceJobJournalState`, a real fork where `runner.rs:11082` and `:11090` send
different messages; and `normalize_checkout_mtimes`, which has a live caller at
`checkout.rs:196`.

**Incomplete as written:** the `velnor-cas` / `action-journal` / `action-model` / `supersession`
/ `diagnostics` set is one connected component and must go together, along with the dead
`velnor-runner/Cargo.toml:112` edge and the `dependency_boundaries.rs` entries; deleting
`diagnostics.rs` leaves `velnorctl diagnostics` permanently a stub; and `DistributedTaskClient`
is 15 methods with 3 live, not 16 of 19.

**Missing from the list:** `ResolvedAction::javascript_invocation` (`action.rs:648`) and all
`ExecutableStep::JavaScript` machinery, which has zero production construction sites;
`NativeActionAdapter::ApprovedComposite` (`action.rs:115`), never matched outside the manifest
and the enabling condition for the mr-boxington defect; `handoff.rs`'s
`write_completion`/`read_completion`, a completion protocol that never had a caller; and the
`std::env::var` at `github_adapter.rs:211`.

### Capability-surface defects, confirmed and enlarged

`jdx/mr-boxington-action` is admitted (`manifest.rs:424`) with no adapter (`action.rs:173-202`
lists 26; the repository's own test asserts the absence at `manifest.rs:2466`), declares node24,
and therefore reaches `runner.rs:9712 bail!` — admitted but unexecutable. The same shape applies
to the four `ApprovedComposite` entries as a class, and `fsfe/reuse-action` (`:413`) is already
mislabelled — it is a Docker action — while `manifest.rs:2604-2609` asserts only the reverse
direction.

The proposed invariant "a manifest entry with no adapter and a JavaScript runtime is a build
failure" is **not implementable as stated**: the runtime is discovered at fetch time from
`action.yml` (`action.rs:229-253`), so no build-time check can know it.

Two further surface defects: `clean` and `fetch-tags` are admitted and honoured but absent from
the tools list, so `target_audit` `bail!`s (`velnor-tools/src/main.rs:2279`); and
`actions/setup-python` is advertised (`:2419`) with no manifest backing at all.

### Ordering corrections

Four packages were re-planned that have already landed: admission and pool partitioning in full,
the expression evaluator, the Docker deadlines and fact cache, and the benchmark harness.

Newly identified conflicts, none of which the synthesis listed:

- **The verifier package versus Rust acceleration.** The fixture's own `.github/AGENTS.md`
  *mandates* sccache in every Rust compile job, while the acceleration package deletes sccache.
  The fixture policy has to change first or every fixture Rust job fails.
- **Verifier, capability surface and acceleration** all own `velnor-tools/src/main.rs` and
  `manifest.rs`.
- **Trust derivation versus the one-site security controls** both gate the Docker socket, and
  the argv-hardening work in flight will freeze on `trust_scope: &str` unless the trust type
  lands first as a leaf.
- **`JobStage` unification must precede the lifecycle package**, not follow it: a private
  three-variant `JobStage` already exists in `runner.rs` from the landed admission work, so the
  lifecycle package introducing its own would create a *fifth* stage vocabulary — reproducing
  the very root cause it exists to remove.
- The Docker package should split into a facade half that can start now and an Engine-abort half
  that must follow cancellation; as listed it puts priority-2 work behind priority-4.

### Confirmed sound — not to be second-guessed

The seven-root-cause collapse and RC-1's formulation; RC-4's "delete the unsafe overload"
remedy; the synchronous-facade resolution, which the process model makes more right than the
synthesis knew; keeping the completion outbox, with its checksum-verified replay and three-point
fence; `CompletionUnresolvable` and the disk-pressure total state machine; the verifier package
going first and in parallel; and — named as the single highest-value item for the program's
stated goal, and untouched at head — deriving a `HostBudget` from the cgroup quota and
propagating it into the job as `CARGO_BUILD_JOBS` and the mbx scheduler's share. That is not
P2 work. Dispatched as T-016.

The landed admission fix is named as the model for how the remaining packages should land.

### BC-28 partially resolved — the job image, measured rather than estimated

Both images were actually built (the `mise_github_token` secret was obtainable via
`gh auth token`), so these are measured unpacked sizes taken inside the running container, not
computed figures. Baseline built from a `git archive` snapshot per the evidence rule.

| Area | Before | After | Δ |
| --- | --- | --- | --- |
| clang/LLVM | 448 MiB | 128 MiB | −320 |
| Docker Engine (dockerd, containerd, shim, runc) | 222 | 0 | −222 |
| Docker client + Buildx | 111 (apt) | 98 (`docker:29-cli`) | −13 |
| pyenv `-dev` deps, `libsqlite3-dev`, misc | — | — | −~128 |
| **Whole image** | **4503 MiB** | **3820 MiB** | **−683 MiB (−15.2%)** |
| installed packages | 377 | 313 | −64 |

Each removal was confirmed rather than assumed: no `bindgen` in `Cargo.lock` and the mold
adapter asserts no clang linker; jobs use the host Engine through the lease proxy and nothing
starts a daemon; `MISE_PYTHON_COMPILE=0` plus an `install_only_stripped` CPython URL means
Python is never compiled; `rusqlite` is `bundled`. Two facts only a real build could establish:
Ubuntu 26.04 has **no `docker-cli` package**, and `docker-buildx` depends on `docker.io`, so
removing only `docker.io` would have saved literally zero bytes — the client and Buildx now come
from a digest-pinned `docker:29-cli`.

**The layer inversion had a deeper cause than layer order.** `docker/job-mise.lock` *mirrors*
the Rust channel, so any layer copying the lock is invalidated by a Rust bump regardless of
where the `COPY` sits. A stage now strips that mirror — it is a bare version echo with no URL
and no checksum, unlike every other entry — and the non-Rust toolchain layer is keyed on the
stripped copy. Proven by simulating a 1.98.1 → 1.98.2 bump and rebuilding: the toolchain layer
reported `CACHED` where previously that edit rebuilt everything.

**Laziness was tried and the evidence rejected it.** Building the image without cosign showed it
reinstalled anyway: mise's `exec_auto_install` makes *any* `mise exec` materialise the entire
configured toolset, so a tool still listed in `job-mise.toml` is not lazy — it is deferred to
the worst possible moment, the first `mise exec` of every job. Unblocking it requires either
`exec_auto_install=false` fleet-wide, which changes tool resolution for every user workflow, or
removing the tool from the mise config and giving its adapter a pinned source. hadolint is
blocked differently: its adapter runs the binary directly instead of calling
`locked_mise_install` as the cosign, mold and just adapters do — and that inconsistency is the
real bug class. The browser and font stack stays because `playwright install-deps` at job time
would need root apt with network, which the one-toolchain contract forbids.

Version bumps taken and verified in the built image: gh 2.100.0, protoc 36.1, **mbx 1.7.0**,
cargo-nextest 0.9.143, mise v2026.9.1. protoc 36's breaking changes are gated on edition 2026 or
concern other languages, and this repository has no `.proto` files.

**mbx 1.7.0 changes an assumption Velnor should know about.** 1.6.0 could let Cargo compile a
dependent against metadata produced before a mid-compile source edit; 1.7.0 deletes the modeled
outputs and **fails the build** instead. So any job that writes into the workspace while a cargo
build runs — parallel steps, or a cache restore landing on sources — now gets a hard failure
rather than a silent stale artifact. That is the correct behavior and it is also the same defect
class as BC-14, fixed upstream. Its cache-key changes additionally mean the host-persistent
`/var/cache/mbx` stores take a one-time warm-cache miss on the first job after this ships.

| ID | Change | Commits |
| --- | --- | --- |
| T-012 | Job image reduced 4503 → 3820 MiB; layer invalidation inverted back; five tool versions bumped and reconciled across environments. | `f1e272f`, `709583a` |

**Blocked, and the block is itself a finding.** sccache 0.16.0 → 0.17.0 and mold 2.41.0 →
2.42.0 could not be taken, because both versions are also hardcoded in `executor.rs`
(`:4866-4867` asserts `sccache 0.16.0` "must be preinstalled"; `MOLD_LOCKED_VERSION` at `:4972`,
asserted at `:26795`) and in four workflow files. Bumping only `docker/` would break those
adapters at job time.

This is exactly the coupling the new pin-integrity gate exists to catch, and it does not yet
cover it: the gate validates mise configs, lockfiles and the declared mirrors in
`config/version-pins.json`, but not version constants embedded in Rust source. Extending it
there is the follow-up that unblocks both bumps.

Also stale and needing a follow-up: `README.md:8` and
`content/docs/guides/execution.mdx:123` still name mbx 1.6.0.

### Correction to BC-24 / BC-29 — the secret masker is not duplicated

The security audit reported five redaction copies with three sentinels that "disagree
materially". That is wrong, and I verified it in source rather than taking either side.

There is exactly **one** secret masker: `Masker` (`runner.rs:7203`, `impl` at `:7208`) using
aho-corasick with `MatchKind::LeftmostLongest` and the single sentinel `***`. `mask_all`,
`mask_single_value`, `mask_value`, `mask_log_lines` and `mask_log_lines_with` are thin wrappers
over it, three of them `#[cfg(test)]`. They agree, and there is nothing to collapse.

The other `redact_*` functions are different jobs, not copies of it: `protocol.rs:297` strips
credentials from a URL, `executor.rs:12258` redacts known-secret action *inputs* by name, and
`manifest.rs:106`'s `[redacted]` is an error message about an unsupported capability input. A
sentinel count across unrelated functions is not evidence of a duplicated masker.

The real defect is narrower and therefore more actionable: **two durable sinks do not call the
one masker.** The step summary is uploaded unmasked (`runner.rs:6880`, with `job_masks` already
in scope at the call site), and annotations are published unmasked into both `CompleteJob`
(`:10466`) and timeline records (`:10460`) through `run_service_annotation` (`:11395`). An audit
of every other durable sink — step-log blob, combined job log, timeline feed, telemetry, job
outputs — found all of them masking correctly, and upstream does not mask artifact bytes either,
so that is not a divergence.

This still belongs to RC-4 (a control correct at one construction site while another path
bypasses it), but the remedy is to route two call sites through the existing masker, not to
consolidate implementations that were never duplicated.

### BC-25 enlarged — the cache client could never have worked

Fixing the v1 query parsing uncovered two further defects in the same function, both of the same
class, all now fixed (`1a49c0c`, `674ef1f`):

- `keys_from_query` split the key list on a literal comma *before* decoding, but toolkit sends
  `cache?keys=${encodeURIComponent(keys.join(','))}` and `encodeURIComponent` escapes a comma as
  `%2C`. So the entire restore-key list arrived as one bogus key. Even with the `version`
  parameter fixed, restore keys would never have matched.
- A v1 miss returned HTTP 200 with `{"__typename":"NotFoundError"}`. The toolkit client
  (`getCacheEntry`) treats **only 204** as a miss and throws `Cache not found.` on a 200 lacking
  `archiveLocation`. Now 204 with an empty body.

The wire contracts were fixed against `actions/toolkit` pinned at
`193fa46c20fde8b0ed54194bc08b841c78c0776d`: v1 `ArtifactCacheEntry` is
`{cacheKey, scope, cacheVersion, creationTime, archiveLocation}` with no `cacheId` or
`cacheDownloadUrl` on it at all, and v2's `matched_key` is field 3 of
`GetCacheEntryDownloadURLResponse`, which the client uses to compute
`isRestoreKeyMatch = request.key !== response.matchedKey` — omitting it made every hit look like
a restore-key hit, so the entry was re-saved every run.

One contract was left alone on principle rather than guessed: the reserve response's `cacheId`
is typed `number` in toolkit while Velnor returns a hex SHA-256 string. The JavaScript client
only truthy-checks it, so it works there, but BuildKit's Go client would deserialize into an
integer field. Establishing that would require pinning a third upstream repository, and changing
it means reworking the content-addressed id scheme — reported, not guessed.

The unread `CacheHit.size` field that was blocking the `-D warnings` gate turned out to be
vestigial from a v1 response field the client never had. Deleted rather than allowed.

### The `RUNNER_TEMP` symlink defect is fixed at the syscall

`RUNNER_TEMP` is mode 1777 with predictable step-file names, and every step file was written and
read by path. Confirmed exploitable with no race. Fixed by binding `RUNNER_TEMP` once to a
directory descriptor and creating and reading every step file relative to it: writes stage into
an `O_CREAT|O_EXCL|O_NOFOLLOW` temporary and rename into place, refusing a non-regular
destination; reads use `statat(SYMLINK_NOFOLLOW)` plus `O_NOFOLLOW` and treat a non-regular file
as an error rather than following it; and the summary size is now taken from the open descriptor,
so the file measured is the file read. Existing `fs_copy.rs` primitives were reused rather than
new ones written.

A detail worth keeping: `NoFollowDir::open_absolute` refuses a symlink anywhere in the path,
which fails on macOS because `/var` is itself a symlink. `RUNNER_TEMP` is a runner-configured
root, so it is canonicalised once through `open_trusted_rooted_destination` while everything
below it stays hostile — the same treatment the rest of the tree gives trusted roots.

Deferred root cause, named: step-file names remain predictable, so a step can still *forge*
another step's outputs by writing a regular file at the expected path. Unpredictable per-step
directories would close that, and it needs a change in `executor.rs`, which asserts
`GITHUB_OUTPUT=/__t/step1_output`.

### Coordination defect — I double-assigned a file

`script_step.rs` was assigned to two packages at once, and both edited it. Nothing was lost —
the second agent merged onto the first's commit rather than clobbering it — but that was luck
plus care, not the process working. Ownership claims in §7 must be checked before a package is
dispatched, and a file already claimed must be either declined or explicitly handed over.

The shared index remains the sharpest hazard: `git add <path> && git commit` sweeps whatever
anyone else has staged. The rule is now `git commit -s -- <paths>`, and `git pull --rebase
--autostash` is also discouraged, having flattened another agent's staged files to unstaged
(content survived; only the staged distinction was lost).

### Correction — upstream *does* parse stderr for workflow commands

I told an agent that upstream never parsed stderr for workflow commands. That is wrong.
`Runner.Worker/Handlers/ScriptHandler.cs:334-336` wires both streams into the output manager:
`StepHost.OutputDataReceived += stdoutManager.OnDataReceived` and
`StepHost.ErrorDataReceived += stderrManager.OnDataReceived`. Velnor parsing stderr matches
upstream, and nothing was changed there. The agent verified it against the pinned source rather
than accepting my instruction, which is the behavior this program needs.

### BC-29 instance closed — workflow commands were a permissive re-implementation

All five workflow-command findings were confirmed against upstream at `397b032` and fixed
(`766b214`, `a3fcd59`, `6c7092f`, `8dc57ff`):

- Leading whitespace is now trimmed before matching, as `ActionCommand.TryParseV2` does. Because
  Velnor masks log lines from the registered mask set, the missing registration was exactly what
  leaked the secret — `"  ::add-mask::$SECRET"` registered nothing and the value reached the log.
- `add-mask` now masks each line of a multi-line value as well as the whole, matching
  `AddMaskCommandExtension.ProcessCommand`, and warns on an all-whitespace value instead of
  dropping it silently. A PEM key previously leaked line by line.
- `stop-commands` tokens are validated the way `ActionCommandManager.ValidateStopToken` does —
  rejecting an empty token, `pause-logging`, or any registered command name, and continuing to
  process rather than stopping — with tokens longer than six characters registered as masks, the
  `ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS` opt-in honoured, and resume matched by command
  name rather than by an exact `::token::` string compare that indented or parameterised resume
  lines defeated.
- `::set-env::` and `::add-path::` are refused with upstream's exact error strings unless
  `ACTIONS_ALLOW_UNSECURE_COMMANDS` is set. Because the refusal lives in the parser it covers
  stdout and stderr at once.
- `GITHUB_ENV` now blocks only `NODE_OPTIONS`, loudly, as `SetEnvFileCommand` does, instead of
  silently dropping every `GITHUB_*` and `RUNNER_*` name.

The enabling condition, stated by the agent and worth keeping: `workflow_command.rs` was written
as a permissive re-implementation of the command grammar rather than a transcription of
`ActionCommandManager` plus `ActionCommand`, so every *refusal* upstream performs — the
registered-name gate, stop-token validation, the disabled commands, the block-list — was simply
absent while every *effect* was implemented. That is RC-3 with a specific and dangerous shape:
transcribing what a system does while omitting what it refuses to do.

The remaining instance of the class is a **second, independent command grammar** in
`executor.rs::rendered_output_line`, which has the same missing trim.

### New security gap — the masker has no encoded variants

Upstream registers eight encoders on the secret masker itself (`HostContext.cs:103-112`):
`Base64StringEscape` with shift-1 and shift-2 variants, `CommandLineArgumentEscape`,
`ExpressionStringEscape`, `JsonStringEscape`, `UriDataEscape`, `XmlDataEscape`,
`TrimDoubleQuotes`, and `PowerShellPreAmpersandEscape`. Registering them on the masker means
every value it holds, including anything added by `::add-mask::`, is masked in all those
encodings.

Velnor's `Masker` matches the literal value only, so a secret reaching a log base64-encoded,
JSON-escaped or URI-escaped passes through unmasked. Same shape as the two unmasked sinks: the
masker is correct and its coverage is not. Routed to the `runner.rs` owner.

### Deferred, named

- Upstream sets `CommandResult = TaskResult.Failed` when a command throws. Velnor's
  `StepCommandState` has no such field, so a step emitting `::set-env::` now logs two errors and
  still passes if the process exits 0. Needs a field plus a change in `executor.rs::absorb`.
- Upstream reads the `ACTIONS_ALLOW_UNSECURE_*` opt-ins from the job `env:` context as well as
  the process environment; `parse_workflow_commands` has no env context, so only the process half
  is modelled and a workflow setting the flag in `env:` is refused.

### BC-20 resolved on both sides — the lock-in is broken

The Velnor half went first, so the fixture was never briefly failing an assertion that no
longer reflected intent. `crates/velnor-tools/src/main.rs` no longer requires the fixture to
contain `mozilla-actions/sccache-action@`, `SCCACHE_GHA_ENABLED:` and `cache-sccache:`. In
their place, `fixture_rust_scenarios()` and `check_rust_scenarios()` walk the Rust suite **per
job**: the default path is mandatory and must declare no compiler-cache environment, no
sccache/mbx/kache action and no `target/` cache, while sccache is one of five scenarios.
Forbidden declarations are matched against real `env` keys, step `uses` values and
`actions/cache` `with.path` entries rather than substrings, so a scenario may still *read* a
variable it must not *set*. The gate was proven to bite: against the old suite it reports all
five scenarios missing; against the new one it passes (`7f3e14c`, `c3b5907`).

The fixture now implements all five scenarios with their own evidence and comparator legs
(`03e3020`, `0e51b36`): the default path with plain `cargo`; explicit sccache asserting
`MBX_DISABLE=1` and no mbx store wherever the sccache store was mounted; the acceleration
opt-out; cache interaction across source-cache, target-cache, source-change and lockfile-change;
and four simultaneous shards per lane with a hard assertion that they actually overlapped.
Crucially, `compiler_cache_wrapper` is **measured** in the evidence rather than assumed — a
mismatch on the default path means Velnor leaked a wrapper into it.

I also corrected the fixture policy that had codified the lock-in
(`velnor-actions-fixture` `209ff57`): `.github/AGENTS.md` still read *"Rust compile jobs use
mold and local-only sccache v0.16.0 with a 20 GiB bound"*, which would have re-created the
defect on the next fixture change. It now states that the default Velnor Rust path is the
baseline and sccache is one compatibility scenario.

### The acceleration opt-out exists — correcting the verifier audit

The claim that no workflow-level acceleration opt-out exists in Velnor is **wrong**.
`crates/velnor-runner/src/runtime_env.rs:436` reads
`|| (upper.starts_with("MBX_") && upper != "MBX_DISABLE")`: every `MBX_`-prefixed name is a
protected runner-owned variable dropped from workflow-supplied environment
(`runtime_env.rs:201-205`) **except** `MBX_DISABLE`, which is deliberately carved out and
passed through into `job_runtime_env`, feeding `base_env` at `runner.rs:7853`. Velnor relies on
that meaning internally (`container.rs:87`). The opt-out was undocumented and untested, not
absent.

Gap now recorded: **there is no test asserting the `MBX_DISABLE` passthrough.** One line of
carve-out is all that stands between a supported opt-out and its silent removal.

### New structural finding — acceleration is selected before conditions are evaluated

An `if:`-guarded sccache step still switches the **whole job's** acceleration branch, because
`manifest::declares_sccache` filters on `step.enabled`, which an `if:` expression does not
clear. A matrix job containing a conditional sccache step therefore runs *every* variant on the
compatibility branch. This forced the cache-interaction scenario to split `explicit-sccache`
into its own job.

The root cause is that acceleration selection is decided from *declared* steps at admission,
before conditions are evaluated — so no workflow can mix accelerated and unaccelerated variants
within one job. That is a real limitation of the admission-time model, not a bug in the
condition evaluator, and it belongs with the trust/derivation work where the same
"decide-at-admission from declared shape" pattern appears.

### Fixture items now blocked on the capability baseline refresh

These are correctly blocked rather than forced, because the readiness gate the verifier package
built is doing its job: `validate_remote_uses` rejects any `uses:` for an action absent from the
capability snapshot, and the audit already fails loudly with
`coverage/velnor-capabilities.json source_sha is '2858e92…', but the Velnor build under test
reports 'd4d6e93…'; the baseline is stale`.

Required, in order: regenerate `coverage/velnor-capabilities.json` from the v11 build; move
`EXPECTED_MANIFEST_VERSION` from 10 to 11; drop `EXPECTED_KACHE_REF`,
`EXPECTED_KACHE_VERSION` and `validate_kache_contract`; add a `jdx/mr-boxington-action` row to
`fixture-coverage.json` and `action-surface-coverage.json`; remove the
`kunobi-ninja/kache-action` rows **and** the `compat.yml:cache-kache` job, which is now
unadmittable because Velnor's manifest has no such action at all; and remove
`mozilla-actions/sccache-action` from `MICROVM_SUPPORTED`, flipping that coverage row to
`expected-unsupported` — `validate_microvm_compiler_cache` (`manifest.rs:1371-1385`) refuses
microVM for a declared sccache action *or* any `RUSTC_WRAPPER`/`SCCACHE_*` environment, so the
current `microvm: supported` claim is false.

Once the baseline lands, the sixteen admitted mr-boxington inputs split cleanly: `backend=local`,
`version`, `cache-key`, `restore-keys`, `cache-generation`, `save-on-workflow-dispatch`,
`toolchain`, `max-size` and `cache-links` are exercisable from a sixth scenario; `github-token`,
`server-url`, `namespace`, `token`, `token-file`, `oidc-audience` and `server-mode` select a
remote cache backend and should be recorded `admission_only` under this fixture's local-only
store policy.

### The trust split-brain is closed, and trust now fails closed

`dc06705`. Every claim was verified before editing, and the blast radius was larger than
recorded: the ambient `std::env::var` read fed **six** store paths — `container.rs:1046`,
`:1073`, `:1097`, `:1120` (cargo bin, mise installs, mise binaries, playwright),
`storage.rs:107` (`cache_class_path`, the class root for every canonical store) and
`executor.rs:9438` (the persistent actions cache) — while the CLI-derived value gated the Docker
socket, container options (`github_adapter.rs:504`), service-container privilege (`:514`), host
ports (`:536`, `:568`), user secrets (`executor.rs:3611`) and the job trust policy
(`runner.rs:6321`).

**A fourth ambient read was found and fixed**, and its location is the sharpest detail in this
finding: `node/prove.rs:417` `runtime_trust_scope()` also read the environment directly,
overriding the configured scope *in the node routing-proof evidence*. The code whose job is to
detect trust incoherence was itself a source of it.

The structural fix, in a new `crates/velnor-runner/src/trust_scope.rs`: a single
`TrustScopeArg` clap declaration flattened into both binaries, so there is no second
declaration to diverge; `trust_scope::resolve()` as the only constructor of a `TrustScope` whose
inner value is private, called once at the argument-conversion boundary, publishing to a
write-once cell with exactly one writer — **first resolution wins and a later one is refused, so
trust cannot widen after startup**. Both `std::env::var` reads and `cargo_target_trust_scope_from`
are deleted; nothing outside clap reads `VELNOR_TRUST_SCOPE`; an unresolved process fails closed
to `untrusted`.

Threading a parameter into all six consumers would have required editing files owned by other
agents, so the value is resolved once and published instead. Under `cfg(test)` the cell is
per-thread, so one test's resolution cannot leak into another's store paths.

**The default is now `untrusted`, and this is a deliberate breaking change.** The shipped
`velnor.env` ships `untrusted` with an explicit warning. A pool that legitimately runs only
first-party code must now grant `trusted` deliberately; until it does it loses the Docker socket,
privileged options, privileged service containers, host ports and user secrets, and its stores
move to the untrusted namespace at the cost of one cold-cache job. That is the correct direction:
the previous default failed open, and the flag that was supposed to close it did not reach the
stores.

The proving test sets `VELNOR_TRUST_SCOPE=trusted` **and** `--trust-scope public` at the same
time and asserts through the real `github_job_container_spec` that all six gates and all seven
trust-scoped store paths observe `public`, with no `trusted` component anywhere. A second test in
`crates/velnorctl/tests/trust_scope_single_source.rs` renders the flag from both binaries' clap
commands and fails if the name, default or environment binding differ.

### The same duplication class remains in the rest of the argument trees

Only the trust-scope flag was unified. These four still diverge between `service.rs` and
`velnorctl/runtime.rs`, so which limits a job receives depends on which binary launched it:

| Flag | `service.rs` | `velnorctl/runtime.rs` |
| --- | --- | --- |
| `job_cpus` | `""` | `"2"` |
| `job_memory` | `""` | `"4g"` |
| `job_peak_bytes` | 30 GiB | 4 GiB |
| `node_action_image` | `""` | `"velnor/node-actions:latest"` |

The empty values on the `service.rs` side are exactly the condition recorded in BC-17 — mbx and
Cargo sizing themselves to the whole machine because nothing tells them otherwise. So this is
not a missing default but a *divergent* one, and it is being handed to the resource-budget
package with the instruction to derive these from the host budget rather than pick a side, and
to add the same cross-binary guard test even if it does not unify them.

Also still to correct: `content/docs/getting-started.mdx:88` documents
`VELNOR_TRUST_SCOPE=trusted`.

### BC-24 closed — durable sinks now mask, and the masker covers encoded variants

`e050683`. Both unmasked sinks are fixed: the step summary is scrubbed line by line into a
masked copy, matching `FileCommandManager.cs:256-284` (read line, mask, write into a scrubbed
file, and queue only the scrubbed copy) — line-by-line also reproduces upstream's CRLF-to-LF
normalisation and terminating newline. Annotations now pass through one masker in both the
per-step results and the job-level list of `CompleteJob`, and masking happens **before** the
length cap, as `ExecutionContext.cs:801-805` does.

One deliberate divergence, flagged in the code: upstream masks only `Issue.Message`, because its
title and path live in an untyped `Issue.Data` bag. Velnor carries `title` and `path` as typed
fields on the same durable payload, and `::error title=<secret>::` puts a secret there — so
Velnor masks them too. Diverging *toward* more masking on a field upstream does not have is
correct.

The encoder gap is closed and was larger than I reported: upstream registers **eleven** value
encoders at `HostContext.cs:103-113`, not eight — the nine I named plus `Base64StringEscapeShift2`
and `PowerShellPostAmpersandEscape`. All eleven are implemented from the bodies in
`Sdk/DTLogging/Logging/ValueEncoders.cs` and expanded inside `Masker::new`, so every value the
masker holds — including `::add-mask::` additions — is covered in every encoding, exactly as
`SecretMasker.AddValue`/`AddValueEncoder` do, with empty encodings dropped per their guard.
Details that only reading the source would give: `ExpressionStringEscape` is `'` → `''`
(`ExpressionUtility.cs:259-262`), `TrimDoubleQuotes` returns empty unless the value is longer
than eight characters *and* quoted at both ends, and both PowerShell encoders refuse sections
under six characters.

The upstream caps are implemented: 4096-character annotation truncation after masking, ten
published per issue type with counters clamped to the same cap (upstream only increments while
under it, so its published count is `min(total, 10)`), `step_number` populated from the record
order instead of hardcoded `None`, and the telemetry limits of three issues at 256 characters.

Coordination note kept for the record: the *increment* site is in `workflow_command.rs` and
increments unconditionally. Clamping at the publication boundary produces byte-identical
published values, so nothing is left wrong, but structural parity with upstream would put the
"only count while under the cap" rule at the increment site.

### BC-32 and BC-33 closed — the timing SLOs can now report a breach

`914cf59`. Fixed as a class rather than as assignments: every duration a path may not measure is
now `Option<u64>`, with `None` meaning not measured and filtered out of the percentile exactly as
`queue_ms` already was.

The audit of the sibling fields found a second real defect. `finalize_ms` is measured on every
path that builds a record and was fine, but `container_boot_ms` was not: the microVM backend's
`script_job_result_from_outcome` emitted `ExecutionTimings { first_step_ms: 0, checkout_ms: 0,
container_boot_ms: 0, steps_ms: 0 }`. All four became `Option`, and since `first_step_ms` feeds
an SLO, the zeros had been silently *improving* the pickup-to-first-step number as well.

The percentile guard reuses the benchmark crate's rule (`n >= 1/(1-q)`: p50 needs 2, p95 needs
20, p99 needs 100) and its `Unsupported` shape, plus a `NoSamples` case, so the doctor now prints
`UNSUPPORTED` or `NO-SAMPLES` where it previously printed `PASS`. The rule was **mirrored rather
than imported**, with a reason I accept: `crates/velnor-bench` is maintainer-only tooling that is
never shipped in the deb, and importing it would pull the benchmark harness into the shipped
daemon. Deferred and named: hoist the statistics module into `velnor-model` so both crates share
one implementation.

BC-33's structural fix was done rather than deferred. A typed `job-timing.jsonl` sink is written
and read through one serialiser, bounded to 512 records per slot, and `JobTimingRecord` gained a
typed `completed_at`, so ordering no longer depends on a string-compared log prefix whose format
is maintained for an unrelated web-UI reason. The human `"job-timing "` lifecycle line stays as
forensics and nothing parses it — a test asserts that a record present only in `lifecycle.log`
is not counted as a sample.

Both secret tests assert on the **uploaded bytes** rather than an intermediate value: the byte
sequence handed to `upload_step_summary`, and `serde_json::to_vec` of the real
`RunServiceCompleteJob` that `complete_job` posts. The agent stated the boundary it could not
cross rather than overclaiming: there is no HTTP-level assertion, because the summary upload
validates a signed blob URL and refuses a loopback mock.

### The shared branch tip does not compile

Verified at `3749065`: `crates/velnor-runner/src/container.rs:10` imports
`crate::execution::docker::host_budget`, but `crates/velnor-runner/src/execution/docker/` is
**untracked** — it exists only in one agent's working copy, while `git ls-tree` shows
`execution/docker.rs` as a single file. Every consumer fails with `unresolved import` and
`module docker is private`.

Two agents were already blocked by it; both anchored their gate evidence at `709583a`, the last
tip that builds, and one spent part of its run proving the breakage was not its own. Routed to
the owner with instructions to commit the module together with its consumer and to verify
against a `git archive` of HEAD rather than the shared worktree.

This is the shared-tree hazard in its most damaging form: not a lost edit, but a commit that is
individually correct and collectively broken, because the index and the working tree disagree
about what exists. It is also an argument for the pin-integrity discipline being extended to a
"HEAD must compile" check.

### BC-11 closed — expressions are parsed, and every recorded divergence is fixed

`c4afada`, `5efb20d`, `7c0e201`, `c0338f5`, `26002ce`. A real lexer, parser and evaluator now
live in `crates/velnor-runner/src/expression/`, transcribed from upstream. **Correction to this
plan's own citations: the upstream path is `src/Sdk/DTExpressions2/Expressions2/`, not
`DTExpressions/`.** The code carries the corrected citations.

| | Before | After (matches GitHub) |
| --- | --- | --- |
| D-3 | `if: steps.x.outputs.count` with `count=0` skipped the step | runs; only the empty string is falsy |
| D-4 | `if: env.UNSET == ''` was false, because the expression became its own source text | true; a missing value is null and coerces to `""` |
| D-5 | `if: github.run_number > 5` on run 1 **ran** | skipped |
| D-6 | `if: startsWith(github.ref, 'refs/tags/')` on a branch push **ran the release step** | skipped |
| D-7 | an unevaluatable condition ran the step | typed error; the step fails, the job continues |

The semantics were taken from source rather than intuition, and several are surprising enough
that only source would give them: mismatched coerced kinds and NaN compare false in *both*
directions, so `1 > 'a'` and `1 < 'a'` are both false while `null >= 0` is true, because `>=` is
literally "equal or greater"; strings compare `OrdinalIgnoreCase`; objects and arrays compare by
reference and are never coerced, and are always truthy even when empty; objects are ordered and
case-insensitive except `env`, which is case-sensitive off Windows; `And`/`Or` return the operand
value rather than a boolean; numbers accept `0x`/`0o` and `Infinity` but reject `"infinity"` and
`"nan"` because .NET's `Double.TryParse` is stricter; and rendering is `G15`.

Two further upstream behaviours were restored along the way: `toJSON` is pretty-printed to
upstream's exact layout, and status-check detection now walks the parsed tree instead of matching
a substring, so `if: env.X == 'always()'` is no longer mistaken for a status check.

**A regression the agent caused and then fixed, worth recording as a design constraint.** Velnor
renders `${{ }}` twice — at plan time from the job message, then at step time. Once missing values
correctly became null, plan-time rendering collapsed not-yet-known values such as
`${{ env.EDIT_URL }}` to `""`. `resolve_job_context_expressions` now parses each span and defers it
verbatim when the tree reads a runtime context or a runner function, which also subsumes an ad-hoc
`steps.*.outputs.*` substring check that was in `action.rs`. Any future change to two-phase
rendering has to preserve that property.

The old path was deleted entirely — twenty-two functions, listed in the commit — with nothing left
alongside it. Gates: `cargo fmt` clean, and `cargo nextest run --locked -p velnor-runner --features
test-support` reports **1564 passed, 1 skipped, 0 failed**, including 35 expression tests and the
five condition regressions.

### Still divergent after T-004, and now precisely located

1. **A second, independent string-rewriting evaluator survives** at
   `script_step.rs:392 evaluate_format_expr_with_lazy_args` together with `resolve_context_path`,
   used to render composite and local-action inputs at setup time. Same bug class, different
   boundary. This is the third implementation the red team predicted; two are now gone and this
   one needs its own package.
2. **`${{ }}` interpolation is not fail-closed.** Upstream fails template evaluation, and therefore
   the step, when a span cannot be evaluated; Velnor keeps the raw span. Step *conditions* are now
   fail-closed, which is what D-7 required, but interpolation is not. Documented at the call site.
3. `hashFiles`' `--follow-symbolic-links` is parsed and ignored — Velnor's `hash_files` has no
   symlink mode.
4. Upstream's evaluation-memory accounting (`MemoryCounter`, `ResultMemory`) and condition trace
   rendering are not implemented; only the depth and length limits are.

Pre/post conditions are fail-*closed* and reported, where upstream fails the owning step. That
difference is deliberate and named: Velnor's pre/post steps have no result row to fail, so the
lifecycle package owns it.

### Provenance note

`executor.rs` was staged wholesale during this package while another agent was editing it, so two
commits also carry roughly 120 lines of that agent's in-flight work. Nothing was lost, but the
provenance is mixed. This is the same shared-index hazard as before, and it is the reason the
commit rule is now an explicit pathspec.

One commit (`f87b977`) appeared unpushed from the agent's stale local branch; it landed as its
rebased twin `26002ce`, and the deleted symbol is absent from the tree — verified, nothing missing.

### BC-29 closed at its worst instance — argv injection and secrets on argv are now unconstructible

`37c3725`, `be9aa1c`. Both defects were confirmed exactly as reported, and both fixes remove the
unsafe construction rather than patching the sites, which is what RC-4 requires.

**Argument injection.** A new `crates/velnor-runner/src/docker_argv.rs` makes the defect a type
error. `DockerCommand`/`DockerArgv` are the flag phase; the *only* transitions into the operand
phase are `image(&ImageReference)` and `operands()`, and both emit `--` first. The operand-phase
types expose no method that appends a flag, so there is no way to write a call site that places an
operand where Docker will read a flag. The `vec!["run".into(), …]` idiom is gone from
`container.rs` and `guest_runtime.rs`. `ImageReference` parses the actual OCI distribution
reference grammar — domain and port, path component rules, the tag charset, `algo:hex` digests,
the 255-byte name cap — and an image can only reach a command line through it, so
`runs.image: docker://--privileged` is refused at the single place the scheme is stripped, with
the repository named and the value not echoed.

**Secrets on argv.** The builder has **no `-e NAME=VALUE` emitter at all**. Callers use `env()`,
and rendering materialises a mode-0600 `--env-file` created `O_CREAT|O_EXCL|O_NOFOLLOW` and
unlinked on drop, placed exactly where the entries were declared so Docker's last-wins override
order is preserved. Values an env file cannot carry — newlines, whitespace in the name — become a
bare `-e NAME` forwarded from the client's own environment. This covers the job container,
workflow services, node and Docker action sidecars, `docker exec`, and the MicroVM guest path.
The guest step script no longer rides on argv either: it goes to `sh -s` on stdin, which also
stops the script and its environment leaking into the `HostDockerInvoked` and `GuestDocker`
events, both of which log the entire argv. Prepared commands additionally run a fail-closed audit
that refuses the command if any masked secret reached the finished argv.

### Correction to my own correction — redaction

I retracted the "five copies that disagree" finding on the grounds that there is exactly one
*secret masker*. That remains true, and the retraction was right about the masker. But it
over-corrected: there were **six redaction implementations across the workspace** with three
sentinels, and one disagreement was real and load-bearing — `records.rs`'s
`is_exact_redaction`/`json_value_is_exact_redaction` accepted only `[REDACTED]`, so a
runner-masked `***` string failed the store validator, exactly as the security audit said.

The right reading is that the two reports were about different scopes: one masker for *secrets*
in the runner, and several unrelated redactors elsewhere in the workspace. Both were right; my
retraction collapsed the distinction.

Now consolidated (`435a34c`) into `crates/velnor-model/src/redaction.rs`, the only crate both
`velnor-runner` and `velnor-control` depend on, sitting beside the existing
redaction-by-construction module. One sentinel, `***`, matching upstream's `SecretMasker`.
Upstream's rules are implemented: the literal value, **every line of a multi-line secret** so that
line-oriented output cannot evade a whole-value match, and the encoder forms — JSON string escape,
URI data escape, XML escape, backslash escape, surrounding-quote trim, base64 — longest match
first, with values under three characters never registered in any encoding. `velnor-control`'s log
and diagnostics redactors and `velnor-tools`' fleet-policy client are migrated, and `records.rs`
now accepts the single sentinel while correctly treating `[REDACTED]` and `[redacted]` as *not*
redacted.

Remaining for the `runner.rs` owner: delete `MaskPatterns`/`Masker`/`mask_all` and call the shared
masker. They already use `***`, so only the weaker matching rules differ. `script_step.rs` and a
test-local sixth copy in `crates/velnor-runner/tests/idle_scaling.rs` move with it.

### The `RUNNER_TEMP` class was already closed

Re-verified independently: `RUNNER_TEMP` is still mode 1777 — deliberate, matching GitHub-hosted
`/tmp` — and step-file names are still predictable, but the exploit is closed. All step-file I/O
is descriptor-relative through a `NoFollow` directory handle, writes stage into an
`O_CREAT|O_EXCL|O_NOFOLLOW` temporary and rename over the name, reads error on a non-regular file,
and no `fs::write`/`fs::read_to_string` by path remains in non-test code. The regression test
covers the script, output and summary paths. The optional remaining hardening is a per-step
private directory, which would remove the predictable-name property itself.

### Boundary crossings, disclosed

Two fixes could not be delivered without narrow edits to `executor.rs`, and the agent made them
rather than stopping: three argument builders had to become fallible, a four-line call-site change;
and the twenty-one `CommandRunner` test doubles observe argv, which no longer carries environment,
so each gained one line expanding the env file under `cfg(test)`. Eleven assertions of the form
`!call.contains("GITHUB_TOKEN=…")` became delivery assertions with a comment recording why:
confidentiality is no longer a property of those call sites, it is enforced by construction and
tested where the construction happens. That is the correct place for the assertion to live.

### BC-9 and BC-10 closed — credentials are reaped, and checkout stopped doing work it did not need

`1d5ecf8`, `35e09bd`.

**Credential lifetime.** Both leak paths were confirmed: `runner.rs:8109` returns early before the
cleanup at `:8126`, and a crash skips it entirely, while `checkout.rs:311-318` printed to stderr
and returned `Ok`. The fix uses the kernel as the liveness oracle rather than guessing: a
credential is journalled *before* it reaches disk, in a file the owning process holds an
**exclusive flock** on, so however the process dies the lock is released.
`reap_stale_checkout_credentials()` then scrubs every entry whose lock is free while live jobs'
entries stay locked. Cleanup is unconditional, tolerates only git's exit 5 ("key not set"),
*verifies* the config afterwards, scrubs any residual `extraheader`, and returns `Err` if one
survives — which the caller already turns into a job failure.

The agent declined to remove the persisted credential entirely, correctly: `persist-credentials:
true` is upstream's default and later steps legitimately use it. What it removed is the write
nobody asked for — the `lfs: true` path called `persist_git_credentials` *before* the fetch
regardless of the setting, and cleanup then skipped it. LFS now authenticates through the same
per-command `GIT_CONFIG_*` environment the fetch uses, so `persist-credentials: false` with
`lfs: true` writes nothing to disk.

**A GHES defect found independently of the ignored input.** `checkout_clone_url` hardcoded
`https://github.com/{repository}.git` for external repositories, so a GHES job checking out a
second repository fetched from the public site. Now derived from the job's own self-repository
clone URL. This is the same defect the ignored `github-server-url` input would have caused, but it
did not need that input to trigger.

**Mirror and hydration.** The store key is now `{host}__{owner}__{repo}`, with scp-style
`git@host:owner/repo.git` split correctly — that form previously produced a mangled owner.
Corruption detection now requires git to recognise the repository, `for-each-ref` to read, and an
object named by a ref to exist; a failing mirror is renamed aside under the exclusive lock and
rebuilt, and the warn-and-fall-back path is gone, so mirror failure fails the checkout step
carrying git's own exit code. The mirror fetches only the wanted revision, pins it under
`refs/velnor/*`, holds the exclusive lock only for create and repair, fetches under a **shared**
lock, and short-circuits with zero network when the commit is already present. The workspace no
longer fetches from the mirror at all — it hard-links objects and writes refs directly.

Measured on a synthetic 6 MB repository with 250 `refs/pull/*`:

| | before | after |
| --- | --- | --- |
| cold mirror | 7448 KB, 1.13 s | **1296 KB, 0.45 s** |
| five further matrix legs on the same commit | 3.36 s, five full mirror fetches | **1.90 s, zero fetches** |
| object bytes copied per workspace | 264 KB | **0** (inode identity asserted) |
| unique disk for five workspaces | 1320 KB | **92 KB** |

The safety argument for hard links is explicit and I accept it: alternates were rejected because
the job container does not mount the mirror store, so the workspace would be a broken repository
inside the container; a fetch only *adds* object files and never rewrites a linked one, with
in-progress files carrying a `tmp_` name; mirror gc cannot reach pinned revisions and is disabled
anyway, and even an unlinked pack keeps its inode alive through the link; workspace deletion
unlinks names only; rebuild happens only after the probe declared the mirror broken, under the
exclusive lock; and EXDEV falls back to a copy, so correctness never depends on links existing.

**One deliberate semantic deviation, flagged rather than buried:** with `fetch-depth: 1` the
workspace is no longer shallow — it carries the mirror's full linked history at zero byte cost —
so `git rev-parse --is-shallow-repository` differs from upstream. Everything visible in the
worktree is identical. This also makes the mtime pinning *more* consistent, because
`git log -1 --format=%ct` now resolves on any commit rather than only a shallow tip.

### Confirmed again, independently: BC-8 was correctly withdrawn

The agent verified the fail-closed path itself rather than taking my word: `manifest.rs:434-449`
declares nine `InputRule`s, `validate_inputs` (`:1526-1560`) raises a violation for any unmatched
name, and `runner.rs:5736` calls `validate_job_with_context` before anything runs. No rejection
logic was added and `submodules` was not implemented.

Implementing `submodules` properly is blocked from that file set: it needs a `CheckoutPlan` field,
and `CheckoutPlan` is built as a struct literal in nine places across `executor.rs`, `runner.rs`
and `execution/tests.rs`.

### Routing queue from T-011

1. `runner.rs` startup should call `checkout::reap_stale_checkout_credentials()` once. Today it
   runs at the start of every checkout, so a leak is reclaimed on the next job but not while a host
   sits idle — the same gap the security audit found in `leftover_disk.rs:14`, which only reaps at
   90% disk.
2. `executor.rs:12183` prints `submodules` as a literal `false`; it should reflect the plan once
   the field exists.
3. The guest lane has no post step, so the credential scrub is an in-script handler covering the
   abort case; a real post-job step is the structural fix.
4. `velnor-tools/src/main.rs:2110-2117` should stop advertising `submodules`.

**Recommendation, endorsed: unify the two checkout implementations.** The guest shell script has
already drifted in ways that mattered — it passed `--no-tags --tags` together, had no credential
cleanup, and inherited credentials from a reused workspace, none of which were true of the host
path. The guest lane should call the same Rust checkout over the guest command channel rather than
maintaining a second implementation in shell where every fix must be made twice. That is BC-23's
shape again: a second implementation of a thing that already exists, drifting silently.

### BC-17 addressed — one derived host budget, and honest counter-evidence

`dd43606`, `03e77ef`. The budget is now derived rather than assumed:
`HostBudget::observe(root, parallelism)` takes the **minimum** of `available_parallelism`, every
cgroup v2 `cpu.max` from the unified root down the `/proc/self/cgroup` chain (so an ancestor quota
is not missed), `cpuset.cpus.effective`, and the `velnor-jobs.slice` quota; memory is the minimum
of `MemTotal` and every applicable `memory.max`. The root is injectable, so tests drive synthetic
`/proc` and `/sys/fs/cgroup` trees. An unobservable value becomes
`Observation::Unobservable(reason)` and is never cached and never replaced — downstream then emits
**nothing**: no `--cpus`, no `CARGO_BUILD_JOBS`, no scheduler sizing, asserted by test. That
follows the pattern the Docker fact cache established.

`per_slot(slots)` floors the share so N slots always fit inside the budget, and the derived values
are injected after workflow env and createOptions: `--cpus`, `CARGO_BUILD_JOBS`, `MAKEFLAGS`,
`MBX_SCHEDULER_CPUS`, `MBX_SCHEDULER_MEMORY`, plus a `VELNOR_JOB_BUDGET` string carrying the
derivation.

**Two of my premises were wrong, and the agent corrected them from source.** The slice at
`host_cpus × 95%` is a deliberate *aggregate* ceiling for all jobs, not a per-slot allowance — its
own test says so (`tests/node_arch.rs:265`) — and dividing it by slots would idle the host at
`--slots 1`. The real defect was that nothing divided the budget **for the container**. And mbx's
pool is machine-wide *by design*, one permit pool under the cache dir; what it cannot do is see
the slice quota, which is why the per-slot cap is expressed as `CARGO_BUILD_JOBS` rather than by
shrinking the pool. That was checked against mbx v1.7.0's `config.rs` and
`docs/configuration.md`, which document exactly that `CARGO_BUILD_JOBS` caps one build's permits
without shrinking the machine-wide pool.

Precedence: the daemon budget wins over a workflow-set value and a declared limit may only narrow,
never widen — a workflow cannot see the host it shares — and the override is *named* in the
`VELNOR_JOB_BUDGET` notice rather than applied silently.

**The measurement is counter-evidence, and it is recorded as such.** Four concurrent cold
`cargo check` runs in separate worktrees, two repetitions: `-j18` each took 126.3 s and 119.2 s;
the capped `-j4` each took 147.4 s and 142.9 s — the cap costs roughly 17-20% on this host. The
agent did not dress that up, and the explanation is sound: this host is macOS, so it has **no
cgroup quota and no `velnor-jobs.slice`**, and therefore cannot reproduce the CFS-throttling
condition the change exists to prevent. What was measured is the cost side of the trade with none
of the benefit, and with mbx absent so the machine-wide permit pool that hands an idle sibling's
share back was not in play.

It also correctly reported that the benchmark's `rust/concurrent-jobs` scenario has **no power for
this defect**: its measured iteration is a warm no-op `cargo check`, and the two
identical-by-construction conditions differed 2×, which bounds the noise above any effect.

**This is therefore an open question, not a settled improvement.** Settling it needs a Linux host
with the slice quota in place, mbx installed, and the runner-driven benchmark scenarios — the same
infrastructure gap that blocks the acquisition and persistent-host half of the matrix. Recorded in
§12 as a decision requiring evidence rather than claimed as a win.

The duplicated `DaemonArgs` declaration is partly closed: the `velnorctl` side is aligned to the
daemon's declarations and a guard test now fails if `job_cpus`, `job_memory`, `job_peak_bytes`,
`node_action_image` or `emergency_reserve_bytes` diverge again — negative-checked by reverting one
default and confirming the failure. Full unification still needs `service.rs`.

### Corrections to my own coordination notes

- **`709583a` does not build.** I cited it to several agents as the last good tip; a clean archive
  of it fails with 13 errors, because `executor.rs` there already calls a newer `container.rs`
  API. Snapshot SHAs must be chosen by testing them, not by trusting a note.
- **The `velnor-runner` lib test suite is badly flaky under parallelism**, 21 to 46 failures
  across runs with an identical failure list with and without a given change. Only
  `--test-threads=1` is stable. The cause is that many tests mutate process environment
  concurrently — a real hygiene defect that undermines every gate this program runs, and it should
  be fixed rather than worked around indefinitely.
- The shared worktree's branch ref diverged from origin, carrying a duplicate of a commit already
  pushed. Verified the duplicate's trees are identical to the pushed version, so a rebase dropping
  it is correct; routed to the agent that holds the tree rather than reconciled underneath it.

### V-3 — baseline refresh is one command, and a recommended split was correctly refused

Fixture commits `e129032`, `136161a`, `2d1669c`.

`just refresh-capability-baseline` now runs the real `velnor-runner capabilities export` through the
**same** `load_runner_baseline` the readiness gate uses, writes that document verbatim, carries the
manifest identity into `fixture-coverage.json`, and then re-runs readiness — so a refresh that does
not certify fails. The checked-in file is never read or merged into. Two guards were added along the
way: the runner build no longer inherits the fixture's `RUSTUP_TOOLCHAIN`, which had been silently
overriding the runner's own `rust-toolchain.toml`, and deriving the SHA from a checkout requires
`manifest.rs`, `action.rs`, `build.rs` and `Cargo.toml` to be committed. The two prose restatements
of the baseline SHA in `README.md` and `GOAL.md` were deleted — those copies are what three earlier
commits had been hand-retyping.

Items 1 and 2 of that package were already done by the oracle work: the manifest is at v11, the
`EXPECTED_*` constants were deliberately deleted rather than updated, and the Kache contract, its
coverage rows and the `compat.yml:cache-kache` job are all gone — the only surviving mention is a
mutation test that replays the exact identity swap. Diffing the runner export against the
checked-in baseline showed the **only** disagreement was `source_sha`; actions, workflows, refs,
subpaths and inputs matched exactly.

The false `microvm: supported` claim for sccache is fixed, and a new test now holds every microVM
disposition to the supported set. `_rust-suite.yml` already carried a comment documenting the
opposite — that is how far the claim and the behaviour had drifted.

**The recommended Mr Boxington coverage split was refused, and the refusal is correct.** At the
admitted ref the action's own bundle contains
`function AV(t){if(t==="github"||t==="server")return t;throw new Error('backend must be "github" or "server"…')}`
— the string `local` appears nowhere in it. Velnor runs it as a pinned generic Node action, so
`compat.yml`'s existing `backend: local` **must fail on both lanes**. The split is therefore not
local-versus-remote but github-backend-versus-server-backend: `cache-key`, `restore-keys`,
`cache-generation` and `save-on-workflow-dispatch` are GitHub-cache-backend inputs, and
`github-token` resolves mbx release metadata rather than selecting a remote store. I had passed
along the wrong split; the agent checked the action's source instead of applying it.

That also yields a new Velnor-side defect: **`MR_BOXINGTON_INPUTS` admits `backend: local`, a value
the pinned action always rejects.** Admission accepts something that cannot work.

Three previously reported surface defects are confirmed at current line numbers:
`velnor-tools/src/main.rs:2375` lists `submodules` as a supported checkout input while admission
rejects it; `:2520` records `actions/setup-python` with no manifest capability at all, and
`baptiste0928/cargo-install` and `dtolnay/rust-toolchain` share that problem; and `clean` and
`fetch-tags` are admitted and honoured but missing from the supported list, so `target_audit` bails.

**The readiness audit is red, and that is the correct state.** No check was removed, skipped or
loosened — three were added. It is red because the shared runner checkout advanced twice during the
work and its tree does not currently compile (`missing field 'cancellation' in initializer of
JobExecutionState`, from the in-flight cancellation package), so the refresh cannot run against it.
One command clears it once the runner builds.

Flagged for whoever resumes: binding the baseline to an exact commit SHA turns the fixture gate red
on **every** unrelated runner commit, which is what produced the hand-retyping commits in the first
place. The single-command refresh mitigates that but does not remove it, and a content-derived
identity — hashing the capability document rather than the commit — would.

### Coordination alert — pending Docker cleanup review

A read-only review of the concurrent lead's uncommitted Docker cleanup found two release-blocking
conditions: `reclaim_unlabeled_testcontainers` force-removes IDs selected only by the shared
Testcontainers label plus an empty job label, reopening the unrelated-host deletion that `c091338`
closed; and `unlabeled_testcontainer_ids` silently drops malformed `docker ps` rows, allowing a
partial listing to drive deletion. T-004 route authorization is unchanged. Restore the fail-closed
ownership validator and reject malformed listings before this cleanup reaches the shared branch.

## 16. Pause handover

All five packages stopped cleanly on instruction. Six commits that were stranded on a diverged
local ref have been cherry-picked onto the origin tip, verified with
`cargo check --workspace --all-targets --features velnor-runner/test-support` on a clean
`git archive` (exit 0, zero errors), and pushed: `2a85017`, `fe5baf9`, `5ba8cd3`, `e2f369e`,
`ccbd396`, `95ab8ca`. A duplicate of an already-pushed commit was correctly dropped as
already-applied.

### T-018 cancellation — model built, deliberately inert

`2a85017` lands `execution/cancel.rs`: the token, its registry and one termination ladder, with
upstream's timings cited from source (`ProcessInvoker.cs:32-33` 7500/2500 ms, escalation at
`:443-447`; `JobDispatcher.cs:1280-1285`; `StepsRunner.cs:146-187`; the fresh unlinked post source
at `ExecutionContext.cs:436`, `:1384-1395`; `ContainerOperationProvider.cs:57-63`; and
`JobCancelMessage.cs` carrying the `Timeout` that `runner.rs` currently discards).

The executor half — process groups for every host child registered at the single spawn seam, a
required `cancellation` field on `DockerJobEngine` and `JobExecutionState`, truthful `cancelled()`
with `success()`/`failure()` false under cancellation, `continue_on_error` gated on not-cancelled,
and post steps under a fresh unlinked token — **compiles and is gate-clean but is not
test-verified**, and is preserved as a patch at
`scratchpad/t018-uncommitted-executor.patch` (2232 lines, five files) rather than committed.

The agent's own caveat is the important one: an earlier green test run was invalid because a second
agent had overwritten its scratch directory, so those results never compiled its `executor.rs`.
Nothing requests cancellation in production yet — the model is inert **by choice**, so nothing
looks wired that is not. Requirements 3, 5, 6, 7 and 8 are unstarted.

### T-019 completion durability — landed, with the schema discipline intact

`fe5baf9`, `5ba8cd3`, `ccbd396`. `JOURNAL_SCHEMA_VERSION` 3 → 4 carries three events:
`JobTerminalResult` written before payload serialisation (closing the crash window that converted a
green job into a synthetic failure), `CompletionAttemptFailed` as a durable attempt counter so
recovery terminates across restarts, and `CompletionUnresolvable` — whose reducer **refuses it
unless the durable state already proves the budget is exhausted**, so the terminal state is proven
rather than asserted by a caller.

The ordering defect the red team predicted was found and fixed: the old code stamped `user_version`
only when it was 0, so a v4 binary would leave a v3 stamp and a v3 binary would then reopen the
journal and silently skip events it did not know. `open` now migrates and stamps before any event
may be written, which is what makes the silent `continue` safe to turn into a hard error. A version
behind or ahead of its physical shape is refused without mutating the file.

The operator-facing decision is right: an unresolvable completion tells the operator plainly that
GitHub was never told and will time the job out, releases the slot, and preserves the event — and
deliberately does **not** send a synthetic conclusion, because fabricating a result that could not
be delivered is worse than an honest timeout. `remote_acked` stays false and the send claim is
never released, so abandonment can never become a second terminal send.

Seven fault-injection boundaries pass, including budget-does-not-reset-across-restart and
zero-sends-on-a-fenced-generation.

### T-021 last string-rewriting evaluator — deleted

`e2f369e`, `95ab8ca`. **Correction to my framing:** the survivor rendered the *script step's* `run`
and `working-directory` inputs, not composite or local-action inputs — those already go through the
migrated path. Six functions deleted, no shim.

The setup-time versus runtime split was lifted into `expression::reads_runtime_context` rather than
duplicated. One genuine difference from the established pattern, and the reason it needed thought:
the whole expression is a `format()` call whose template holds the script's literal braces as
`{{`/`}}` escapes, so deferring the span wholesale would re-emit those escapes *and* truncate at the
first `}}`. Deferral is therefore per argument, with argument source recovered verbatim through the
real lexer rather than by re-printing the tree or splitting on commas.

Upstream is fail-closed here — `ActionRunner.cs:174-185` → `EvaluateStepInputs` →
`context.Errors.Check()` → `StepsRunner.cs:335-344` — and Velnor now is too. Named divergence:
upstream fails the *step*, Velnor fails the *job*, because Velnor renders at setup when no step row
exists yet.

### T-020 storage — committed locally, conflicts with the other lead's work

`d76eab7` is **not** on origin: it conflicts in `cache.rs` with `dd93963`
("isolate cache reclaim storage context"), which landed from the other lead's effort while T-020
was running. That is the first genuine cross-lead conflict of this program, and I am not resolving
it unilaterally — both sides changed reclaim behavior, and the merge needs the intent of each.

Its content: a `StoreCatalog` as the sole path constructor (making the artifact-store path defect
unrepresentable rather than fixed), a `HostCapacity` from `statvfs` with Docker accounted, lease
classes for the emergency-managed stores, a reaper that consults claims and leases instead of
`docker ps` alone, and disk pressure as a total transition function with a bounded
`RefuseUntil`. Tests assert that a job mid-checkout, mid-artifact-upload and mid-target-publish is
not reaped while a genuinely abandoned workspace is.

One item needs an explicit decision when it lands: `prune_owned_builder` was dead code and now
actually runs `buildx prune --max-used-space 0B` during emergency reclaim, with no lease covering
the BuildKit store. The agent's reasoning — the cache is regenerable and BuildKit will not prune
records in use — is sound, but it is the only change that becomes *more* destructive, and it should
be a deliberate call rather than a side effect.

### Open items carried forward

- The `Acquired` unjournaled completion branch is prepared but not applied; it is fail-closed only
  once the provisional acquire marker exists.
- The provisional-marker design is specified and **gated on one upstream fact**: whether the 409
  reply from `acquirejob` names the acquiring runner. If it does, the 409 is the ownership oracle;
  if it does not, the oracle must become a probe, which is a materially different design. Verify
  before building.
- `CommandResult = TaskResult.Failed` needs three edits across two files nobody owned at once.
- The runner test suite is unstable under parallelism from a shared-`/tmp` and process-env race in
  `cache`, `container` and `executor` tests. Two packages independently hit it and neither could
  attribute failures without a baseline diff. This undermines every gate in the program and should
  be fixed before it costs another package a day.

## 17. T-020 storage — recovered, merged, and safety decision recorded

The `d76eab7` object was recovered from the local object database/reflog and compared with
`dd93963`. The merge keeps `dd93963`'s explicit `StorageLayout` snapshot through cache listing,
pointer protection, and reclaim while adding d76's catalog, host accounting, leases, claims, and
bounded disk-pressure model. The storage package landed in `2036c30` and the compatibility entry
point in `5af89ef`; the explicit BuildKit safety follow-up landed in `371d3c1`. All three
commits carry DCO signoff and the Codex co-author trailer.

The changed runner paths are `cache.rs`, `host_capacity.rs`, `leftover_disk.rs`, `lib.rs`, and
`store_catalog.rs`. Store roots now come from `StoreCatalog`, including the artifact root; canonical
and legacy paths use one explicit layout snapshot. `HostCapacity` uses `statvfs` plus Docker's
`system df` accounting. `ScopeLease` and `FilesystemCoordinator` publish class-typed leases under
one snapshot lock, while the leftover reaper combines Docker liveness with leases, job claims, and
a bounded workspace-idle floor. `DiskPressure` is total and absorbing at `Deregistered`; every
degraded refusal is a bounded `RefuseUntil`.

Decision: emergency reclaim **does not** invoke `prune_owned_builder`. BuildKit is Docker-owned and
the current storage package has no BuildKit lease or job claim that spans its mutable content;
`buildx prune --max-used-space 0B` could therefore remove live build state even when a path looks
idle. The explicit owned-builder function remains available for a future caller that proves that
lease. No broad Docker/system/volume/builder prune was added.

Still blocked by the task boundary: apply `StoreCatalog::for_job_temp(...).artifacts_run(...)` at
`executor.rs:9400-9408`; move disk-pressure admission ahead of acquisition at
`runner.rs:2588-2646` and `2751-2760`; and publish the three missing lease classes at
`runner.rs:5688-5695`. Those files remain untouched by T-020.

## 18. Continuation — capability/runtime repair, race isolation, and forbidden-boundary report

### T-001 capability surface — repaired and re-bound to executable dispatch

The tools-side target audit now derives its checkout surface from the runner manifest and asserts
the exact set: `clean`, `fetch-tags`, `fetch-depth`, `lfs`, `path`, `persist-credentials`,
`ref`, `repository`, and `token`. `submodules` is explicitly absent and remains rejected by
admission. The target action inventory was reduced to actions that actually occur in the manifest;
`actions/setup-python`, `baptiste0928/cargo-install`, and `dtolnay/rust-toolchain` are no longer
advertised. The regression test parses the compiled manifest source and fails if either list drifts.

The pinned `jdx/mr-boxington-action@adc5c234c02592f7edd008bf81d5bc0e9584dc03` bundle was checked:
its backend parser accepts only `github` and `server`. The fixture now uses `backend: github`.
The runner manifest restores that action as an explicit `ActionAdapter::JavaScript` capability,
with the closed input surface and runtime validation. The ordered planner now sends that class to
the existing JavaScript sidecar executor; it is not mislabeled native or silently admitted without
an executable arm. This landed in `0bcc304`.

### T-003 test isolation — changed where the race was enabled

Cache and container tests now use UUID-qualified temporary roots and no cache test mutates process
environment. The executor tests remain untouched as required. Their remaining hazards are
`docker_lifecycle_guard()`'s `/tmp/velnor` fallback at `executor.rs:172-176` and the shared-shape
lease paths in `executor.rs:15170-15254` (`/tmp/vlc-abort-*` and `/tmp/vlc-defer-*`). They need
the same injected per-test root/lease fixture when that forbidden file is available.

### T-004 documentation — pool trust is the boundary

The getting-started example uses `VELNOR_TRUST_SCOPE=untrusted`; README and execution guidance use
Mr Boxington 1.7.0. The execution guide now describes daemon-pool trust, no per-job trust
classification, unavailable `head_repository_id`, and the fail-closed/ephemeral fallback for
missing repository identity.

### T-005 verifier — identity made content-derived

The fixture audit now binds the capability document by a canonical-content SHA-256 identity rather
than rejecting every unrelated runner commit. The runner export still requires a valid full source
SHA for provenance; no readiness check was skipped or weakened. After the final runner manifest
export, `just refresh-capability-baseline` is the only intended baseline update.

### Exact patches blocked by the file boundary

These are implementation instructions, not applied changes:

1. `crates/velnor-runner/src/executor.rs:9400-9408`: retain the existing temp/run-key validation,
   then return `StoreCatalog::for_job_temp(temp).artifacts_run(&run_key)`. The two callers at
   `executor.rs:7099` and `:7327` then share the catalog's artifact root with GC. Remove the
   temporary `store_catalog.rs` exemption after the call site lands.

2. `crates/velnor-runner/src/runner.rs:2588-2646`: replace the `df`/`free_space_bytes` probe
   and best-effort reclaim with `HostCapacity::probe` over every writable root, a total
   `DiskPressure` transition, and bounded `RefuseUntil`. A failed capacity probe must be unhealthy,
   not treated as available capacity; reclaim must use the same layout snapshot and lease/claim
   set as normal GC.

3. `runner.rs:2751-2760`: remove the unbounded 60-second retry loop. Dispatch the transition's
   bounded `RefuseUntil` deadline, then `Drain` and finally `Deregister`; wake the wait on the
   existing drain signal so an operator can terminate the state machine promptly.

4. `runner.rs:5683-5695`: construct lease scopes from `StoreCatalog` and publish every mounted
   emergency-managed class before execution, including the missing `mise/rustup`, `mbx`, and
   `sccache` scopes. Decide and test explicit policy for `artifacts` and `gha-cache`, whose writers
   still bypass the catalog; they must either acquire matching leases or be removed from the
   reclaimable set. Do not let a lease-free class look live merely because its directory is fresh.

5. `runner.rs:4786-4833` and `:5330-5338`: use the same `HostCapacity`/`DiskPressure` snapshot for
   post-acquire reservation and place capacity admission before `acquire_job` where protocol
   semantics allow. The current path still calls `free_space_bytes`, then reserves after acquire.

6. `executor.rs:172-176`, `:15170-15254`: inject the job/test run root into the Docker lifecycle
   guard and cancellation cleanup fixtures. Do not use the ambient `/tmp/velnor` fallback or
   shared `/tmp/vlc-*` names in parallel tests.

### Evidence at handoff

Runner `839c950` passes the serialized library suite (`1524 passed, 1 ignored`) and tools suite;
workspace all-target check with `velnor-runner/test-support` also passes. The final immutable
archive check passes `cargo fmt --check`, workspace clippy, and the workspace check (337 crates).
BuildKit emergency reclaim remains deliberately disabled because no BuildKit lease spans its mutable
content; `prune_owned_builder` is reserved for a future caller that proves that lease.

## 19. Post-handoff concurrent follow-up

After §18 was pushed, a concurrent worker advanced `perf/docker-rust-mbx` from
`b7635a9` to `839c950`. The following commits changed paths that the
continuation boundary explicitly reserved:

- `506847b` applied the catalog constructor at
  `executor.rs:9400-9408` and removed the named catalog-test exemption.
- `575097a` changed the cancellation registry and termination fan-out in
  `execution/cancel.rs`.
- `d500aab` reordered modules in `execution/mod.rs` as required by
  rustfmt.

Those changes were not authored or applied by this worktree; §18 remains the
exact patch report for the forbidden-file work requested from this lane. The
same follow-up also fixed the four pre-existing `docker_lease.rs` clippy
findings (`d285d0f`) and the allowed-file tools formatting issue
(`839c950`). Final immutable-archive evidence for `839c950` is:
`cargo fmt --check` pass, workspace check of 337 crates pass, clippy pass,
and the serial runner library suite `1524 passed, 1 ignored`.

### T-019 follow-up — admission and planning now share one dispatch class

`01229a8` makes remote admission recurse only when the manifest declares
`ActionAdapter::Composite`. JavaScript and Docker actions are admitted as leaf nodes after their
fetched runtime is cross-checked against the manifest, matching the planner's dispatch boundary.
The regression test covers the pinned `jdx/mr-boxington-action` Node24 action as a leaf. At the
merged `839c950` tip, the serialized runner library suite passed 1,524 tests with one ignored.

### Acquire `409` — no remote winner identity exists

The pinned upstream runner contract (`actions/runner` `397b032`) sends only `jobMessageId`,
`runnerOS`, and `billingOwnerId` in `AcquireJobRequest`; `RunServiceError` carries only `source`,
`statusCode`, and `errorMessage`. The upstream `409` path reports an already-acquired job and
does not identify the runner that won it. Velnor's provisional-marker design therefore cannot
use a remote winner identity: `409` means unavailable/owned elsewhere, while local durable
claims, leases, and generation fencing remain the ownership proof.

## 20. Final verification — five-task handoff

This is the final evidence section. It supersedes older counts and the artifact-callsite
blocked status above. The runner tip verified here is
`f4ccf9c3867cfd386052080d4d36a0b598ebb37e`; the verifier tip is
`705a77b9a86b48981646ada461ae2d2df2d0f0fd`. Every runner build/test below came from
`git archive <SHA> | tar -x` of the stated immutable commit.

### T-001 capability surface — executable and manifest-derived

The tools audit now derives the checkout surface from the manifest and asserts the exact
supported set: `clean`, `fetch-tags`, `fetch-depth`, `lfs`, `path`, `persist-credentials`,
`ref`, `repository`, and `token`. `submodules` is absent and remains rejected by admission.
The target inventory no longer advertises capabilities absent from the manifest; the regression
test parses the compiled manifest source and fails on either direction of drift. The relevant
surface work is in `7ca8b17`, `0e3b6ea`, and `637ae99`.

The pinned `jdx/mr-boxington-action@adc5c234c02592f7edd008bf81d5bc0e9584dc03` bundle accepts
only `github` and `server`. The manifest and admission path now use explicit
`ActionAdapter::JavaScript`, validate the closed input/runtime surface, and route the capability
to the existing JavaScript executor (`0bcc304`, `01229a8`). The fixture uses `backend: github`,
not the unsupported `local` value.

### T-002 storage — merged with explicit BuildKit safety decision

`2036c30`, `5af89ef`, `c881402`, and `371d3c1` preserve both sides of the cache/storage merge:
`StoreCatalog` is the path-construction authority, `HostCapacity` combines `statvfs` with Docker
accounting, leases and claims participate in liveness, and disk pressure exposes bounded refusal
transitions. `7b0bfd7` makes claim-directory iteration fail closed: an unreadable directory entry
marks evidence incomplete, so the reaper deletes nothing.

Emergency reclaim deliberately does not run BuildKit prune. No BuildKit lease spans mutable
content, so `buildx prune --max-used-space 0B` could delete live state. `prune_owned_builder`
remains available only to a future caller that proves that lease; no broad Docker/system/volume/
builder prune was added.

The artifact-store constructor now calls
`StoreCatalog::for_job_temp(temp).artifacts_run(&run_key)` at `executor.rs:9492-9498`
(`506847b`). This follow-up landed on the shared remote from another concurrent lead after the
earlier boundary report; this turn did not edit the forbidden executor file. The remaining
forbidden-boundary patch instructions are unchanged: move disk-pressure admission and reclaim to
the shared `HostCapacity`/`DiskPressure` snapshot before acquisition
(`runner.rs:2588-2646, 2751-2760`), publish the missing `artifacts`/`gha-cache`/`mbx`/`sccache`
policy and leases (`runner.rs:5683-5705`), and use that same snapshot for reservation/acquire
ordering (`runner.rs:4786-4833, 5330-5338`).

### T-003 test isolation — race enabling conditions removed where allowed

`13b2bc5` gives container tests UUID-qualified roots. Cache/container tests no longer mutate
process environment or assert against shared fixed paths. The forbidden executor test hazards
remain at `executor.rs:172-176` (`/tmp/velnor` fallback) and
`executor.rs:15310-15350, 15394-15434` (`/tmp/vlc-abort-*` and `/tmp/vlc-defer-*`); inject a
per-test run/lease root there when that boundary is available. No executor test edit was made by
this turn.

Focused comparison evidence: container tests were `45 passed` on `7b0bfd7` versus `44 passed`
on the unmodified storage baseline `c881402`; cache tests were `34 passed` on both, with eight
test threads. The final runner snapshot passes the serial library gate with `1576 passed, 1
ignored`. Full parallel runner suites at both comparison snapshots stalled in the same controller/
wiremock harness after about seven minutes and were stopped; that is a harness limitation, not a
claimed pass or a code failure. Serial execution is the accepted gate for this repository.

### T-004 documentation — synchronized with runtime trust boundaries

`fb8ae38` changes the getting-started trust example to `VELNOR_TRUST_SCOPE=untrusted`, updates
Mr Boxington references to 1.7.0, and describes daemon-pool trust rather than a nonexistent
per-job trust class. The execution guide records unavailable `head_repository_id` and the
fail-closed/ephemeral fallback for missing repository identity.

### T-005 verifier — content identity plus source provenance

`ee91532` makes `capability_id` the SHA-256 of canonical capability content, excluding only
`source_sha` and `capability_id`; `source_sha` remains a required full lowercase commit SHA for
provenance. Baseline and fixture coverage therefore remain bound to content while still proving
which runner produced the export. The final immutable refresh from runner
`f4ccf9c3867cfd386052080d4d36a0b598ebb37e` reports manifest version `12`, crate version
`0.1.250`, capability ID
`23749db8aab50310a27021ac24ef7dff7b8480468fd26f800d4b0018b4732229`, and leaves the fixture
clean. Readiness passed against that archive; a baseline-only audit still fails closed when no
runner source/export is supplied.

### Final gates

| Gate | Immutable source | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | runner `f4ccf9c` | pass |
| `cargo check --workspace --all-targets --features velnor-runner/test-support --locked` | runner `f4ccf9c` | pass; 3 crates compiled |
| `cargo clippy --workspace --all-targets --features velnor-runner/test-support --locked -- -D warnings` | runner `f4ccf9c` | pass; `cargo clippy: No issues found` |
| `cargo test -p velnor-runner --lib --features test-support --locked -- --test-threads=1` | runner `f4ccf9c` | pass; 1576 passed, 1 ignored (40.15s) |
| `cargo test -p velnor-tools --locked -- --test-threads=1` | runner `f4ccf9c` | pass; 187 passed (4.89s) |
| `just refresh-capability-baseline` + readiness audit | runner archive `f4ccf9c`, fixture `705a77b` | pass; v12 / ID `23749db8…` |
| `python3 .github/scripts/test_audits.py` | fixture `705a77b` | pass; 38 tests |
| `just capability-contract` | fixture `705a77b` | pass |
| `just python-check` | fixture `705a77b` | pass |

### Shared-tree boundary note

The final shared runner tip also contains concurrent-lead commits
`7ed4883`, `cade1df`, `7c32055`, `111ed1d`, `9149e67`, `d8fcfb2`, `f4ccf9c`, `506847b`, `575097a`, `d285d0f`, and `d500aab` touching files that this turn was instructed not
to edit (`executor.rs` and `execution/**`, plus related runner integration). They were already
on the shared remote and were not authored or reverted here. The exact remaining forbidden-file
patches are recorded above and in §18; this report does not misstate the final tree as
boundary-clean.

## 21. Resolved follow-up — provisional acquire oracle and timeout premise

The completion-durability package specified a provisional marker to close the lost-completion
window between `acquirejob` returning 200 (`runner.rs:5145`) and the durable marker
(`runner.rs:5328`), and correctly refused to build it until one upstream fact was verified:
**does the 409 reply name the acquiring runner?** It does not.

Read from `actions/runner` v2.337.0, `src/Sdk/RSWebApi/RunServiceHttpClient.cs:88-120`. The
409 path is:

```csharp
if (TryParseErrorBody(result.ErrorBody, out RunServiceError error))
{
    switch ((HttpStatusCode)error.Code)
    {
        case HttpStatusCode.Conflict:
            throw new TaskOrchestrationJobAlreadyAcquiredException(
                $"Job message already acquired '{messageId}'. {error.Message}");
```

`RunServiceError` carries only `source`, `statusCode` and `errorMessage` — the same envelope
whose wire names BC-1 was about. `error.Message` is free-text with no structured runner
identity. So a 409 tells us the message was acquired by *someone*, and cannot distinguish "we
acquired it and then crashed" from "another runner acquired it". As an ownership oracle it is
useless, and the design that depended on it must not be built.

**The oracle is `renewjob`.** `RunServiceHttpClient.cs:184-218` posts `{planId, jobId}` and
either succeeds — which only the lease holder can do — or raises
`TaskOrchestrationJobNotFoundException` on a 404. That is exactly the discrimination the 409
could not provide, keyed on the identifiers a provisional marker already records.

Corrected design:

1. Before `acquirejob`, journal `JobAcquisitionIntended{slot_id, generation, plan_id, job_id,
   message_id}`, creating the job row in a **provisional** state that occupies the slot for
   admission but is not an ownership proof — `outbox_owner_is_proven` must keep rejecting it,
   so no completion can ever be sent against a provisional row.
2. Issue `acquirejob`. A 200 promotes the row to owned. A 404 or 422 drops it and frees the
   slot.
3. On a 409, **or** on recovery after a crash with a provisional row present, call `renewjob`
   for the recorded `(plan_id, job_id)`. Success promotes the row — we own the job and crashed
   after a previous attempt's 200. A 404 drops it.
4. Bound the whole thing with the durable attempt counter and deadline the completion package
   added, so a provisional row also reaches a terminal state instead of pinning a slot.

With a provisional row always present before the call, there is no state in which a terminal
completion has no journal row, so deleting the unjournaled `Acquired` branch becomes a pure
deletion rather than a behaviour change — which was the other half of that package's blocked
work.

One caveat to carry into the implementation: `renewjob` extends the lease as a side effect, so
the recovery probe is not free of consequence. On the "not ours" path the 404 arrives before
any extension, and on the "ours" path extending the lease is what we want anyway — but a probe
loop must still be bounded, or a job we own but cannot execute would have its lease renewed
indefinitely.

### Also settled: job `timeout-minutes` is not a Velnor wall clock

Recorded as a P0 by the cancellation audit ("a 10-minute job with 20 steps can run 200
minutes"). Checked before implementing: there is **no job-level timeout field in the job
message**, because upstream does not enforce it in the runner either. GitHub enforces
`timeout-minutes` server-side and sends an ordinary `JobCancellation` when it elapses.

What upstream's runner *does* implement is the listener-side escalation backstop,
`max(cancel_timeout, 60s) − 15s` (`JobDispatcher.cs:1280-1285`) — and Velnor already has it,
in `forced_kill_delay`, with a test citing those lines. Building a local wall clock would have
introduced a divergence rather than closing one.

Three items on the open list turned out to be inherited from audit claims rather than from
source. The rule that caught all three is the same one: verify before implementing.

## 22. Concurrent hardening — stale fencing and action-tree isolation

The shared branch now contains these bounded follow-ups from the post-handoff audit:

- `111ed1d` hardens the durable journal boundary. `SlotStale` is rejected when any job on the
  slot is in `Assigned`, `Starting`, `Running`, or `Completing`; the reducer leaves state and
  commands unchanged. Idle same-generation slots still fence. The test covers all four occupied
  phases and the persisted journal path.
- `a1e64dd` closes the failed-acquire capacity leak. `JobAcquisitionLost` now removes only the
  provisional row, restores the exact same-generation slot through its registration/readiness
  proof, advertises capacity when it is healthy, and permits the next acquire immediately.
- `d8fcfb2` mounts the fetched action tree at `/__a:ro` in the job, Node-action, and Docker-action
  containers. Workspace, temp, and tool mounts remain writable. Container tests assert both the
  read-only mount and absence of the old writable form.
- `b665fae` makes `ReadyProof` an opaque model capability. Its deserializer routes every wire
  value through `try_new`, so an all-false or partially false serialized proof cannot satisfy a
  readiness check; the existing JSON shape remains unchanged.
- `ac85ea1` adds the microVM guest-cancel hook. `8725209` then orders hooks before process and
  container termination regardless of registration order, so the guest gets its graceful stop
  request before its jailer can be killed; focused cancellation tests pass.
- `0a23bbc` closes the opposite cancellation failure direction: an unknown Docker inspect error
  keeps the container in the escalation ladder instead of falsely treating an unproven daemon as
  gone.
- `931ac8a` wires recovery of a durable provisional acquire through the `renewjob` ownership
  oracle. It is integrated, but its transport and controller call sites remain part of the other
  lead's runner boundary.
- `3f14859` rejects zero-generation continuation tokens at the query boundary, so callers cannot
  forge a token that bypasses generation validation.
- `185ca1e` counts Unicode characters, rather than UTF-8 bytes, for telemetry identity limits;
  the model now matches the JSON Schema `maxLength` contract.
- `e34d05a` rejects benchmark records whose declared driver contradicts their runnability, so an
  unrunnable scenario cannot report measurements.
- `d671dc8` routes fixture cleanup through the surviving `velnorctl` operator CLI. `2c2b40e` then
  removes raw PATs from cleanup and daemon subprocess argv; `ad4ab1b` makes `velnorctl` read the
  token only from `GITHUB_TOKEN`.
- `7e8e257` rejects an explicit `--instance` that conflicts with the selected context, while
  `6194e14` rejects the currently unsupported global `--repo` selector instead of ignoring it.
- `0c3bfda` allows cancellation only for known nonterminal GitHub run statuses; unknown statuses
  fail closed without issuing a cancel request.
- `738007b` aligns the dependency-boundary test with the intentional shared-model leaf used by
  `velnor-tools`; service, transport, and renderer edges remain forbidden. `1990182` rejects
  ignored global selectors on `remove`, and `f579e6d` propagates cleanup subprocess failures so
  remote runner registrations cannot be left behind while smoke reports success.
- `dfa72c8` removes deleted-crate references from the development, security, and interface docs;
  the interface now documents environment-only `GITHUB_TOKEN` registration.
- `d7d10cf`, `372bf6a`, and `f294929` remove the unreachable action-journal, action-model, and
  CAS crates. The later journal/CAS durability findings therefore do not apply to this tree; no
  remaining root publisher owns that abandoned API.

A full fixture gate passed at source `62deda7fc641265123b37479309d4f59bc86afa5`, with
the fixture branch at commit `580f74472ee3dc137304c893a001d5c5409ddfc0` and its generated export
from `2b61ac5f3d4a9d1a4bed0c3c9c00fd0a8d6bd762`. The cleanup, dependency-boundary, and
documentation commits do not alter the capability manifest; the refresh readiness audit and the
full gate both passed after the final provenance update.

These changes do not claim the remaining cancellation gaps are solved. Node sidecar cancellation
and mixed native/JavaScript post-action ordering remain in the other lead's reserved `runner.rs`,
`executor.rs`, and `execution/**` worktrees.

## 23. Final immutable revalidation — five-task handoff

This is the newest handoff record. It supersedes stale current-tip counts in §§20 and 22 while
leaving those sections as history. The tested runner source snapshot is
`6c0c1af6db984e53fa33608a82927cebee1553a4`; it was materialized with
`git archive 6c0c1af6db984e53fa33608a82927cebee1553a4 | tar -x -C <scratch>`.
The verifier's current provenance commit is
`75b19e581f7d80b1166e347f199e14f083a97e65`, with the generated baseline present in its
parent `ed834d044a4c597e99b1575c93ea9b59d2fd5ae5`. Manifest identity is v12, crate `0.1.250`,
capability ID `23749db8aab50310a27021ac24ef7dff7b8480468fd26f800d4b0018b4732229`, and source
SHA `6c0c1af6db984e53fa33608a82927cebee1553a4`. No live dual-lane verdict or deployed image
digest is claimed.

### T-001 capability surface

The immutable manifest-derived checkout set is exactly `clean`, `fetch-tags`, `fetch-depth`,
`lfs`, `path`, `persist-credentials`, `ref`, `repository`, and `token`. `submodules` is absent
and rejected. Target expected actions no longer include unsupported manifest entries. Mr Boxington
accepts only `github` and `server`; the fixture uses `backend: github`. `7ca8b17`, `0e3b6ea`,
`637ae99`, and `0bcc304` provide the implementation and regression proof.

### T-002 storage and admission

`StoreCatalog`, `HostCapacity`, leases/claims/reaper, bounded `DiskPressure`, and fail-closed
claim iteration and portable Unix `statvfs` accounting are present. BuildKit prune is deliberately disabled: no lease spans mutable
BuildKit content, so broad prune could delete live state. The artifact-store constructor follow-up
is present on the shared tip but came from the concurrent lead. The remaining disk admission,
reclaim, lease-publication, and acquire-order changes are still reserved by the file boundary;
exact patch instructions remain in §18. This handoff does not claim those forbidden call sites
are boundary-clean.

### T-003 test isolation

Allowed cache/container tests use UUID-qualified roots and do not mutate process environment.
The executor fallback `/tmp/velnor` and `/tmp/vlc-*` fixture paths remain reserved; §18 gives the
injection patch. Full parallel runner execution is not claimed: the known controller/wiremock
harness stalled at both comparison snapshots. Serial execution is the accepted repository gate.

### T-004 documentation

Trust defaults to `VELNOR_TRUST_SCOPE=untrusted`; Mr Boxington is documented at 1.7.0; trust is
daemon-pool scoped, not per-job; unavailable `head_repository_id` is documented with fail-closed
and ephemeral-identity behavior. This is the `fb8ae38` documentation package.

### T-005 verifier provenance

`capability_id` is SHA-256 over canonical capability content excluding only `source_sha` and
`capability_id`; the full lowercase source SHA remains mandatory provenance. The fixture refresh
and source-bound readiness audit pass against the 6c0c1af archive. Baseline-only and single-lane
diagnostics remain fail-closed; no live parity result is substituted.

### Final gates

| Gate | Result |
| --- | --- |
| Runner `cargo fmt --all -- --check` | pass |
| Runner workspace check, all targets, `velnor-runner/test-support`, locked | pass |
| Runner workspace Clippy, warnings denied, locked | pass |
| Runner dependency-boundary test, serial | 6 passed |
| Runner tools test, serial | 190 passed |
| Runner library test, serial | 1583 passed, 1 ignored |
| Fixture refresh plus capability readiness audit | pass; v12 / crate 0.1.250 / ID above |
| Fixture capability contract, workflow audit, Python syntax, Python tests | pass; 39 Python tests |
| Fixture `mise run workflow-check` | pass; actionlint and audit suite included |
| Fixture L2 closure | pass; `closure-valid=3` |
| Fixture Rust fmt/check/Clippy | pass |
| Fixture Rust nextest, one test thread | 49 passed; 11 binaries |

### Reserved-file boundary

This turn did not author edits to `crates/velnor-runner/src/executor.rs`,
`crates/velnor-runner/src/execution/**`, or the reserved cancellation/storage/admission/lease
integration call sites. Concurrent shared-branch commits touch some of those paths and remain in
the tested snapshot; they are identified in §§18, 20, and 22. The allowed configless acquisition
regression fix is `e7bcac5`. No forbidden patch was silently presented as completed.

## 24. Continuation anchor — 2026-09-04

The exact requested branch heads were re-resolved before this continuation. The live
Velnor head is `915cc15057665e461363f9cf87a6bcf27d3ddeea` on
`origin/perf/docker-rust-mbx`; the live fixture head is
`0cbec6e3265c59e714a31df5cdd67c7d99462443` on
`origin/codex/verifier-completion-fixes`. These are newer than the immutable starting
SHAs in §0 and remain the same requested branches.

The continuation has pushed these focused benchmark hardening packages to the Velnor
branch: `d7c061a` makes image identity probing read-only and refreshes the digest after a
successful workload; `9cc2c3f` accounts for concurrent Cargo worker processes through
isolated runners; and `915cc15` records per-child Linux/macOS wait-time resources so
per-iteration RSS is not inherited from the process-wide high-water mark. The preceding
journal, checkout, Docker status, cache, and identity packages remain in the history.

Evidence at this anchor: Velnor fmt, workspace check, workspace Clippy with warnings
denied, and workspace nextest pass (`2366` passed, `1` skipped); the fixture `mise run
check` passes against this exact source and includes workflow/capability audits, actionlint,
`41` Python tests, `49` Rust tests, fmt, check, and L2 closure. No live dual-lane result,
deployed image digest, default-mbx end-to-end run, or soak result is claimed.

The goal remains open. Highest unresolved classes include the reserved runner/control
completion and acquisition recovery paths, unavailable VelnorJob benchmark driver,
unsupported production reads, true network-cold Docker proof, and the broad semantic,
cancellation, fault, soak, and architecture decomposition work in §§15, 18, and 58.

## 25. Continuation findings — current anchor `aa913f3`

`aa913f3` serializes mutable writes to each shared bare Git mirror. Health probing is
shared-lock read-only work; mirror repair, ref publication, fetch, pinning, and requested
SHA resolution now execute under one exclusive per-mirror lease. A barrier-started regression
proves two different cold requests resolve their own commits and never overlap fetch writers.
The workspace hydration phase remains outside that lease.

The benchmark follow-ups are also on this branch: `d7c061a` removes image pulls from
identity probing, `9cc2c3f` merges concurrent Cargo worker invocations into the sample
census, and `915cc15` replaces process-wide RSS snapshots with per-child Linux/macOS
`wait4` accounting. Full source and fixture gates remain green at the prior anchor; the
mirror package has passed its focused tests and runner Clippy but still requires the next
full workspace gate after this commit.

Fresh independent audits found these unresolved requirements, not accepted as complete:

* P0: recovery classifies a provisional acquisition as accepted and can leave the slot
  occupied without a bounded `renewjob` ownership probe or terminal outcome. This crosses
  the reserved runner/controller boundary and requires coordination with the other lead.
* P1: mixed native/JavaScript post steps are stored and executed in separate lists rather
  than one upstream LIFO stack; the implementation boundary is reserved `executor.rs`.
* P1: the remaining live GitHub dual-lane provenance, deployed image identity, default-mbx
  Rust run, fault suite, soak suite, and official-runner comparison are absent. Existing
  local fixture gates prove schemas and mutation resistance, not live parity.
* P1: control/mutation and acquisition recovery have additional fail-open/indeterminate
  paths; no benchmark or source gate is treated as evidence that those paths are fixed.

The branch is intentionally not declared complete. Next packages must preserve the shared
ownership table, use explicit pathspec commits, and obtain independent review before
touching reserved lifecycle or semantic code.

## 26. Continuation anchor — mirror lease correction `d337304`

`d337304` corrects the mirror package's reader lifetime. A successful checkout now retains a
shared lease across mirror-backed hydration, and the repair path downgrades its exclusive
lease only after ref publication, fetch, pinning, and requested-SHA resolution complete. A
regression proves an active checkout reader blocks destructive mirror repair and releases the
repair lock after checkout ownership is dropped. The stale shared health probe is rechecked
under exclusive ownership before quarantine, so concurrent cold callers cannot quarantine a
mirror that another caller just repaired.

Independent post-correction reviews found no blocking correctness issue. Remaining medium
proof/performance work is explicit: measure or test warm same-SHA N-way reads, different-repo
concurrency, mutable-ref pinning, and end-to-end repair contention; release the reader lease
as soon as hydration no longer needs the mirror if checkout semantics permit. These are
bounded follow-ups, not evidence that the whole rearchitecture is complete.

The exact Velnor head at this anchor is `d3373048ce80e65a6254cfe64684c803f8a5bcf9`; the
fixture remains `0cbec6e3265c59e714a31df5cdd67c7d99462443`. Full workspace and fixture gates
must be rerun after this correction. The reserved provisional-recovery and mixed-post-order
findings in §25 remain open and owned by the other lead.

## 27. Workflow generation migration — initial package `54782fd`

T-020 now owns the Velnor-hosted workflow generator in `crates/velnor-workflow`.
The package contains the static scanner/renderer and a standalone
`velnor-workflow` binary. Generated workflow jobs invoke that binary directly;
the generated `.github/ci/project.toml` is the strict runtime contract for
affected-unit planning, layered execution, policy, and release checks. The
former generated `run.sh`, `policy.sh`, and `release.sh` helpers are no longer
emitted.

The ownership sidecar path remains `.github/ci/.github-actions-generator-state`
for compatibility with repositories produced by the former standalone tool.
The existing Velnor owner-routed workflows remain authoritative; the stale
generic artifact under the generator repository is not promoted over them.

Focused proof at this package: `velnor-workflow` tests `87` passed, all-targets
check passed, and the package was pushed to `perf/docker-rust-mbx`. Packaging,
monitoring, and the full workspace gate remain follow-up work in this same
ownership claim.

## 28. Workflow runtime distribution and evidence — package pending

The Velnor node image and Debian release package now include
`velnor-workflow`. Release archives carry the executable beside the runner;
release verification checks its presence and the Debian payload digest. The
workspace dependency-boundary inventory includes the new package.

`velnor-tools workflow-monitor` now polls the authoritative GitHub run through
`gh api`, optionally snapshots the selected Velnor instance through the typed
`velnor-client` read transport, and atomically rewrites bounded JSON evidence.
Timeout, local transport failure, and GitHub terminal failure remain distinct;
local success never substitutes for GitHub completion. The monitor is
read-only and credentials stay in the existing environment/runner transport.

Focused proof: `velnor-tools` tests `192` passed, strict package Clippy passed,
and dependency-boundary tests `6` passed. Full workspace gates remain required.

## 29. Continuation anchor — shared tip `118437e`

This continuation synchronized the parallel branch without rewriting either history.
`f7238fe` releases the per-mirror reader lease immediately after workspace hydration;
`e9e4113` proves moving branch refs are refetched and repinned; `3550690` validates every
mirror ref tip before accepting the object store; and `6d6b809` excludes Cargo setup,
optional priming, and restoration from measured timing, process/resource, disk, and Git
observations. The workspace package allowlist and strict Clippy contracts were updated in
`5a9481a`, `c37c64f`, and `010af2c`. Merge commits `d2d4a2b` and `118437e` preserve the
other lead's workflow generator, packaging, monitor, and typed-client work.

Final local proof at this anchor:

| Gate | Result |
| --- | --- |
| Source fmt | pass |
| Source workspace check, all targets, locked | pass |
| Source workspace Clippy, warnings denied, locked | pass |
| Source workspace nextest | 2460 passed, 1 skipped; 41 binaries |
| Fixture `mise run check` against this source | pass; 41 Python tests, 49 Rust tests, workflow/capability/L2 audits |

The hydration error-propagation patch was deliberately not landed: it exposed a fake
mirror path in a reserved executor test. Propagating those errors safely requires a
coordinated update at that reserved boundary; the uncommitted patch was reverted and the
tree is clean. No live dual-lane run, deployed image digest, default-mbx end-to-end proof,
VelnorJob benchmark, Docker daemon-resource accounting, fault/soak result, provisional
acquisition recovery fix, or mixed native/JavaScript post-order fix is claimed. Those
remain open under the ownership and evidence constraints recorded in §§15, 18, 25, and 28.

## 30. Continuation anchor — workflow monitor sync `68d4cff`

After §29, the shared branch received the other lead's reqwest-backed workflow monitor
implementation and packaging follow-ups. The synchronization commits preserved those
changes while adding `5a22ba9` (reject a successful no-op `cargo update`), `010af2c` and
`69819df` (strict workflow-runtime Clippy cleanup), and the required merge ancestry. The
workspace package boundary now names `velnor-workflow`; no duplicate package entry remains.

Final proof at `68d4cff`:

| Gate | Result |
| --- | --- |
| Source fmt | pass |
| Source workspace check, all targets, locked | pass |
| Source workspace Clippy, warnings denied, locked | pass |
| Source workspace nextest | 2461 passed, 1 skipped; 41 binaries |
| Fixture `mise run check` against source | pass; 41 Python tests, 49 Rust tests, workflow/capability/L2 audits |

The branch remains open. Fixture audits still find static-only or vacuous default-mbx,
sccache, opt-out, image-digest, and provenance assertions; compatibility gate predicates
have event/skip cases needing explicit tests; and no live dual-lane run has been triggered.
Source audits still identify Docker CLI-only resource accounting and the unreachable
VelnorJob/Rust/MBX authoritative benchmark path. Reserved provisional-acquisition recovery
and mixed native/JavaScript post-order work remain with the other lead. Checkout traversal
error propagation remains deferred because it requires a coordinated change to a reserved
executor test boundary.

## 31. Workflow migration completion evidence — runtime and policy hardening

T-020 closes the previously open workflow-runtime follow-ups in the migration slice.
GitHub-hosted generated jobs bootstrap the pinned Velnor commit
`a2eecb6bededb3ef6c92ef2a921bec436d167256` with `cargo install`; Velnor lanes use the
packaged `velnor-workflow` binary from the node image/release archive. Preview artifacts
accept the explicit `preview` version, and Velnor release/preview jobs gate manual dispatch
to the default branch.

The policy runtime parses YAML structurally, ignores comments and shell text, rejects
forbidden triggers/external reusable workflows/unpinned actions, and fails closed for
unresolved dynamic runner labels. The generated policy job checks both the candidate
checkout and the trusted base checkout. `velnor-tools workflow-monitor` uses bounded
`reqwest` GitHub API polling with `GITHUB_TOKEN`/`GH_TOKEN` environment authentication,
plus optional typed-client Velnor observations and atomic evidence writes.

Focused proof: workflow-runtime tests `93` passed; tools tests `192` passed; strict
package Clippy and locked all-target checks passed. Full workspace gates and live
dual-lane execution remain final verification work, not claimed here.

## 32. Continuation anchor — graph and evidence hardening `ce4bb8b`

The shared source branch now includes `533fe67`, which normalizes Cargo's
`dependency.workspace = true` keys before affected-unit graph expansion and adds
coverage that preserves aliases and path dependencies. The parallel lead's
`ef344c9`, `31e8e02`, and `e204e5f` changes remain preserved through merge
`ce4bb8b`: trusted self-hosted jobs require an explicit gate even when the
configuration supports both runner classes, the hosted bootstrap pin is checked,
and Velnor jobs target the intended runner label.

Final proof at this anchor:

| Gate | Result |
| --- | --- |
| Source fmt | pass |
| Source workspace check, all targets, locked | pass |
| Source workspace Clippy, warnings denied, locked | pass |
| Source workspace nextest | 2468 passed, 1 skipped; 41 binaries |
| Fixture `mise run check` against source `ce4bb8b` | pass; 41 Python tests, 51 Rust tests, workflow/capability/L2 audits |

The fixture branch `codex/verifier-completion-fixes` is at `c97ec0e`: duplicate
lane names are rejected, the L2 verifier expects its actual `produce` job, and
JSON-loaded provenance enforces the same lane/environment binding as collection.
The fixture gate now passes against `ce4bb8b`; this is static/fixture proof, not a
live dual-lane run. No deployed image digest,
default-mbx end-to-end proof, VelnorJob benchmark, Docker daemon-resource
accounting, fault/soak result, provisional recovery fix, or mixed post-order fix
is claimed. Reserved runner/executor ownership and the coordinated checkout
error-propagation follow-up remain open.

## 33. Continuation anchor — trust and fixture provenance hardening `b8b95fe`

The benchmark now refuses to treat an arbitrary checkout as the Actions fixture:
`3ffa772` validates the origin remote against the canonical
`tailrocks/velnor-actions-fixture` repository, and `1c78b3f` tightens the probe
regressions. The fixture branch `codex/verifier-completion-fixes` is at `a87bcbf`.
Its incremental hardening rejects duplicate lanes (`4174d97`), binds L2
provenance to the actual `produce` job (`963d846`), validates lane/environment
and numeric/SHA provenance while loading evidence (`c97ec0e`, `cbf0767`), and
runs fork pull requests GitHub-only (`4268a84`, `a87bcbf`).

The parallel source history through `b8b95fe` is preserved without rewriting:
immutable workflow policy, base-owned pull-request admission, pinned policy
runtime, hosted bootstrap, manual-dispatch runner routing, and Velnor job-image
provisioning are all present. The source branch remains shared with the other
lead; merge commits retain each remote advancement.

Final proof at this anchor:

| Gate | Result |
| --- | --- |
| Source fmt | pass |
| Source workspace check, all targets, locked | pass |
| Source workspace Clippy, warnings denied, locked | pass |
| Source workspace nextest | 2477 passed, 1 skipped; 41 binaries |
| Fixture `mise run check` against source `b8b95fe` | pass; 41 Python tests, 53 Rust tests, workflow/capability/L2 audits |

This is static and fixture-backed proof. No live dual-lane run, deployed image
digest, default-mbx end-to-end proof, reachable VelnorJob benchmark, Docker
daemon-resource accounting, fault/soak result, provisional acquisition recovery,
or mixed native/JavaScript post-order fix is claimed. Reserved runner/executor
ownership and checkout traversal error propagation still require coordination;
the fixture's remaining static-only defaults and no-live-run limitations remain
explicit rather than promoted to readiness.

## 34. Workflow migration closure evidence — Velnor-owned runtime

The workflow-generation migration is implemented in the Velnor workspace at the
`perf/docker-rust-mbx` branch. `crates/velnor-workflow` is the sole Rust
generator/runtime package; the legacy `github-actions-unified` repository is now
a compatibility facade pinned to Velnor commit
`dacf345e641c12ef27ad019a10b8562d497816b1`.

Generated workflows execute `velnor-workflow` directly. GitHub-hosted jobs
bootstrap source revision `a099520c761559d3875f76be25f18a1a63ccbafc`; Velnor
job images and release archives package the binary. Legacy generated Bash
helpers are no longer emitted. The monitor is Velnor-tools-owned, uses typed
GitHub polling, optional local Velnor observations, atomic evidence, bounded
polling/history/error/snapshot/evidence resources, and one global deadline.

Trust boundaries now include: exact action SHAs; exact approved policy reusable
workflow ref `fe805b984d3e261d3686d7ec670792f8121306bc`; a required exact-shape
base-owned `.github/workflows/ci-policy.yml` entrypoint; fork policy checkout
sparse-isolated to workflow data; exact trusted self-hosted gates; and explicit
or static-Git-derived default branches. Pull-request generated workflows keep
policy advisory-only; the base-owned entrypoint is the authoritative Policy
check. Release tag verification requires an explicit `--branch`.

Focused proof after this closure: `velnor-workflow` tests `99` passed,
`velnor-tools` tests `196` passed, source facade tests `1` passed, source
format/check/Clippy passed, and Dockerfile BuildKit syntax checks passed.
Final workspace gates remain the authoritative verification record. No live
dual-lane GitHub run, deployed image digest, or branch-protection mutation is
claimed here. The currently observed Velnor `protect-main` ruleset requires
`DCO` and `ci-required` only; after the workflow entrypoint is materialized on
the protected branch, operators must add its exact `Velnor workflow policy /
Policy` check to the ruleset and verify one test PR before declaring the merge
gate complete. Existing handwritten Velnor workflows remain outside generator
ownership until separately migrated.

## 35. Workflow install command correction — multi-binary workspace

The hosted bootstrap and immutable policy workflow must pass the positional
`velnor-workflow` package to `cargo install`; `--bin` alone is rejected when
the Velnor git workspace contains multiple binary packages. This correction
preserves the existing pinned-source and pinned-policy trust boundaries and
is covered by a direct `cargo install --locked --git ... --rev ...
velnor-workflow --bin velnor-workflow` proof. The corrected immutable policy
workflow is now pinned at `0b68533ea24e8259ebd8aee54e36905debe8fcc3`.

## 36. Final audit corrections — branch propagation and runtime identity

The final audit found three code-owned gaps to close before migration sign-off:
interactive and estate generation must honor an explicit default branch, the
base-owned policy workflow must install a runtime containing exact policy-pin
enforcement, and Docker preflight must compare the workflow binary with
image-baked identity metadata. Release tag verification also requires the tag
commit to equal the protected branch tip, not merely be an ancestor. The live
branch ruleset remains an operator-owned prerequisite because changing it is an
external repository policy mutation.

## 37. Final pin synchronization — hardened hosted runtime `20b9402`

The hosted workflow bootstrap now points at the full Velnor revision
`20b940278fb11c993910aaf201e55a19b802bdf0`, which contains exact policy-pin
enforcement and the privileged-job runner routing fixes. The current
compatibility facade is source commit `d9a409c`; its Cargo lockfile and README
use the same full Velnor revision. The corrected base-owned policy workflow is
pinned separately at `47f06562126e8a3cfa08db7b668a21d60def7f1a` and installs
the hardened runtime.

## 40. Policy pair correction — workflow `12da623`

Section 37's pre-cycle policy pin is historical. The current base-owned
reusable workflow is `12da6232672f039e42c21fe9dff00085856ef92d`; it installs
the generic-input runtime at `8859e3c537cfc2d6e44a92d0c4c0f7ca071e92e0`.
Generated entrypoints and advisory calls pass the workflow's own full SHA as
`policy-revision`, which the installed runtime validates. This removes the
self-referential commit dependency.

## 38. Policy bootstrap cycle removal

The policy workflow revision and the runtime revision cannot safely pin each
other through self-referential commit hashes. The policy contract therefore
passes its caller-selected full policy revision as a required trusted input to
the pinned reusable workflow; the installed runtime compares the entrypoint's
full SHA with that input. The base-owned entrypoint remains the authority, and
the static scanner requires the exact-shape entrypoint plus self-consistent
full-SHA input.

## 39. Monitor deadline contract

The workflow monitor timeout is a cooperative polling and local-observation
budget, bounded additionally by the 2,048-observation cap. It is not a hard
function-return deadline: scheduler delay and evidence serialization, flush,
or atomic rename may extend return time. Terminal classification records the
GitHub response-acquisition instant so a response obtained after the budget is
reported as timed out.

## 41. Benchmark record integrity — T-021 `ab1cbe1`

`BenchRecord::validate` now recomputes `Summaries` from the record's own
observations and rejects mismatched aggregates. It also rejects records below
the summary sample floor with a typed error. This closes a producer-side trust
gap without changing the NDJSON wire shape or touching the reserved runner,
executor, acquisition, or lifecycle boundaries.

Focused source proof at `ab1cbe1269629389849c45db036ba85c0074ba49`:
`velnor-bench` nextest passed (`78` tests), strict package Clippy passed, and
format/diff checks passed. The fixture branch
`codex/verifier-completion-fixes` was refreshed to source `ab1cbe1` at
`e5a2792`; its full `mise run check` passed (`44` Python tests, `53` Rust
tests, workflow/actionlint, capability readiness, format/check, and L2
closure). This remains static/fixture proof: no live dual-lane run, deployed
image identity, default-mbx end-to-end result, fault/soak result, or
VelnorJob lifecycle benchmark is claimed.

## 42. Cargo benchmark environment isolation — T-022 `a92074e`

The benchmark's host Cargo driver now removes ambient compiler-wrapper,
cross-target, flags, and offline-mode overrides before setup, warm-up, metadata,
and measured commands. Its explicit per-sample target directory, terminal
policy, incremental policy, and trace settings remain applied. The shared
process runner exposes this as a narrow `exec_without` operation; existing
execution and accounting behavior is unchanged.

Focused source proof at `a92074e4a10ab2a53e58c30787db1012120a253d`:
`velnor-bench` nextest passed (`80` tests), strict package Clippy passed, and
format/diff checks passed. The fixture baseline was refreshed to this source at
`d29b0bf`; its full gate passed (`44` Python tests, `53` Rust tests,
workflow/actionlint, capability readiness, format/check, and L2 closure).
This remains static/fixture proof: no live dual-lane run, deployed image
identity, default-mbx end-to-end result, VelnorJob lifecycle benchmark,
fault/soak result, or reserved runner/executor change is claimed.
