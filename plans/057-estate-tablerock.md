# Plan 057: tablerock — replace macOS-only CI with Class D

> **Executor instructions**: Follow step by step; verify each step; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/tailrocks/tablerock`,
> `git log --oneline -1` — planned against `03c6dd9`. On excerpt mismatch, STOP.

## Status

- **Priority**: P3 (Phase 3 #13 — last)
- **Effort**: S
- **Risk**: LOW (early-stage repo; almost no CI to lose)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md
- **Category**: dx
- **Planned at**: velnor repo commit `48b04ad`, target `03c6dd9`, 2026-07-18
- **Current**: IN PROGRESS — delivered through trunk-only `main` at `9763873`;
  local strict clippy, actionlint, audit-ci, and 768/768 nextest pass. Native
  checkpoint run `29827520875` is fully green; V-B/V-C remains pending the
  organization-fleet migration.

## Why this matters

tablerock (early terminal DB workbench) is fully non-compliant: its single
workflow `dependencies.yml` runs on `macos-15` (forbidden), uses
`dtolnay/rust-toolchain@4cda84d5 # stable`, `EmbarkStudios/cargo-deny-action`,
no mise, no lanes, no concurrency, no timeouts, no root config files. It gets
a fresh Class D build — nothing is migrated, everything is written new.

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/tablerock`, `main`. One workflow:
`.github/workflows/dependencies.yml` (1 job, `runs-on: macos-15`, checkout
v7.0.0 + dtolnay + cargo-deny-action v2.1.1). No `mise.toml`,
`rust-toolchain.toml`, or `renovate.json` at root.

Template: reuse the **complete Class D `ci.yml` + root files from plan 055**
(`plans/055-estate-library-trio-class-d.md`, section "The Class D template" —
self-contained YAML + toml). Since tablerock is a TUI app (not a published
library), drop the `Package` step from the template.

## Scope

**In scope**: `.github/workflows/ci.yml` (create),
`.github/workflows/dependencies.yml` (replace — see step 2), root
`rust-toolchain.toml`, `mise.toml`, `renovate.json`, `.github/AGENTS.md`.
**Out of scope**: sources; release automation (none exists — do not invent).

## Git workflow

Branch `velnor-estate-standard`; `ci:` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: Root files + ci.yml

Write `rust-toolchain.toml` (stable, minimal, rustfmt+clippy), `mise.toml`
(`cargo:cargo-nextest`, `cargo:cargo-deny`, idiomatic rust), `renovate.json`
(copy holla's), and the Class D `ci.yml` from plan 055 minus the `Package`
step. Verify locally first: `cargo fmt --all --check && cargo clippy
--workspace --all-targets --locked -- -D warnings && cargo nextest run
--workspace --locked` — if clippy or tests fail on the CURRENT code, record
the failures and STOP (CI must not land red; the operator decides whether to
fix code first).

**Verify**: local gate exit 0; `actionlint` exit 0.

### Step 2: Replace dependencies.yml

Rewrite as a `deny` job inside ci.yml (mise-installed `cargo deny check` —
matching the velnor repo's own `mise run deny` pattern) and delete
`dependencies.yml`; or, if the operator prefers a separate schedule, keep the
file but: canonical matrix, mise cargo-deny, timeout 15, concurrency, no
macOS, no dtolnay. Default: fold into ci.yml.

**Verify**: `grep -rn "macos\|dtolnay\|cargo-deny-action" .github/workflows/` → none.

### Step 3: AGENTS + hygiene

`.github/AGENTS.md` (three lanes + 26.04); confirm timeouts + concurrency
present (template carries them).

**Verify**: python timeout check exit 0.

## Test plan

Local cargo gate; operator three-lane dispatch on ci.yml. §2.11 Class D
budget (no-change rerun ≤ 60 s Velnor).

## Done criteria

- [x] Class D ci.yml + root tool files landed; native Apple preview remains an
  explicit product-specific macOS exception
- [x] Local gate green; actionlint green
- [ ] Operator dispatches green or deferred; no out-of-scope changes

## STOP conditions

- Current code fails fmt/clippy/nextest locally (step 1) → STOP with the
  failure list; landing red CI is worse than no CI.
- tablerock genuinely needs a macOS-only capability (e.g. tests exercising
  macOS terminal APIs) → STOP; operator decision §12.4 — do NOT silently keep
  a macOS job.

## Maintenance notes

- This repo previously had no test CI at all — the new gate may surface
  latent breakage; that is signal, not noise. Reviewer: confirm the deny job
  uses the same `deny.toml` posture as other tailrocks repos (create a
  minimal `deny.toml` only if `cargo deny` requires one to pass).
