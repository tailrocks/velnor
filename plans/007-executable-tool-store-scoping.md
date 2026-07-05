# Plan 007: Prevent cross-job code execution via the shared writable tool stores

> **Executor instructions**: This plan has an **investigation step first** — do
> Step 1 and report its findings inline in your PR description before making the
> Step 2+ changes. Run every verification. STOP-condition ⇒ stop and report.
> Update `plans/README.md` when done. **Security** plan.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/container.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED
- **Depends on**: 006 (reuse the same trust-scope helper it exposes)
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The daemon bind-mounts **daemon-shared, writable** tool stores into every job
container: the cargo store at `/github/home/.cargo` (which includes
`.cargo/bin`, holding installed binaries/proxies) and the mise store at
`/opt/mise/installs` + `/opt/mise/cache` (installed toolchains). These stores
are keyed only off the daemon store root — **no repository or trust component**.
Because the mounts are read-write and the directories hold executables placed on
`PATH`, a job can overwrite a cargo proxy or plant a tool that a **later job on
the same daemon — potentially a different repository — then executes**. That is
a cross-job code-execution / persistence vector, not just data poisoning, and it
has the same precondition as the cache issue (a daemon serving more than one
repo, e.g. an org/enterprise URL). The opt-in cargo-**target** store was already
trust-scoped; the executable stores were not brought along.

The isolation/speed tradeoff is real (these stores exist for measured warm-cache
speedups), so Step 1 investigates the exact mount set and the chosen strategy
before changing behavior.

## Current state

- `crates/velnor-runner/src/container.rs` — the shared store hosts and their
  mounts. Excerpts:
  - `container.rs:564-575`:
    ```rust
    pub(crate) fn cargo_store_host(temp_host: &Path) -> PathBuf {
        daemon_store_root(temp_host).join("_velnor_cargo")   // mounted at ~/.cargo/{registry,git}
    }
    pub(crate) fn mise_store_host(temp_host: &Path) -> PathBuf {
        daemon_store_root(temp_host).join("_velnor_mise")     // mounted at /opt/mise/{installs,cache}
    }
    ```
  - The mount assembly is around `container.rs:77-96` (the `-v` args for the job
    container; read it to enumerate exactly which host store maps to which
    container path and whether each is `:ro` or `:rw`).
- The trust-scope helper to reuse comes from plan 006 (`cargo_target_trust_scope`,
  reading `VELNOR_TRUST_SCOPE`, default `"trusted"`) and
  `sanitize_store_key` (`container.rs:598`).
- The existing scoped pattern is `github_cargo_target_store_host`
  (`github_adapter.rs:68`), which scopes by `<trust>/<repo>/<workflow>/<job>`.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner container --locked`         | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/container.rs` — store-host path derivation and the
  mount assembly, plus tests.

**Out of scope**:
- The cargo **registry/git** data cache is downloaded-crate data, not
  executables. Registry/git may stay shared for warmth **if** `.cargo/bin` is
  separated out (see Step 2). Do not needlessly de-warm registry/git.
- The `actions/cache` store — plan 006.

## Git workflow

- Branch: `advisor/007-executable-tool-store-scoping`
- Commit: `fix(security): isolate writable executable tool stores across trust+repo`
- Do NOT push/PR unless instructed.

## Steps

### Step 1 (INVESTIGATE — report before changing)

Read `container.rs:77-96` and enumerate, in your PR description, every shared
store mount: host path → container path → `ro`/`rw`. Identify precisely which
mounts expose **executables on PATH** (`.cargo/bin`, `/opt/mise/installs`,
mise shims) versus **pure data** (`.cargo/registry`, `.cargo/git`, `/opt/mise/cache`).
Confirm whether `cargo_store_host` mounts the whole `~/.cargo` (including `bin`)
or only `registry`/`git`. This determines the fix surface.

**Verify**: no code change yet; findings written in the PR description.

### Step 2: Choose and apply an isolation strategy

Apply the **least-warmth-cost** strategy that removes cross-job write-execute:

- **Executable dirs** (`.cargo/bin`, `/opt/mise/installs`, shims): scope the
  host path by `<trust>/<repo>` using the plan-006 helper + `sanitize_store_key`,
  mirroring `github_cargo_target_store_host` (trust+repo granularity is enough).
  A job can then only write executables into its own repo's scope.
- **Pure-data dirs** (`.cargo/registry`, `.cargo/git`, mise `cache`): may remain
  daemon-shared for warmth — they are not executed. Keep them shared.

If the current code mounts `.cargo` as a single store (bin + registry together),
split it: a scoped writable path for `bin` and a shared path for
`registry`/`git`. If that split is not cleanly possible without broad changes,
STOP and report — do not force it.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 3: Tests

Add tests in `container.rs` `#[cfg(test)]` (model after the store-scoping test
`github_cargo_target_store_is_scoped_by_trust_repo_workflow_and_job` in
`github_adapter.rs:634`):
- The executable store host path for a job includes the trust segment and the
  sanitized repository segment.
- Two jobs with different `github.repository` values resolve to **different**
  executable store paths.
- A pure-data store (registry) that you intentionally keep shared resolves to
  the same path for both repos (documents the deliberate tradeoff).

**Verify**: `cargo nextest run -p velnor-runner container --locked` → pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- Path-scoping tests proving executable-store isolation across repos and
  documenting which stores stay shared.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] PR description lists every shared mount (host→container, ro/rw) and marks executable vs data
- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] Executable store host paths include `<trust>/<repo>`; two different repos get different paths (test proves it)
- [ ] Pure-data stores that remain shared are explicitly justified in a code comment
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only `container.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- `container.rs` mount code doesn't match the excerpt (drift).
- Splitting `.cargo/bin` from `.cargo/registry` requires changing the job
  container's env or the executor — report the surface first.
- The plan-006 trust-scope helper is not yet available (land 006 first).

## Maintenance notes

- This trades some cross-repo warm-cache benefit for isolation on multi-repo
  daemons. On a single-repo daemon the scoping is a no-op for warmth (one repo →
  one scope). Note the expected first-run cold-start in release notes.
- The seeded-base optimization (mount an image-baked tool base read-only, give
  each scope a writable overlay) is a **follow-up** if warmth regresses
  measurably — do not build it here; record it as deferred.
- Reviewer: verify no executable directory remains mounted `rw` at an unscoped
  shared path.
