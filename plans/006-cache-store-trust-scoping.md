# Plan 006: Repo/trust-scope the `actions/cache` store and neutralize `..` cache keys

> **Executor instructions**: Follow step by step; run each verification and
> confirm before continuing. STOP-condition ⇒ stop and report. Update
> `plans/README.md` when done. This is a **security** plan.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs crates/velnor-runner/src/container.rs crates/velnor-runner/src/github_adapter.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none (plan 002 also touches `save_cache_result`; if both are
  in flight, land 002 first — see Maintenance notes)
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The `actions/cache` store directory is **daemon-shared with no repository or
trust component** — the only namespacing on an entry is a sanitized version of
the workflow-authored cache key. A Velnor daemon can register at an **org or
enterprise** URL (supported; see `debian/velnor.env`), so jobs from *different
repositories* on the same daemon share one cache namespace keyed only by a
predictable key. Repo B can restore and read repo A's cached build output
(cross-repo exfiltration) and pre-poison a key repo A will later restore
(supply-chain injection into A's build). GitHub's hosted cache is repo-scoped;
Velnor collapses that boundary.

Separately, the key sanitizer preserves `.`, so a whole-segment `..` passes
through unchanged. Combined with `save_cache_result` doing
`fs::remove_dir_all(&cache_dir)` where `cache_dir = store.join(sanitize(key))`,
a key of `..` resolves `cache_dir` to the **parent** of the store and
`remove_dir_all`s it — a destructive delete above the cache root.

The codebase already scopes the opt-in cargo-target store by
`<trust>/<repo>/<workflow>/<job>` (`github_adapter.rs`); this plan brings the
general cache store to the same scoping and closes the `..` edge.

## Current state

- `crates/velnor-runner/src/executor.rs` — the unscoped cache store, excerpt at
  `executor.rs:3666-3674`:
  ```rust
  fn cache_store_dir(state: &JobExecutionState) -> Result<PathBuf> {
      let temp = state.temp_host.as_deref()
          .ok_or_else(|| anyhow::anyhow!("cache actions require a temp directory"))?;
      // Daemon-shared (across slots), not per-slot: cold slots must hit the
      // caches their siblings saved (see container::daemon_shared_root).
      Ok(crate::container::daemon_shared_root(shared_work_root(temp)).join("_velnor_caches"))
  }
  ```
  Note the comment at `container.rs:544` justifies sharing as "same repo trust
  domain" — true only for a repo-scoped daemon, **false** for an org/enterprise
  URL. That is the decision-drift this plan corrects.
- The permissive sanitizer, excerpt at `executor.rs:4163-4171`:
  ```rust
  fn sanitize_artifact_name(name: &str) -> String {
      name.chars().map(|ch| {
          if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') { ch } else { '_' }
      }).collect()                             // '..' survives intact
  }
  ```
- The **existing, correct** trust-scoping pattern to mirror, excerpt at
  `github_adapter.rs:68-95`:
  ```rust
  fn github_cargo_target_store_host(job, temp_host) -> PathBuf {
      crate::container::cargo_target_store_host(temp_host)
          .join(sanitize_store_key(&cargo_target_trust_scope()))              // trust: VELNOR_TRUST_SCOPE or "trusted"
          .join(sanitize_store_key(job_variable(job, "github.repository").unwrap_or("unknown-repository")))
          .join(sanitize_store_key(job_variable(job, "github.workflow").unwrap_or("unknown-workflow")))
          .join(sanitize_store_key(&job.job_display_name))
  }
  fn cargo_target_trust_scope() -> String { /* VELNOR_TRUST_SCOPE, default "trusted" */ }
  ```
- `container::sanitize_store_key` (`container.rs:598`) is the sibling sanitizer;
  like `sanitize_artifact_name`, verify whether it collapses `.`/`..` (read it).
- The consumer of `cache_store_dir` is `save_cache_result` (`executor.rs:3252`,
  `cache_dir = cache_store_dir(state)?.join(sanitize_artifact_name(key))`) and
  the restore path (grep `cache_store_dir` — restore uses the same root).
- The scope value `state` gives you: `JobExecutionState` must expose the
  `github.repository` and `github.workflow` values (grep the struct / its
  `job_variable`-equivalent). If `state` cannot reach the repository string,
  STOP and report — do not invent a scope.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner cache --locked`             | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — `cache_store_dir` scoping,
  `sanitize_artifact_name` `..` hardening, and tests.
- `crates/velnor-runner/src/container.rs` — `sanitize_store_key` `..` hardening
  (only if it also lets `..` through) and its test.

**Out of scope**:
- The cargo-target store (`github_adapter.rs`) — already scoped; do not change.
- The executable tool stores (cargo bin, mise) — that is plan 007.
- Cache **eviction / GC** — that is plan 029.

## Git workflow

- Branch: `advisor/006-cache-store-trust-scoping`
- Commit: `fix(security): scope actions/cache store by trust+repo and reject '..' keys`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Harden the sanitizer against whole-segment `.`/`..`

In `sanitize_artifact_name` (`executor.rs:4163`), after the char mapping, reject
or neutralize a path-traversal segment: if the sanitized result is `"."` or
`".."` (or is empty), return a safe sentinel such as `"_"`. This guarantees no
single key segment can climb a directory. Do the same in
`container::sanitize_store_key` **iff** reading it shows it also passes `..`
through (Step 0: read `container.rs:598` first).

```rust
fn sanitize_artifact_name(name: &str) -> String {
    let mapped: String = name.chars().map(|ch| {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') { ch } else { '_' }
    }).collect();
    match mapped.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => mapped,
    }
}
```

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Scope the cache store by trust + repository

Change `cache_store_dir` to append a `<trust>/<repository>` prefix under
`_velnor_caches`, mirroring `github_cargo_target_store_host`. Reuse the existing
trust-scope helper (`cargo_target_trust_scope` reads `VELNOR_TRUST_SCOPE`,
default `"trusted"`) — if it is private to `github_adapter.rs`, either make it
`pub(crate)` and call it, or add a small `container`/shared helper; do not
duplicate the env-reading logic. Scope by **trust + repository** (workflow/job
granularity is unnecessary for cache correctness and would hurt hit rate):

```rust
fn cache_store_dir(state: &JobExecutionState) -> Result<PathBuf> {
    let temp = state.temp_host.as_deref()
        .ok_or_else(|| anyhow::anyhow!("cache actions require a temp directory"))?;
    let root = crate::container::daemon_shared_root(shared_work_root(temp)).join("_velnor_caches");
    let trust = /* trust scope, default "trusted" */;
    let repo = /* state's github.repository, default "unknown-repository" */;
    Ok(root.join(sanitize_artifact_name(&trust)).join(sanitize_artifact_name(&repo)))
}
```

Both the save path (`save_cache_result`) and the restore path call
`cache_store_dir`, so both get the scoping from this one change — verify by
grepping `cache_store_dir` and confirming there is exactly one root definition.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 3: Tests

Add tests in `executor.rs` `#[cfg(test)]`:
- `sanitize_artifact_name_neutralizes_traversal`:
  `assert_eq!(sanitize_artifact_name(".."), "_");`
  `assert_eq!(sanitize_artifact_name("."), "_");`
  `assert_eq!(sanitize_artifact_name("normal-key.v2"), "normal-key.v2");`
- `cache_store_dir_is_scoped_by_trust_and_repo`: build a `JobExecutionState`
  with a known `github.repository` (model the state construction on an existing
  cache test — grep `cache_store_dir` or `restore_cache` in the test module) and
  assert the returned path contains both the trust segment and the sanitized
  repository segment, and ends under `_velnor_caches/<trust>/<repo>`.
- If `container::sanitize_store_key` was hardened in Step 1, add the matching
  assertion in `container.rs` tests (model after
  `cargo_target_trust_scope_defaults_and_trims` in `github_adapter.rs:660`).

**Verify**: `cargo nextest run -p velnor-runner cache --locked` → new tests
pass; `cargo nextest run --workspace --locked` → all pass.

## Test plan

- Traversal-neutralization tests for both sanitizers.
- A store-path-scoping test proving cross-repo isolation (path contains repo).
- Model state construction on the existing cache tests in `executor.rs`.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` exits 0
- [ ] `sanitize_artifact_name("..")` and `("." )` return `"_"`; same for `sanitize_store_key` if it was affected
- [ ] `cache_store_dir` output path includes a trust segment and the sanitized repository segment
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only `executor.rs` (and `container.rs` if its sanitizer was hardened) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Any "Current state" excerpt doesn't match (drift).
- `JobExecutionState` cannot reach the `github.repository` value — report; do
  not use a different scope key that isn't the repository.
- Making the trust-scope helper reachable would require touching more than
  visibility (`pub(crate)`) or a small shared helper — report.

## Maintenance notes

- **Cache cold-start**: this changes on-disk cache paths, so existing
  `_velnor_caches/<key>` entries go cold once. That is expected and safe (caches
  rebuild). Mention it in the PR / release notes so operators aren't surprised.
- **Plan 002** also edits `save_cache_result` (staging uniqueness). The staging
  dir is derived via `cache_dir.with_file_name(...)`, so it inherits this
  scoping automatically. If 002 has not landed, this plan still works; if both
  are open, land 002 first to avoid a conflict in `save_cache_result`.
- **Plan 007** applies the same scoping idea to the executable tool stores.
- Reviewer: confirm both save and restore resolve to the same scoped root
  (a scoping applied to only one side silently disables all cache hits).
