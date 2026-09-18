# F1 E2E probe: jackin e2e sources at live main

Date: 2026-09-17. Method: READ-ONLY `gh api` (raw blob reads + compare
API). No clones, no writes to the repo (F gate not reached).
Ref: live `main` `0be3fcf95cd33c14fd3fe47af08026f17135a157`;
audited `92f347ac39fbf0d6f9853168e2896a6c60522924` used only for the
1-commit drift diff. Fetched bytes staged under `/tmp/f1probe/` for
verifier re-checks.

Closes the open questions in `/tmp/a0-consumers.md` §2.5.

---

## (a) The 20-capsule fanout is NOT in dind_e2e — it is in usage_broker_e2e

No `dind_e2e*` source contains the literal `20` at all (`dind_e2e.rs`
has zero `20` occurrences; nothing named `*fanout*` exists in any e2e
source). The A0 survey was correct and exhaustive on this point. Both
"20" fanouts live in the usage-broker suite:

1. **Host-process fanout (named constant).**
   `crates/jackin/tests/usage_broker_e2e.rs:26`:
   `const CLIENTS: usize = 20;`
   Consumed by test `usage_broker_twenty_host_processes_make_one_provider_call`
   (`:181`) → `assert_host_process_singleflight(CLIENTS)` (`:190`): re-execs
   the test binary 20x as host OS processes (`JACKIN_USAGE_BROKER_E2E_CHILD=i`,
   barrier files `ready-*`/`go`/`done-*`) and asserts exactly **1** provider
   call (singleflight). Sibling `:186` runs the same helper with literal `2`.

2. **Docker-capsule fanout (literal `20`, the spec §9.1 "20-capsule fanout").**
   `crates/jackin/tests/usage_broker_e2e/docker.rs:201-202`:
   test `usage_broker_desktop_and_twenty_docker_capsules_make_one_provider_call`
   calls `assert_desktop_capsule_singleflight(20).await`
   (sibling `:197` passes literal `2`; there is NO named constant — F1 may
   want one, but must not invent semantics: both call sites pass plain `usize`).
   Helper `assert_desktop_capsule_singleflight(capsules: usize)` (`:424`):
   `start_capsule()` per index (`:513`) → `docker run -d --rm --name
   jackin-usage-e2e-<pid>-<i> --mount type=bind,src=<relay-i>,dst=/jackin/run
   python:3.14-alpine sleep 120` (`CAPSULE_IMAGE`, `:19`), one
   `UsageRelayGuard` tunnel per capsule, `docker exec` python client per
   capsule (`run_capsule`, `:577`), barrier-sync via relay-dir files
   (`client-ready` → `client-go` → `requested`), then asserts the shared
   generation completes with a single provider call. `Drop` impl
   (`:571`) force-removes each container.

F1 consequence: the "serial groups, 20-capsule fanout" requirement is
satisfied by invoking the existing `docker-e2e` profile (serial group
`docker-e2e`, `max-threads = 1`, verified in `.config/nextest.toml`
lines 35, 67–69 at live main) — no new fanout code needed.

---

## (b) `per_mount_isolation_e2e.rs` disposition

**What it tests.** One hermetic test,
`materialize_then_clean_exit_removes_record_and_branch` (`#[tokio::test]`,
`:79`): builds a `ResolvedWorkspace` with a single `Worktree` mount inside
`TempDir`s, runs `materialize_workspace` with a `ScriptedRunner` (canned git
outputs — no real git, no docker), asserts the aux bind-mount metadata
(host `.git` target `/jackin/host/workspace/jackin/.git`, `.git`/`gitdir`
override file contents, **no** `commondir` override), then runs
`finalize_foreground_session` with `NoPrompt` + `common::NoOpDocker` and
asserts `FinalizeDecision::Cleaned`, empty records, and recorded
`worktree remove --force` + `branch -D`.

**Why it runs under default.** Two independent reasons, both verified:
- Nextest filters name only 4 binaries
  (`dind_e2e|session_send_e2e|usage_broker_e2e|load_options_e2e`):
  default *excludes* them (`.config/nextest.toml:5`) and `docker-e2e`
  *includes* them (`:9`). `per_mount_isolation_e2e` matches neither, so
  the default profile picks it up.
- Unlike the other four, it carries **no** `#![cfg(feature = "e2e")]`
  gate (file opens with `#![expect(...)]` + `mod common;`), no
  `require_e2e_prereqs`, no `JACKIN_CAPSULE_BIN`, no `script(1)`, no
  daemon use. Its only docker-adjacent lines are the trait import
  (`use jackin_docker::{CommandRunner, RunOptions};`, `:10`) and the
  `NoOpDocker` stub (`crates/jackin/tests/common/mod.rs:76`, used at
  `:195`, `:203`).

**Which profile SHOULD own it: `default` (current placement is correct).**
It needs no daemon, no capsule ELF, no PTY, no serial group. The `_e2e`
suffix is a misnomer — it is a hermetic integration test. F1 options:
(i) leave it in `default` (no coverage change, cheapest, correct); or
(ii) rename to drop the `_e2e` suffix so future filter audits stop
tripping on it. F1 must NOT move it to `docker-e2e` (would needlessly
serialize a daemon-free test behind `max-threads = 1` and require a
Docker host) and must NOT add it to the default-exclusion without adding
it to `docker-e2e` (would silently drop coverage).

---

## (c) `JACKIN_CAPSULE_BIN` contract + same-source capsule ELF build

**Contract (`dind_e2e/common.rs`, live main).**
`require_e2e_prereqs()` (`:18`) calls `require_capsule_binary_override()`
first (`:19`), which (`:39-79`): panics unless `JACKIN_CAPSULE_BIN` is set;
asserts the path `is_file` (`:53`), asserts the unix executable bit
(`mode & 0o111 != 0`, `:65`), and asserts ELF magic
(`7f E L F` via `is_elf_binary`, `:71-78`, `:81-87`). The panic text
forbids falling back to the preview-release download verifier and
prescribes `jackin-dev pr sync <N>` + `env.sh`, else
`eval "$(cargo run --bin build-jackin-capsule -- --export)"`.

**Who consumes it.** `dind_e2e.rs` (every test: `require_e2e_prereqs()` +
`e2e_serial_lock()`), `session_send_e2e.rs` (shares `dind_e2e/common.rs`
verbatim via `#[path]`, `:50-51`, `:69`, `:96`), and `load_options_e2e.rs`
(weaker form: asserts `var_os(...).is_some()`, `:197-198`, no ELF/exec
check). `usage_broker_e2e` does **not** consume it — its "capsules" are
`python:3.14-alpine` containers (`docker.rs:19`) running python tunnel
scripts, no capsule ELF involved. `per_mount` does not consume it either.
Nothing in e2e sources copies or installs the binary beyond gating on
it; product-side resolution (by the `jackin` binary under test at `load`
time) was not probed — out of scope for this e2e-surface probe.

**How CI must build the same-source ELF**
(`crates/jackin/src/bin/build_jackin_capsule/main.rs`, 450 lines):
`cargo run --bin build-jackin-capsule -- --export` builds `-p jackin-capsule`
from the same checkout (`workspace_root()` walks up from
`CARGO_MANIFEST_DIR` to `[workspace]`) via
`cargo zigbuild --profile <release|debug> -p jackin-capsule
--target <aarch64|x86_64>-unknown-linux-gnu.2.17` (`zigbuild_target`,
`:304`; `build_via_zigbuild`, `:370`). Requires `zig` + `cargo-zigbuild`
on PATH (`mise install zig cargo:cargo-zigbuild` per `:21`, `:327`) and
auto-runs `rustup target add <triple>` (`:348-366`). Output caches to
`<cache>/jackin-capsule/<version>/linux-<arch>/jackin-capsule` (release
default; `-debug` suffix for `--profile debug`; `:163-181`); `--export`
prints `export JACKIN_CAPSULE_BIN='<path>'` for eval (`:91-96`); default
arch follows the host container, overridable with
`--arch arm64|amd64` (`:12`, `:237`).

F1 `docker-e2e` job shape therefore: install zig/cargo-zigbuild + rustup
target → run the builder with `--export` → eval into env → 
...[truncated 4402 chars]