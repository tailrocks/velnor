# Plan 021: Make `hashFiles()` stream, parallelize, and memoize — bounded memory, faster cache keys

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. The digest contract must stay
> **byte-identical** (cache keys depend on it) — see Step 3.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs crates/velnor-runner/Cargo.toml`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`hashFiles('**/...')` is on the cache-key hot path of nearly every cache-using
workflow, and it delays the step that gates cache restore/save. The current
implementation reads **each matched file entirely into RAM** then hashes it,
strictly sequentially on one core, and **recomputes** for every identical
`hashFiles(...)` expression (a key and an `if:` using the same pattern re-walk
and re-hash the tree). On large trees (`node_modules`, `target`, big lockfile
globs) this is slow and memory-heavy. This plan streams each file through the
hasher in fixed chunks (bounded memory), fans file hashing across a thread pool,
and memoizes by `(workspace, patterns)` — while keeping the exact digest so
cache keys stay stable across the fleet.

## Current state

- `crates/velnor-runner/src/executor.rs` — `hash_files`, excerpt at
  `executor.rs:5650-5670`:
  ```rust
  fn hash_files(workspace: &Path, patterns: &[String]) -> String {
      let Ok(globs) = build_globs(patterns) else { return String::new(); };
      let mut matches = Vec::new();
      collect_hash_file_matches(workspace, workspace, &globs, &mut matches);
      matches.sort();
      matches.dedup();
      if matches.is_empty() { return String::new(); }
      let mut aggregate = Sha256::new();
      for path in matches {
          let Ok(bytes) = fs::read(&path) else { continue; };   // <-- whole file into RAM
          aggregate.update(Sha256::digest(bytes));               // <-- per-file digest, sequential
      }
      hex_digest(aggregate.finalize().as_slice())
  }
  ```
- **Digest contract (must preserve exactly)**: sorted+deduped match list; per
  file compute `Sha256::digest(bytes)`; feed each file's digest into an
  aggregate `Sha256`; hex-encode. Any change to ordering, per-file-vs-streamed
  digest, or the aggregate feed changes the output and **invalidates every cache
  key across the fleet** — so streaming must produce the identical per-file
  digest, and parallelism must fold in the **sorted** order.
- `hash_artifact_dir` (`executor.rs:4144`) uses the same
  `Sha256::digest(fs::read(file))` loop on the artifact path — apply the same
  streaming fix there.
- Dependencies: `sha2` is already a workspace dep. For parallelism, `rayon` is
  **not** currently a dep — adding it is in scope (or use `std::thread` scoped
  threads to avoid a new dep; prefer the smaller footprint the team accepts).

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner hash --locked`             | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — `hash_files`, `hash_artifact_dir`,
  and a per-job memo cache for `hashFiles`.
- `crates/velnor-runner/Cargo.toml` — only if you add `rayon`.

**Out of scope**:
- Changing the digest algorithm/format — the output must be byte-identical to
  today for the same inputs.
- The glob collection (`collect_hash_file_matches`) — unchanged unless needed.

## Git workflow

- Branch: `advisor/021-hashfiles-streaming-parallel`
- Commit(s): `perf(hashfiles): stream file hashing with bounded memory`,
  `perf(hashfiles): parallelize and memoize per (workspace, patterns)`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Stream each file (bounded memory), identical digest

Replace `Sha256::digest(fs::read(path))` with a streamed per-file digest:
`io::copy` (or a fixed-size read loop) from the file into a `Sha256` hasher,
producing the **same** per-file digest without loading the whole file. Keep the
aggregate feed identical (`aggregate.update(per_file_digest)`). Do the same in
`hash_artifact_dir`.

**Verify**: add the Step 3 equivalence test now and confirm the digest is
unchanged for a known input; `cargo clippy ...` → exit 0.

### Step 2: Parallelize file hashing, fold in sorted order

Hash the matched files in parallel (rayon `par_iter` or scoped `std::thread`),
collect `(path, per_file_digest)`, then fold the per-file digests into the
aggregate **in the sorted `matches` order** (not completion order) so the output
is deterministic and identical to the sequential version.

**Verify**: the Step 3 equivalence test still passes; `cargo clippy ...` → exit 0.

### Step 3: Equivalence test (byte-identical digest)

Add `hash_files_digest_is_stable` in `executor.rs` `#[cfg(test)]`: create a temp
tree with a few files of known content, compute `hash_files` and assert it
equals a **hardcoded expected hex digest** captured from the current
implementation (run the old code once to get the value, or compute the expected
via an independent Sha256-of-Sha256 in the test). Include a case with >1 file to
exercise ordering, and a large-ish file (e.g. a few MB via repeated bytes) to
exercise streaming. The point: prove the new implementation yields the same
digest as before.

**Verify**: `cargo nextest run -p velnor-runner hash --locked` → pass.

### Step 4: Memoize per (workspace, patterns)

Add a small per-job cache (e.g. a `HashMap<(PathBuf, Vec<String>), String>` on
`JobExecutionState`, or a scoped memo) so repeated identical `hashFiles(...)`
expressions within a job reuse the result instead of re-walking/re-hashing.
Ensure the memo does not persist across jobs (files change between jobs).

**Verify**: `cargo fmt --all --check` → exit 0; `cargo nextest run --workspace
--locked` → all pass.

## Test plan

- `hash_files_digest_is_stable`: multi-file + large-file, asserting the exact
  hex digest matches the pre-change output.
- Optional: a memo test that a second call with the same args does not re-read
  (harder to assert; may be skipped — note it).
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] `hash_files` and `hash_artifact_dir` stream files (no `fs::read` of whole files) and hash in parallel
- [ ] The digest is **byte-identical** to the old implementation (test asserts a fixed expected hex)
- [ ] Parallel results are folded in sorted order (deterministic)
- [ ] Repeated identical `hashFiles` within a job are memoized
- [ ] `cargo nextest run --workspace --locked` exits 0
- [ ] Only the in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The `hash_files` excerpt doesn't match (drift).
- The new digest differs from the old for any input — STOP; a changed digest
  invalidates every fleet cache key and must not ship silently.
- Adding `rayon` is undesirable and scoped threads complicate the code beyond
  reason — report; a streaming-only (no parallel) version is still a valid
  smaller win.

## Maintenance notes

- The digest stability test is the guard rail — never let it be "updated" to a
  new value without a deliberate, fleet-wide cache-invalidation decision.
- Reviewer: the only acceptable behavioral change is speed/memory; confirm the
  fixed-digest test and the sorted fold.
