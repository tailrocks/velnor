# Fixed32 legacy-path map

Read-only investigation. Inputs: `G0/fleet/configs.tsv` (fixed32 inventory),
each configured repository's `.github-gen/velnor-workflow.toml` at the TSV
`default_sha`, and checked-in generated workflows in the local evidence
checkouts. Generator source was inspected at
`dual-lane-generator` commit `122fd50a60f55c66f34bed1ab035233a0d4b5744`;
the owner checkout is unchanged. No source or generated files were edited.

## Result

The blanket statement “schema 1 is unused” is false:

| fixed32 row | count | active path |
| --- | ---: | --- |
| `schema = 2` | 1 | `tailrocks/velnor` |
| `schema = 1` | 22 | legacy generator/config/renderer path remains consumed |
| no generation config | 9 | no Velnor generator dispatch |

The count is independently reproducible from `G0/fleet/configs.tsv:2-33`
(`awk -F '\t' 'NR>1 {counts[$6]++} END {for (key in counts) print key, counts[key]}'
configs.tsv`). The schema-1 rows are therefore a removal dependency even where
one particular legacy feature is not emitted by a given consumer.

Two separate candidate facts must not be conflated:

1. The schema-1 candidate producer in
   `crates/velnor-workflow/src/primitives/ir.rs:1742-1750,1783-1863,4543-4546,4898-4908`
   is owner-only: repository must be `tailrocks/velnor` and a Rust unit must
   be rooted at `crates/velnor-workflow`. No current schema-1 fixed32 row owns
   that crate. Thus no current schema-1 consumer is proven to emit that
   producer.
2. The checked-in `tailrocks/velnor` tree is schema 2, but its generated
   workflows at `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` still pin
   `fdeed261bd2247a38db6922a7726cd45d3d6f31e` and emit the *pre-122fd schema-2*
   unit producer: `.github/workflows/ci-pr.yml:1113` passes
   `candidate_publish: true`, and `.github/workflows/ci-unit-rust.yml:559-634`
   contains `Prepare candidate generator product` plus the unit upload. This
   is an active stale generated path until the reviewed source product is
   published, adopted as the config pin, and regenerated. It is not evidence
   that the schema-1 producer is active.

APT is definitely still legacy-consumed. `tailrocks/velnor-apt` is schema 1
at `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`, declares `[release] kind =
"apt"`, and its checked-in `release.yml` runs `apt-resolve-commit`, `apt-fetch`,
`apt-verify`, and `apt-publish` (`velnor-apt/.github/workflows/release.yml:96-135,223-240`).
That surface is rendered by the schema-1 `primitives/release.rs` path. In
contrast, `tailrocks/holla-apt` is schema 1 but only declares the apt profile
and `files = ["ci-unit-docs.yml"]`; it has `NO_WORKFLOWS_REQUIRED.md` and no
`[release]` section (`G0/distribution-consumers/clones/holla-apt/.github-gen/velnor-workflow.toml:1-24`).
It is not an APT feed publisher.

## Fixed32 affected repositories and dispatch

Inventory source: `G0/fleet/configs.tsv:2-33`. Actual configs were queried at
their TSV heads with:

```sh
gh api "repos/<owner>/<name>/contents/.github-gen/velnor-workflow.toml?ref=<default_sha>" \
  --jq .content | tr -d '\n' | base64 --decode
```

Schema 2 (1):

- `tailrocks/velnor` — `providers = ["github-hosted", "velnor"]`, both
  automatic, both default dispatch providers; native release contract for
  `velnor-runner` (`velnor/.github-gen/velnor-workflow.toml:1-27,65-77`).

Schema 1 with `runners = "both"` (15):

- `tailrocks/velnor-apt`
- `tailrocks/parallax`
- `tailrocks/tracing-request-level`
- `tailrocks/termrock`
- `tailrocks/termpane`
- `tailrocks/schemalane`
- `tailrocks/ruxel`
- `tailrocks/pg-bigdecimal`
- `tailrocks/parallax-telemetry-playground`
- `tailrocks/homebrew-tablerock`
- `tailrocks/homebrew-ruxel`
- `tailrocks/homebrew-parallax`
- `tailrocks/homebrew-holla`
- `tailrocks/holla-apt`
- `tailrocks/holla`

These retain legacy manual `github|velnor|both` dispatch semantics. Most make
automatic events GitHub-only; `velnor-apt`, `holla-apt`, and
`parallax-telemetry-playground` declare automatic `both` (the exact rows and
heads are in TSV lines 3-18). A representative actual consumer is
`G0/rust-consumers/research/parallax/.github-gen/velnor-workflow.toml:1-17`;
its generated `ci-pr.yml:60-78` uses the pinned owner action and legacy
`VELNOR_LANES`/`runner` vocabulary.

Schema 1 with `runners = "github"` (7):

- `tailrocks/tablerock`
- `jackin-project/jackin`
- `jackin-project/jackin-agent-smith`
- `jackin-project/homebrew-tap`
- `jackin-project/jackin-the-architect`
- `jackin-project/jackin-sentinel`
- `jackin-project/jackin-role-action`

No config (9; no generator dispatch):

- `tailrocks/homebrew-velnor`
- `tailrocks/tailrocks-typescript-skills`
- `tailrocks/tailrocks-skill-authoring-skills`
- `tailrocks/tailrocks-rust-skills`
- `tailrocks/tailrocks-roadmap-skills`
- `tailrocks/tailrocks-pull-request-skills`
- `tailrocks/tailrocks-open-source-skills`
- `tailrocks/tailrocks-macos-skills`
- `tailrocks/tailrocks-code-quality-skills`

Release-bearing configured rows are only:

- `tailrocks/velnor`: schema-2 `native` release (not an APT feed publisher).
- `tailrocks/velnor-apt`: schema-1 `apt` release; current APT migration
  blocker.
- `jackin-project/jackin`: schema-1 `tasks` release with two named macOS
  jobs; not APT, but another schema-1 release obligation that prevents
  deleting the legacy release parser. Its `[release]`/`[[release.job]]` data is
  at the TSV head `665f7e3735c1f76ce5ee8a9e27c676381a474cfb`.

The Homebrew/docs `kind` values visible in unit rows are not `[release]`
contracts. They do not make those repositories APT or release-primitive
consumers.

## Actual source seams

Dispatch is a bridge, not a schema migration:

- Root entrypoint calls `s2::dispatch::run_if_s2()` and otherwise falls into
  schema 1 (`crates/velnor-workflow/src/lib.rs:5685-5701`).
- The bridge recognizes runtime commands, `--providers`, or a local config
  with `schema = 2`; otherwise it yields to schema 1
  (`crates/velnor-workflow/src/s2/dispatch.rs:36-59,68-101,120-133`).
- Schema-1 config is hard-coded to schema 1 and owns `runners`, `automatic`,
  `velnor_labels`, and `default_dispatch_runner`
  (`crates/velnor-workflow/src/config/mod.rs:25-31,170-190,230-244,1784-1795`).
- Schema-2 config is hard-coded to schema 2; its workflow contract is
  provider sets and provider selectors, and explicitly does not read schema-1
  lane strings (`crates/velnor-workflow/src/s2/config/mod.rs:26-33,172-202`).
- Promotion preserves both branches: schema-2 trees use V2 and schema-1
  trees use the original renderer; the comments explicitly say schema-2
  rejects legacy fields such as `velnor_labels`
  (`crates/velnor-workflow/src/promote.rs:279-305`).

The schema-1 candidate producer is source-reachable through the old fallback,
but owner-gated as described above. The current schema-2 source has already
removed the unit-owned candidate producer from its intended path: the PR
aggregate emits an owner-only, dependency-free `candidate-bootstrap`
(`crates/velnor-workflow/src/s2/primitives/ir.rs:3114-3194`), whose body checks
out base setup action before the head and builds the head product
(`crates/velnor-workflow/src/s2/primitives/ir.rs:5188-5308`). The existing
policy consumer expects that job and validates repository IDs, head identity,
run/job identity, revision, closure, digest, and tokenless execution
(`crates/velnor-workflow/src/s2/mod.rs:4540-4646`). The checked-in owner output
has not adopted this source yet because it remains pinned to `fdeed…`.

## APT: current versus intended schema-2 seam

Current schema-1 path:

- Schema-1 `Release` accepts APT-specific fields (`apt_arches`, feed URL,
  keyring, signer and secret names) in its primitive schema
  (`crates/velnor-workflow/src/primitives/release.rs:110-155`).
- Its APT completeness gate requires package/binary/source/consumer,
  manifest schema, signer, both secret names, and feed URL
  (`primitives/release.rs:919-939`). It renders the full APT feed workflow at
  `primitives/release.rs:4242-4291`.
- Its runtime dispatch exposes the APT command family
  (`crates/velnor-workflow/src/runtime.rs:2961-2989`), and delegates the
  typed validation/operations to shared `crate::apt` (`runtime.rs:3005-3065`;
  `apt.rs:511-634`).
- `velnor-apt`’s actual config carries the full contract (fingerprint,
  secrets, keyring, origin, feed URL, retention) and generated workflow
  consumes it (`velnor-apt/.github-gen/velnor-workflow.toml:32-50`;
  `.github/workflows/release.yml:96-135,223-240`).

Intended schema-2 seam:

`s2::dispatch` → `s2/config::ReleaseSection` →
`s2/primitives/release.rs` → schema-2 runtime/typed APT implementation.

It is not equivalent yet:

- `s2::config::ReleaseSection` has package/consumer fields but no APT signer,
  secret, keyring, origin, identity, feed URL, architecture, or retention
  fields (`crates/velnor-workflow/src/s2/config/mod.rs:339-398`).
- The schema-2 release primitive accepts only `package` and
  `consumer_repository` for `kind = "apt"` and renders a generic
  `verify-feed`/`update-feed` package-feed workflow
  (`crates/velnor-workflow/src/s2/primitives/release.rs:120-155,854-874,4167-4192`).
- Schema-2 runtime has only generic `verify-feed`/`update-feed`; it has no
  `apt-resolve-commit`, `apt-fetch`, `apt-verify`, `apt-publish`, pointer,
  channel, or deploy-guard commands (`crates/velnor-workflow/src/s2/runtime.rs:3052-3073,3714-3760`).

Therefore an APT worker must not merely replace `src/primitives/release.rs`
references or rename legacy `runners` flags. The schema-2 migration must first
carry the existing typed APT contract and command semantics into the schema-2
surface. Reuse `crate::apt` and one common runtime command implementation;
do not create a second, weaker APT publisher beside the old one. The same
typed renderer must preserve the current verify-before-mutate flow, two-arch
coherence, signing identity, source/consumer binding, retention, rollback
pointer, deploy guard, and GitHub-only mutation gate.

The schema-1 `tasks` release is a second typed migration obligation: schema 1
has `ReleaseJobSection`/`[[release.job]]`
(`crates/velnor-workflow/src/config/mod.rs:370-405,476-480`), while schema 2
has no task-job rows in `ReleaseSection` and its release kinds omit `tasks`
(`crates/velnor-workflow/src/s2/config/mod.rs:339-398,2121-2131`). Do not
classify `jackin` as migrated until this named-task release contract is either
ported once into schema 2 or intentionally converted to an equivalent typed
surface with parity tests.

## Shortest complete migration/removal sequence

1. **Keep the old pin while admitting source.** Land/admit the independent
   candidate producer (`122fd…`) with no generated-output hand edit. Keep
   `fdeed…` as the consumer/runtime pin until its replacement product is
   published and reviewed. This avoids a source/config cycle and preserves the
   current APT runtime product.
2. **Migrate the owner first.** Publish the reviewed candidate/runtime product,
   update `tailrocks/velnor`'s generator pin, and regenerate its aggregate so
   `candidate-bootstrap` is checked in. Verify policy consumes the independent
   product before touching consumers.
3. **Complete schema-2 capability seams once.** Add schema-2 typed fields and
   renderer/runtime support for the exact APT contract above, and port the
   schema-1 named-task release contract needed by `jackin`. Add focused parity
   tests against the current generated APT and tasks workflows. Share the
   existing `apt.rs` operations; do not fork them.
4. **Migrate configured consumers in bounded batches.** Convert the 22 schema-1
   configs to schema-2 provider sets/selectors, preserving each repo's runner,
   automatic-event, dispatch, profile, workflow-file, unit, release, and
   policy obligations. `velnor-apt` and `jackin` are release-parity gates;
   `holla-apt` remains a no-workflows/docs surface. Regenerate from clean
   shallow checkouts and verify ownership/provenance, not hand-written YAML.
5. **Prove the fleet boundary.** Re-query all fixed32 default heads. Require
   zero schema-1 configs, zero legacy `runners`/`automatic` config fields in
   active surfaces, no old `candidate_publish` unit producer in generated
   owner workflows, and release parity for APT/tasks. The nine no-config rows
   remain no-config.
6. **Remove the bridge and legacy implementation in one final breaking slice.**
   Delete the schema-1 dispatch/parser/promotion branch and the now-unreachable
   schema-1 candidate/release renderers. Remove legacy generated outputs only
   through the schema-2 renderer. Then run full generator/policy/release parity
   tests and re-scan fixed32. Do not delete `primitives/release.rs` while
   `velnor-apt` or `jackin` still depend on it.

**First bounded implementation task after explicit assignment:** source-only
schema-2 APT parity. Extend the schema-2 release/config contract with the
validated APT fields, expose the existing APT operations through the schema-2
runtime without duplicating `apt.rs` logic, render the existing verify/fetch/
publish/deploy graph from `s2/primitives/release.rs`, and add parity tests
against the checked-in `velnor-apt` workflow. Do not touch fixed32 configs or
generated YAML in that task; tasks-release parity for `jackin` is the next
separate bounded task.

## Reproduction commands

```sh
# Inventory counts
awk -F '\t' 'NR>1 {counts[$6]++} END {for (key in counts) print key, counts[key]}' \
  /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/configs.tsv

# Owner generated output at the fixed32 head
git -C /Users/donbeave/Projects/tailrocks/velnor-project/velnor \
  rev-parse HEAD
rg -n 'candidate_publish|Prepare candidate generator product|candidate-bootstrap' \
  /Users/donbeave/Projects/tailrocks/velnor-project/velnor/.github/workflows

# APT generated output at the fixed32 head
rg -n 'apt-resolve-commit|apt-fetch|apt-verify|apt-publish|runner:' \
  /Users/donbeave/Projects/tailrocks/velnor-project/velnor-apt/.github/workflows/release.yml

# Source seam checks
rtk rg -n 'run_if_s2|wants_s2|dir_is_schema2' \
  crates/velnor-workflow/src/lib.rs crates/velnor-workflow/src/s2/dispatch.rs
rtk rg -n 'candidate_publish|candidate-bootstrap' \
  crates/velnor-workflow/src/primitives crates/velnor-workflow/src/s2
rtk rg -n 'apt-resolve-commit|apt-fetch|apt-verify|apt-publish|verify-feed|update-feed' \
  crates/velnor-workflow/src/runtime.rs crates/velnor-workflow/src/s2/runtime.rs \
  crates/velnor-workflow/src/primitives/release.rs crates/velnor-workflow/src/s2/primitives/release.rs
```

Source checkout status at investigation end: `dual-lane-generator` branch
`codex/github-first-generator`, commit `122fd50…`, clean; no generated edits.
