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
| `crates/velnor-runner/src/docker_lease.rs` (job Docker request authorization and unlabeled cleanup) | codex-lead | claimed — T-004 |

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

`cargo_target_trust_scope()` reads the daemon environment variable `VELNOR_TRUST_SCOPE`
(`github_adapter.rs:211-213`) and the shipped unit sets it to `trusted`
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
