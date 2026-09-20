# Runtime containment design review: 72dd413c proposal

Date: 2026-09-20
Reviewer: G1 independent, read-only
Disposition: **architecture gate; do not integrate the shell-wrapper diff**

## Scope and source state

The requested `72dd413c` is not present as a Git object in the inspected
checkout. This is a design review of the proposal supplied by the owner
(native `run_units`/checks separated from hostile Docker payloads; Linux cgroup
v2, Windows Job Object, explicit unsupported native macOS) plus the visible
owner worktree. The inspected owner worktree was `codex/github-first-hosted-g1-security-3ae`
at `43ba3b414f245bf3aa9176afce5bf97f2d1e5235`, with an uncommitted
`crates/velnor-workflow/src/s2/runtime.rs` shell-wrapper attempt. The dirty
attempt is not a candidate: Bash `jobs -p` is not a process-tree authority,
does not cover double-fork/`setsid` descendants, and owner reports test
regressions. No source, generated output, host payload, Docker payload, or
remote state was changed here.

## Contract that cannot be silently narrowed

`content/docs/guides/execution.mdx:373-405` defines the current step's host
process group as a termination target and explicitly says process-group
signals kill “a step's whole process tree”, not only Velnor's direct child.
The same limitation is disclosed in
`content/docs/guides/execution.mdx:414-418`,
`content/docs/reference/runner-protocol.mdx:221-230`, and
`content/docs/concepts/architecture.mdx:209-214`: the live path currently
registers host groups only, while containers/microVMs have separate gaps.
Therefore “setsid/setpgid escape is unsupported” is not by itself a safe
resolution. It either needs a real containment/admission gate before untrusted
execution, or an explicit product contract change approved by the user. A
passing direct-child exit cannot be reported as successful whole-tree cleanup.

The native commands are authoritative checks, not disposable hostile-payload
probes. `.github/ci/project.toml` repeatedly declares `platform = "linux-x64"`
and `native_macos_arm64 = false` (for example lines 42-51, 64-73, 198-207);
many units are `trust = "untrusted-ok"` and execute PR-controlled build/test
scripts. `.github/workflows/ci-unit-rust.yml:524-548` invokes the normal
`velnor-workflow run` unit path. The Docker unit at
`.github/ci/project.toml:56-76` is a separate BuildKit/daemon lifecycle and
must not be used as proof that native process-tree cleanup is correct.

## Proposed platform gate

### Linux

Dedicated cgroup v2 is a viable bounded source fix, if all admission checks
are performed before the command starts:

1. Verify cgroup v2 and usable `cgroup.kill`; create a private runner-owned
   cgroup with no existing members and reject unsafe path/symlink/delegation
   conditions.
2. Place the command process in that cgroup before any untrusted code runs.
   A merely root-owned path is insufficient if the child can move itself or
   descendants to another cgroup. Setup/assignment failure is an explicit
   pre-spawn or pre-user-code failure, never fallback to a plain process group.
3. On cancellation/error, use cgroup kill, then wait under a bounded deadline
   for `cgroup.events`/`populated=0`. Preserve the cgroup and return a cleanup
   or survivor error if zero cannot be proven.
4. Keep Docker/BuildKit descendants under their own container/daemon
   lifecycle. Do not claim that a native cgroup contains a Docker daemon or
   arbitrary container process.

### Windows

A dedicated Job Object is the corresponding viable backend: create it,
configure kill-on-close and no-breakaway policy, assign the child before it
can run (normally suspended creation/assignment/resume), and query/reap until
no active processes remain under a bounded deadline. Existing/nested Job Object
constraints must be checked; if assignment is unavailable, fail admission
instead of falling back to an uncontained child.

### macOS

There is no cgroup-v2/Job-Object equivalent in this design. With the current
project declaration (`native_macos_arm64 = false`, units targeting
`linux-x64`), rejecting an unsupported native macOS capability **before
spawn** is valid as an explicit admission result or routing decision. It must
not become a green skip, a hidden “success”, or a global bypass that causes
all native checks to pass without execution. Local fixture tests may exercise
the rejection path, but cannot be represented as successful native CI.

If native macOS checks are a requirement, that is a user/platform decision,
not a shell-wrapper patch: choose a real platform-specific supervisor/sandbox
with a whole-tree proof, or explicitly approve a trusted-only uncontained mode
and revise the contract/observability. The latter does not satisfy the current
whole-tree promise and is not an equivalent implementation.

## Safe source-fix boundary

The implementable source slice is an execution-containment capability layer
at the native command admission point (currently
`crates/velnor-workflow/src/s2/runtime.rs:2800-3005`):

- select Linux cgroup or Windows Job Object before spawning user command;
- return typed unsupported/unavailable/setup/assignment/cleanup-survivor
  errors; never execute uncontained after a failed probe;
- preserve native unit command selection, output relay, stall deadlines,
  cancellation status, and authoritative nonzero exit results;
- add regression fixtures for detached `setsid`/`setpgid`, forked descendants,
  assignment failure, populated-zero/active-job cleanup, deadline failure,
  unsupported macOS before spawn, and native-vs-Docker lifecycle separation.

The source slice must not: fail all native jobs to manufacture a green result;
silently skip checks; use `jobs -p`, inherited-pipe closure, or process-table
scans as containment; fallback to an uncontained child; or classify a Docker
daemon/container as covered by the native group.

## Gate / alternatives

Do not approve integration until one of these explicit choices is recorded:

1. **Supported native lane (recommended):** Linux cgroup-v2 and Windows Job
   Object backends, with fail-closed capability admission and cleanup proof;
   native checks remain authoritative.
2. **Current project platform scope:** route native macOS requests to a
   supported hosted/provider lane or return explicit unsupported capability;
   no green skip. Keep `native_macos_arm64 = false` and `linux-x64` semantics
   visible in rendered configuration.
3. **Native macOS required:** user authorizes a separate containment/sandbox
   design and its cross-platform pilot proof. This is larger than the bounded
   Linux/Windows fix.
4. **Trusted uncontained macOS:** only by explicit user/product decision and
   a contract rewrite that no longer promises whole-tree termination. It is
   not approval under the current contract and must not be smuggled in as a
   fallback.

Full pilot proof still needs adversarial descendants (`setsid`, `setpgid`,
double-fork, fork storms), cgroup namespace/delegation and privilege changes,
PID reuse, API failure, Docker/systemd external processes, and Linux/Windows
survivor accounting. No host experiment was run for this bounded review.
