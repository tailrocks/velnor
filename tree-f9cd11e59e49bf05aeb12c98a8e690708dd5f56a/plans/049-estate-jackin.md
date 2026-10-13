# Plan 049: jackin — Velnor default, inline matrix, mold, kill double-cache, no macOS

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions are binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/jackin-project/jackin`,
> `git log --oneline -1` — planned against commit `ad00e3cd6`. On drift,
> re-verify excerpts; on mismatch, STOP.

## Status

- **Priority**: P1 (Phase 1 reference repo #3 — largest pipeline)
- **Effort**: L
- **Risk**: MED-HIGH (13 workflows, release + docs + preview machinery)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md; plans/047-estate-java-monorepo.md (pattern reference must land first)
- **Category**: dx / perf
- **Planned at**: velnor repo commit `48b04ad`, target repo commit `ad00e3cd6`, 2026-07-18

The mandatory strict-capability approval request is fully specified in
[`../docs/capability-proposal-attest-build-provenance-v4.md`](../docs/capability-proposal-attest-build-provenance-v4.md).
Do not implement it without an explicit yes to that exact surface.

## Why this matters

jackin is the most sophisticated estate pipeline and today defaults to the
GitHub lane (`LANES: ... || 'github'`), runs 48× `runs-on: ubuntu-latest`,
1× `macos-latest`, has no mold anywhere, and stacks 23× `Swatinem/rust-cache`
on top of 29× `actions/cache` + sccache — the double-cache class. Flipping it
to Velnor-default with the standard stack is the largest single speed and
consistency win in the estate, and its composites (`sccache-stats`,
`cache-cargo-registry`, `aggregate-needs`) are patterns other repos copy.

## Current state

Target repo: `/Users/donbeave/Projects/jackin-project/jackin`, `main`.
Workflows (13): `cache-cleanup.yml ci.yml construct.yml docs.yml hygiene.yml
jackin-dev.yml preview.yml release.yml renovate-validate.yml renovate.yml
reuse-compliance.yml rust-nextest.yml` (+ AGENTS.md/CLAUDE.md prose files).
Composites in `.github/actions/`: `aggregate-needs build-release-archive
cache-cargo-registry check-deployed-docs download-ci-xtask download-codebook
sccache-stats sign-and-attest-archive sign-capsule-manifest`.

Verified facts (2026-07-18):

- `ci.yml:45` (and :320, :384):
  `LANES: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'github' }}`
  — automatic events default to **github**. Coordinator `matrix-setup` at
  `ci.yml:36-38` on `ubuntu-latest`; consumers use
  `config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}` +
  `runs-on: ${{ fromJSON(matrix.config.runner) }}` (ci.yml:445-446, 469-470).
- `preview.yml` lanes input: `options: [github, velnor, both]`, `default: github`.
- Pins are current-major and SHA-pinned (checkout v7.0.0, cache v6.1.0,
  mise-action v4.2.0, Swatinem v2.9.1, paths-filter v4.0.2, deploy-pages v5).
- 48× `runs-on: ubuntu-latest`, 2× `ubuntu-24.04`, 1× `macos-latest`.
- `grep -c` counts: 23× Swatinem, 29× `actions/cache@` (+6 `cache/restore`),
  27× `./.github/actions/cache-cargo-registry`, 0× mold.
- 8 of 13 workflows have `concurrency`; 11 have some `timeout-minutes`.
- 8× `fetch-depth: 0`, of which 1 carries a justifying comment
  ("README freshness owns checkout (fetch-depth 0)").
- A `sccache-stats` composite exists and is invoked as an explicit step —
  the strict capability contract forbids build-step cache reporting
  (the action/adapter post step owns it).

Canonical standard blocks (identical to the fixture-proven versions):

```yaml
# dispatch input
      lanes:
        type: choice
        default: velnor
        options: [velnor, github, both]

# inline matrix (one line in real YAML)
strategy:
  fail-fast: false
  matrix:
    config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
runs-on: ${{ matrix.config.runner }}

# compile-job env additions
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"   # merge with any existing RUSTFLAGS
  SCCACHE_GHA_ENABLED: "false"
  SCCACHE_CACHE_SIZE: 20G

# setup step order on compile jobs
checkout (persist-credentials: false) → rui314/setup-mold → sccache-action
(version v0.16.0, disable_annotations "false") → mise-action → cargo registry cache
```

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Lint workflows | `actionlint` | exit 0 |
| YAML parse | `python3 -c "import yaml,glob; [yaml.safe_load(open(f)) for f in glob.glob('.github/workflows/*.yml')]"` | exit 0 |

Operator-gated: `gh workflow run ci.yml -f lanes=velnor|github|both` (repeat
for `rust-nextest.yml`, `docs.yml`, `preview.yml`).

## Scope

**In scope**: `.github/workflows/*.yml`, `.github/actions/sccache-stats/`
(delete), other composites ONLY where a step order or input rename forces it,
`.github/workflows/AGENTS.md` / `.github/AGENTS.md` prose.

**Out of scope**: all Rust sources, xtask internals, docs content,
`mise.toml`/`rust-toolchain.toml`, PR #810 L2/L3 machinery (result-reuse,
proof graphs) — do not extend or remove it; release signing/attestation
composites' logic.

## Git workflow

**Operator decision (2026-07-18, answers setup-doc §12.5): jackin's program
branch is the existing head branch of
https://github.com/jackin-project/jackin/pull/810** — resolve it with
`gh pr view 810 --repo jackin-project/jackin --json headRefName`, check it
out, merge latest `main` into it if behind, and stack ALL of this plan's
work there as a `ci:`-prefixed commit series (`git commit -s`). PR #810
remains jackin's single program PR; do NOT create a `velnor-estate-standard`
branch in jackin. Where this plan's changes touch files #810 already
modified, integrate — #810's clarity/reuse direction (L1) and this plan's
standard are the same program; conflicts resolve toward the union of both.

## Steps

### Step 1: Default flip + input normalization

Every workflow with a `lanes` input: `default: velnor`, options ordered
`[velnor, github, both]` (preview.yml currently `[github, velnor, both]`,
default github). Every `LANES: ... || 'github'` fallback expression becomes
the canonical inline matrix (delete `matrix-setup` jobs; consumers get the
matrix pasted in; `runs-on: ${{ matrix.config.runner }}` without the extra
`fromJSON`).

**Verify**: `grep -rn "|| 'github'\|matrix-setup" .github/workflows/` → none;
`grep -rn "default: github" .github/workflows/` → none.

### Step 2: Ubuntu policy

All `ubuntu-latest`/`ubuntu-24.04` → `ubuntu-26.04` (in the matrix GitHub
record and any single-lane utility jobs). Delete the `macos-latest` job; if
it built a release artifact, replace with an Ubuntu `cargo-zigbuild` build of
the same target and note it in the PR; if its purpose is unclear → STOP.

**Verify**: `grep -rn "ubuntu-latest\|ubuntu-24.04\|macos" .github/workflows/` → none.

### Step 3: Standard cache stack

For every compile job: ensure sccache-action (pinned inputs `version: v0.16.0`,
`disable_annotations: "false"`), `SCCACHE_GHA_ENABLED: "false"`,
`SCCACHE_CACHE_SIZE: 20G`, mold step, and the `cache-cargo-registry` composite
(keep it — it is the registry cache). Delete every `Swatinem/rust-cache` step.
Delete raw `actions/cache` steps whose `path` is `target/` or `~/.cargo`
duplicating the composite; keep non-Rust caches (bun, xtask artifacts).
Delete the `sccache-stats` composite and its invocations.

**Verify**: `grep -rn "Swatinem\|sccache-stats" .github/workflows/ .github/actions/` → none;
`grep -rln "setup-mold" .github/workflows/` lists every workflow that compiles Rust.

### Step 4: Hygiene markers

`concurrency` on the 5 uncovered workflows (PR-triggered: cancel-in-progress
true). `timeout-minutes` on every job in the 2 uncovered files. The 7
uncommented `fetch-depth: 0`: add a one-line consumer comment or change to
default shallow — decide per case from what the job does with history
(`git log`/`git describe`/changelog ⇒ keep + comment; otherwise drop).
`persist-credentials: false` on checkouts of jobs that never push.

**Verify**: python timeout check per file (plan 047 step 5) → exit 0;
`grep -rn "fetch-depth: 0" .github/workflows/ | wc -l` — every remaining hit
has a comment on the same or preceding line.

### Step 5: AGENTS prose

Update the workflow AGENTS to: Velnor automatic default, three-lane dispatch,
ubuntu-26.04 pin, standard stack, no macOS.

**Verify**: no contradiction with steps 1–4.

## Test plan

Static gates above. Operator: three-lane dispatch on `ci.yml`,
`rust-nextest.yml`, `docs.yml`, `preview.yml`; `both` run must deploy docs /
publish preview exactly once (writer gating). §2.11 timing table
(cold/warm/no-change, both lanes) in the PR description.

## Done criteria

- [ ] actionlint + YAML parse exit 0; all step greps clean
- [ ] Automatic events resolve to Velnor lane (matrix default arm)
- [ ] Zero Swatinem / sccache-stats / macOS / ubuntu-latest
- [ ] Every job has timeout-minutes; concurrency in all 13 workflows
- [ ] Operator three-lane dispatches green (or deferred)
- [ ] No out-of-scope modifications

## STOP conditions

- Velnor lane fails on a capability/adapter error (likely candidates:
  deploy-pages parity, local composite resolution, xtask bootstrap assuming
  host node/unzip) → STOP, report exact step + error; runner-side fix
  (velnor plans 042) — never weaken the workflow.
- The macOS job's artifact has no obvious zigbuild equivalent → STOP (operator
  decision §12.4 of VELNOR_PROJECTS_SETUP.md).
- A Swatinem removal leaves a job measurably cold and the standard stack
  cannot be applied without restructuring PR #810 machinery → STOP.
- `cache-cleanup.yml` semantics conflict with the standard stack → STOP and
  describe.

## Maintenance notes

- jackin is L2/L3 owner (result-reuse, perf audit gate). After this lands,
  those become candidates to feed velnor-tools audit-ci perf mode — deferred.
- Reviewer: scrutinize writer gating on docs deploy + preview publish + release
  signing; and that merged RUSTFLAGS didn't drop existing flags.
