# Plan 044: Host git-mirror store + reflink cache copies

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/executor.rs crates/velnor-runner/src/storage.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P1 (V1.11 + V1.13; p3 doc ranks 5 + defect 5 — git mirror
  CONFIRMED, checkouts 5–16 s → <1 s)
- **Effort**: M
- **Risk**: MED (checkout correctness is sacred — trust boundaries)
- **Depends on**: plans/035-canonical-storage-contract.md (store
  registration for the mirror class)
- **Category**: perf
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Measured: network checkout costs 5–16 s per job (p3 baseline table); a
per-repo bare mirror on the host makes it O(delta) — fetch only new objects,
local clone. Separately, cache restore/save is full byte-copy
(`std::fs::copy`) on an XFS host where reflink (`FICLONE`) is O(metadata) —
p3 defect 5. Both are invisible to workflows (Law 1: runner-internal,
observable step semantics preserved — checkout still produces the identical
work tree and reports identically).

## Current state (from the audits; re-locate by symbol)

- Eager checkout runs HOST-side in `execute_script_job_inner`
  (`runner.rs:3592-3640`, `execute_checkout` at `:3607`) via git CLI through
  `command_runner`.
- Cache copies: `executor.rs:4383-4428` (`copy_dir_contents`,
  `copy_artifact_download_contents`), `:4452-4474` filtered variant;
  restore/save at `:3496`/`:3571`; all via `fs::copy`.
- No reflink/FICLONE anywhere (grep-confirmed). Host filesystem: Sentry root
  is XFS (reflink-capable); dev machines vary (APFS supports clonefile;
  handle via a capability probe + fallback).
- Store classes/layout authority: plan 035's `storage.rs`. Trust model:
  mirrors are content keyed by repository — scope mirrors by
  `<trust>/<repo>` like the executable stores (`container.rs:773-790`
  pattern) since a poisoned mirror = poisoned checkouts.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Focused | `cargo nextest run -p velnor-runner "checkout or reflink"` | pass |

## Scope

**In scope**: checkout fast path in `runner.rs` (`execute_checkout` call
area), a `git_mirror` module (fetch/clone-with-reference logic), reflink
helper + adoption in `executor.rs` copy sites, storage class registration
(`storage.rs`), tests, `docs/runner-usage.md` note.
**Out of scope**: the actions/checkout ADAPTER semantics (inputs, ref
resolution — unchanged; only the transport gets faster), submodule mirroring
(first cut: mirrors for the primary repo only; submodules stay network),
cache GC of mirrors (register the class; 037's policy covers it).

## Git workflow

Branch `velnor-estate-standard`; `perf(runner):` commits, `git commit -s`;
no push without operator instruction.

## Steps

### Step 1: Mirror store + fetch

`git_mirror.rs`: mirror path
`<cache_root>/<trust>/git-mirrors/<sanitized owner__repo>.git` (register the
class in `storage.rs`). `ensure_mirror(repo_url, token)`:
`git init --bare` once, then `git fetch <url> +refs/*:refs/*` (prune off;
tolerate failure → checkout falls back to direct network path, log the
reason). Auth: reuse exactly the credential mechanism `execute_checkout`
uses today (inspect it — token via header/askpass; NEVER write the token
into the mirror's config or remotes; pass per-invocation).
Concurrency: `flock` the mirror dir during fetch (two slots, one repo).

**Verify**: unit tests with local file:// remotes (create a tempdir origin,
mirror it, fetch twice, assert delta fetch via new-commit presence).

### Step 2: Checkout fast path

In the checkout path (`runner.rs:3607` area): when a mirror exists/refreshes
successfully, clone with `--reference <mirror> --dissociate` (dissociate =
work tree independent of the mirror afterward — GC-safe) or fetch from the
mirror as an additional local source; the produced work tree, ref, and
reported log lines stay IDENTICAL (log-format contract — the step output
must not change shape; keep any existing progress lines).

**Verify**: test `checkout_uses_mirror_when_available` (file:// origin +
mirror; assert git invocation args via the command recorder — find how
existing checkout tests record git argv, `runner.rs` test module) and
`checkout_falls_back_without_mirror`.

### Step 3: Reflink helper

`fs_copy.rs` (or into `executor.rs`): `clone_or_copy(src, dst)` — on Linux
try `FICLONE` ioctl (via `rustix` if present in the tree — check
`Cargo.lock`; else `libc`), on macOS `clonefile`, fallback `fs::copy`.
Adopt in `copy_dir_contents` / `copy_dir_contents_filtered` /
`copy_artifact_download_contents` (`executor.rs:4383+`).

**Verify**: unit test asserting fallback correctness everywhere (content
equality); on a reflink-capable CI/dev host the fast path is exercised —
gate the assertion on a runtime capability probe, not cfg alone.

### Step 4: Ops note

`docs/runner-usage.md`: mirror store location, trust scoping, budget class,
and the fallback semantics; one paragraph.

**Verify**: grep.

## Test plan

≥ 5 new tests (mirror fetch/delta, fast-path argv, fallback, reflink
equality). Full gates. Operator acceptance: warm fixture run — checkout step
< 1 s (was 5–16 s); `cache du` (037) shows the mirror class; second run of a
big repo (java-monorepo) fetches only delta.

## Done criteria

- [ ] Gates exit 0; new tests pass
- [ ] Checkout output shape unchanged (diff a before/after step log — no new
      or removed lines besides timing)
- [ ] Token never persisted in mirror config
      (`git config -l` in the test mirror shows no auth)
- [ ] Reflink helper adopted at all three copy sites; fallback proven
- [ ] No out-of-scope changes

## STOP conditions

- The checkout implementation turns out to run INSIDE the job container
  (not host-side as mapped) → the mirror must be bind-mounted read-only
  into the container instead; if that changes the trust analysis, STOP.
- `--dissociate` cost on huge repos erases the win (it repacks) → switch
  strategy to `--reference` without dissociate + mirror-retention guarantee
  (mirror class becomes UNDELETABLE while any work tree references it —
  needs 036 lease integration); STOP and propose if so.
- Reflink ioctl needs a new crate that conflicts with deny.toml → STOP with
  candidates.

## Maintenance notes

- Fork/PR checkouts of untrusted code MUST NOT warm the trusted mirror —
  the `<trust>` path component handles it; reviewers verify the scope is
  taken from the daemon's trust scope, not the job's claims.
- Submodule mirroring is the natural follow-up once single-repo wins are
  proven.
