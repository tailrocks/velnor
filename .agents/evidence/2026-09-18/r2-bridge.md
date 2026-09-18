# R2 bridge evidence: provider-schema fork alongside schema-1

Branch: `fix/d2-bridge-r2` (base `a2840748`, merge-base == origin/main HEAD)
Commits (signed, `git commit -s`):
- `eb0303a3` feat(workflow): bridge provider-schema pipeline alongside schema-1 (R2)
- `73356c2c` chore(ci): regen generator state for R2 bridge sources
PR: https://github.com/tailrocks/velnor/pull/924 (open, not merged)

## What R2 implements

- Schema dispatch in `run_from_env` (lib.rs +6 lines): schema==1 falls
  through to the old RunnerMode path verbatim; schema==2 renders through
  the provider pipeline in `crates/velnor-workflow/src/s2/` (42 files).
- s2 = d2a (`d9b9ce45`) provider-schema core + 3-provider fanout + strict
  results, with main drift carried over: head-candidate rendezvous,
  branch-scoped scheduled checks, renovate token-via-job-env gating,
  producer revision verification (default-branch guard, verify-before-create,
  shell-parse/execution tests), unit-dependency/admission records,
  prepared-tools records.
- VelnoR tree stays schema-1; D19 pin unchanged (`a6fa8d4a`). Only tree
  regen: the generator-state `scan` digest (new sources are new scan
  inputs); all 21 rendered files byte-identical.
- d2a clippy debt paid in s2: d2a carries 41 warnings (pedantic gate);
  s2 is warning-free (mechanical fixes + shape-preserving allows with
  reasons; `as_str(&self)` kept: by-value breaks 12 fn-pointer sites).
- Port gaps closed: `impl Cli::parse_args` + `parse_clap` (s2 tests call
  them), `need_records` helper, `pub(crate)` on root-private-equivalent
  structs, orphaned `#[expect]` removed.

## Render-identical proof (schema-1: EMPTY diff)

Binaries stamped identically (`--revision a2840748…`, `--closure
99979a82…`). Script `/tmp/r2render.sh`; targets under `/tmp/r2render/targets`.

| target | result |
|---|---|
| velnor tree (`/tmp/r2-target-velnor`) | IDENTICAL (21 files) |
| fx-check-profiles-workspace | IDENTICAL (12 files) |
| fx-docker-multiarch (`--runners github`, as its contract test) | IDENTICAL (6 files) |
| fx-polyglot | IDENTICAL (16 files) |
| fx-swift-ffi-consumer | IDENTICAL (13 files) |
| fx-synthetic-release | IDENTICAL (10 files) |
| fx-synthetic-workspace | IDENTICAL (12 files) |
| fx-release-bindings (config-level, not a render target) | IDENTICAL-REJECTION |
| fx-versioned-tool (config-level, not a render target) | IDENTICAL-REJECTION |
| jackin-shaped (rust workspace + nextest e2e profile + bun/node + docker + swift + docs) | IDENTICAL (13 files) |
| chainargos-shaped (gradle + node + docker + docs) | IDENTICAL (12 files) |

`ALL-RENDER-IDENTICAL`.

Note: the schema-1 `jobs = [...]` lane opt-out named by the
runners=both/swift error text does not exist as a `UnitSection` key
(stale message); swift forces the github lane, so jackin-shaped uses
runners=github with the full kind set.

## Schema-2 fidelity (bridge s2 vs d2a binary, fixtures-s2/polyglot)

- Bridge renders 16 files, exit 0; base binary rejects the schema-2
  config at its schema gate (`unknown field selectors`). Dispatch
  verified in both directions.
- s2-vs-d2a diff: 695 added lines, all drift ports (unit-dependency
  record steps, admission inputs, hardened cache sweep, cron quoting,
  rendezvous comments); removals are 67 pin-stamp lines (d9b9ce45 →
  a2840748, paired 1:1) + 35 drift-replaced lines, each paired with its
  replacement; state-file digest rows follow content. Zero d2a content
  lost.

## Gates (clean clone `/tmp/r2-clean` @ `73356c2c`)

- `cargo build --locked -p velnor-workflow --bin velnor-workflow`: ok
- `--plain --dry-run --default-branch main .` → `0 files would change`
- `--plain --check --default-branch main --pin-build .` → `Generated files are current`
  (bare `--check` needs the pinned binary provisioned, same as d2a)
- `mbx fmt -- --check`: clean
- `mbx clippy --locked --profile test --all-targets --all-features -D warnings`: clean
- `mbx nextest run --locked --all-features`: 1610 passed / 0 failed
- `cargo check --workspace --all-targets`: 0 errors
- contract crate: 6 passed / 0 failed

## CI shape

Same unit gates as main (`project.toml` velnor-workflow unit: fmt +
nextest + clippy `-D warnings`); no workflow files change, so the
policy/rendezvous lanes see the pin-unchanged, render-stable shape.
Green expected via the head-candidate path.

## Return

- Branch: `fix/d2-bridge-r2` (pushed to `origin`)
- Tip: `73356c2c` (bridge `eb0303a3` + state regen)
- PR: https://github.com/tailrocks/velnor/pull/924
- Tests: 1610 passed / 0 failed; dry-run=0, check=0 in a clean clone
