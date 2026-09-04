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

_Pending wave 1 reports._

## 5. Target architecture

_Pending synthesis._

## 6. Architectural invariants

_Pending synthesis._

## 7. Ownership claims

Claim a boundary here before writing to it. Read-only investigation needs no claim.

| Boundary | Lead | Status |
| --- | --- | --- |
| `rust-toolchain.toml` (toolchain channel pin) | opus-lead | claimed — T-001 |
| `protocol.rs` run-service/broker error contracts | opus-lead | claimed — T-002 |
| `deny.toml` + `mise.toml` deny task | opus-lead | claimed — T-003 |
| `crates/velnor-runner/Cargo.toml` tokio feature set | opus-lead | claimed — T-003 |

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
