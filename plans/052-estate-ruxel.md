# Plan 052: ruxel — lanes, latest action majors, mold, timeouts

> **Executor instructions**: Follow step by step; verify each step; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/tailrocks/ruxel`,
> `git log --oneline -1` — planned against `84f64b7` (branch
> `docs/improve-audit-plans`; the CI file lives on `main` — check out or diff
> against `main` first). On excerpt mismatch, STOP.

## Status

- **Priority**: P2 (Phase 2 #6)
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md
- **Category**: dx
- **Planned at**: velnor repo commit `48b04ad`, target `84f64b7`, 2026-07-18

## Why this matters

ruxel (Rust Ansible executor + uv oracle) has sccache + mise + registry cache
but is behind on action majors (`actions/checkout@v6` where the estate
standard is v7; `actions/cache@v5` where the estate uses v6;
`jdx/mise-action@v4.1.0` vs v4.2.x) — a direct Law-2 (latest-and-greatest)
violation — and has no lanes, no mold, no timeouts. Its Velnor lane also
exercises ansible/uv through mise, which validates the mise host store for
non-Rust tools.

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/ruxel`. Single workflow
`.github/workflows/ci.yml`. Verified 2026-07-18: 5× `runs-on: ubuntu-latest`;
concurrency present (1/1); zero `timeout-minutes`; pins:
`actions/cache@27d5ce7f...# v5`, `actions/checkout@df4cb1c0...# v6`,
`jdx/mise-action@dba19683...# v4.1.0`,
`mozilla-actions/sccache-action@1583d6b3...# v0.0.10`. No mold. Root has
`mise.toml`, `renovate.json`, `rust-toolchain.toml`.

Canonical blocks (inline; exact texts):

```yaml
# dispatch input
      lanes:
        description: velnor (default) | github | both
        type: choice
        default: velnor
        options: [velnor, github, both]

# inline matrix (single line in real YAML) + runs-on
strategy:
  fail-fast: false
  matrix:
    config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
runs-on: ${{ matrix.config.runner }}

# compile-job env
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "false"
  SCCACHE_CACHE_SIZE: 20G

# setup order: checkout (persist-credentials: false) → rui314/setup-mold →
# sccache-action (version: v0.16.0, disable_annotations: "false") →
# jdx/mise-action → cargo registry cache (actions/cache on
# ~/.cargo/registry/{cache,index,src} + ~/.cargo/git/db, key
# cargo-registry-<lane>-<os>-hashFiles('**/Cargo.lock'))
```

## Scope

**In scope**: `.github/workflows/ci.yml`, new `.github/AGENTS.md`.
**Out of scope**: sources, `mise.toml` contents (tool list stays), ansible
playbooks, uv config.

## Git workflow

Branch `velnor-estate-standard` off `main`; `ci:` commits with
`git commit -s`; no push without operator instruction.

## Steps

### Step 1: Bump action majors to latest, SHA-pinned

`actions/checkout` → latest v7.x release SHA (`# v7.x.y` comment);
`actions/cache` → latest v6.x; `jdx/mise-action` → latest v4.2.x. Resolve
latest with `gh api repos/<owner>/<repo>/releases/latest --jq .tag_name`, then
pin the tag's commit SHA (`gh api repos/<owner>/<repo>/git/ref/tags/<tag> --jq .object.sha`;
if the object is an annotated tag, dereference once more via
`gh api repos/<owner>/<repo>/git/tags/<sha> --jq .object.sha`).

**Verify**: `grep -n "uses:" .github/workflows/ci.yml` — every entry has a
40-char SHA + `# v<tag>` comment; no v5 cache, no v6 checkout.

### Step 2: Lanes + matrix + env + mold

Apply the canonical blocks: lanes input, inline matrix on every job,
`runs-on: ${{ matrix.config.runner }}`, lane suffix in job names, standard
env, insert `rui314/setup-mold` (latest SHA-pinned) before the sccache step.
`ubuntu-latest` → gone (matrix carries `ubuntu-26.04`).

**Verify**: `actionlint` exit 0; `grep -n "ubuntu-latest" .github/workflows/` → none.

### Step 3: Hygiene

`timeout-minutes: 20` per job; `persist-credentials: false`;
`.github/AGENTS.md` (three lanes + 26.04, note ansible/uv via mise).

**Verify**: python timeout check (see plan 047 step 5) exit 0.

## Test plan

actionlint + YAML parse. Operator: three-lane dispatch; on the Velnor lane,
verify the ansible/uv mise installs are warm on second run (no re-download in
logs). §2.11 timing table in PR; Class B budget (no-change rerun ≤ 90 s).

## Done criteria

- [ ] actionlint exit 0; all pins latest-major + SHA
- [ ] Lanes + matrix + mold + contract env in place; zero ubuntu-latest
- [ ] Timeouts everywhere; AGENTS.md exists
- [ ] Operator dispatches green or deferred
- [ ] No out-of-scope changes

## STOP conditions

- Velnor lane fails installing ansible/uv via mise (host-store or network
  issue) → STOP, report exact mise error; likely a runner mise-store gap.
- A bumped action major changes an input's name/behavior your workflow uses →
  STOP, report the migration note from its release notes.

## Maintenance notes

- Renovate present — confirm it tracks the new pins (SHA-pin strategy with
  `# vX` comments is what Renovate's `helpers:pinGitHubActionDigests` expects).
