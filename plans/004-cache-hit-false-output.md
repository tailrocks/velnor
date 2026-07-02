# Plan 004: Emit `cache-hit: 'false'` on a total cache miss (not empty string)

> **Executor instructions**: Follow step by step; run each verification and
> confirm before continuing. STOP-condition ⇒ stop and report. Update
> `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The `actions/cache` action documents that its `cache-hit` output is the string
`'true'` on an exact-key hit and `'false'` otherwise. Velnor returns an **empty
string** on a total miss (no key and no restore-key matched) instead of
`'false'`. Workflows written as `if: steps.cache.outputs.cache-hit == 'false'`
(a documented, real pattern) therefore misbehave under Velnor: `"" == 'false'`
is false, so the guarded step is skipped when GitHub would run it. The common
`!= 'true'` form is unaffected, which is why this has gone unnoticed — but the
`== 'false'` form is legitimate and must match. One-line contract fix.

## Current state

- `crates/velnor-runner/src/executor.rs` — cache restore output construction,
  excerpt at `executor.rs:2919-2930`:
  ```rust
  let exact_hit = matched_key.as_deref() == Some(key.as_str());
  let mut outputs = BTreeMap::new();
  outputs.insert(
      "cache-hit".to_string(),
      matched_key
          .as_ref()
          .map(|_| exact_hit.to_string())   // Some(key): "true"/"false"
          .unwrap_or_default(),             // None (total miss): ""  <-- BUG, should be "false"
  );
  outputs.insert("cache-primary-key".to_string(), key.clone());
  ```
  Behavior today: exact hit → `"true"`; restore-key partial hit → `"false"`;
  **total miss → `""`** (should be `"false"`).
- `actions/cache` contract: `cache-hit` is `'true'` only on an exact primary-key
  match; any other outcome (partial restore-key match, or nothing) is `'false'`.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| One test  | `cargo nextest run -p velnor-runner cache_hit --locked`         | new test passes |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — the `cache-hit` output value and a
  unit test.

**Out of scope**:
- `cache-primary-key` / `cache-matched-key` outputs — correct as-is.
- Restore or save logic — not touched.

## Git workflow

- Branch: `advisor/004-cache-hit-false-output`
- Commit: `fix(cache): emit cache-hit 'false' on a total miss to match actions/cache`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Return `"false"` on a total miss

Change the `cache-hit` value so it is `exact_hit.to_string()` in **all** cases:

```rust
outputs.insert("cache-hit".to_string(), exact_hit.to_string());
```

`exact_hit` is already `false` when `matched_key` is `None`, so this yields
`"false"` on a total miss, `"false"` on a restore-key partial hit, and `"true"`
only on an exact hit — matching the `actions/cache` contract. Remove the now-dead
`matched_key.as_ref().map(...).unwrap_or_default()` wrapping.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 2: Unit test the three outcomes

Find the existing cache restore tests in the `executor.rs` `#[cfg(test)]`
module (grep for `cache-hit` or `restore_cache` in tests). Add or extend a test
`cache_hit_output_matches_actions_cache` asserting the `cache-hit` output is:
- `"true"` when the exact primary key matched,
- `"false"` when only a restore-key matched, and
- `"false"` (not `""`) on a total miss.

If restore is hard to drive directly in a unit test, add a focused test on the
smallest reachable helper that produces the outputs map. Do not weaken the
assertions to accommodate test setup difficulty — if you cannot reach a total
miss, STOP and report.

**Verify**: `cargo nextest run -p velnor-runner cache_hit --locked` → passes;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- New/extended test covering exact hit, restore-key partial hit, and total miss,
  asserting the exact string values (`"true"`, `"false"`, `"false"`).
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` exits 0
- [ ] The `cache-hit` insert no longer uses `unwrap_or_default()`; a total miss yields `"false"`
- [ ] `cargo nextest run --workspace --locked` exits 0; the new test asserts the total-miss case is `"false"`
- [ ] Only `executor.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The `cache-hit` construction doesn't match the excerpt (drift).
- You cannot construct a total-miss case in a test — report rather than
  assert something weaker.

## Maintenance notes

- Reviewer: confirm no code reads `cache-hit == ""` anywhere (grep
  `cache-hit` across the repo) that relied on the empty-string behavior.
- This aligns Velnor with the `actions/cache` output contract; if a future
  native cache adapter is added, reuse this same convention.
