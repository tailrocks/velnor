# G3 skills/plugin adapter — independent early review

Observed `2026-09-19T17:07:00Z` UTC. Read-only review. The implementation
worktree `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-skills-adapter`
is clean at `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` (`origin/main`). That
revision is the apt-sentinel merge; its only source changes are `crates/velnor-
workflow/src/apt.rs` and `src/primitives/release.rs`. No Skills implementation
is present at this snapshot: `UnitKind` has no `Skills`, both scan pipelines
have no Skills detector, and no central Skills validator exists. This is an
early design review, not approval of a committed implementation.

## Runtime and reviewed inputs

- Reviewer runtime: `gpt-5.6-luna`, reasoning `max`; `rtk 0.49.0`.
- Prior category evidence: `G0/skills-adapter/report.md`.
- Velnor source revision: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Skills repository snapshots reviewed in isolated clones:

| repository | default-branch SHA |
|---|---|
| `tailrocks/tailrocks-typescript-skills` | `f434715f7c664af431f0be62982aa379400102de` |
| `tailrocks/tailrocks-skill-authoring-skills` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` |
| `tailrocks/tailrocks-rust-skills` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` |
| `tailrocks/tailrocks-roadmap-skills` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` |
| `tailrocks/tailrocks-pull-request-skills` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` |
| `tailrocks/tailrocks-open-source-skills` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` |
| `tailrocks/tailrocks-macos-skills` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` |
| `tailrocks/tailrocks-code-quality-skills` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` |

## Review findings

### F-01 — blocker: no implementation exists at the claimed source revision

`crates/velnor-workflow/src/lib.rs:561-610` and
`src/s2/mod.rs:537-586` enumerate only the existing nine unit kinds.
`src/scan/mod.rs:9-57` and `src/s2/scan/mod.rs:9-57` register only the legacy
detectors. `src/scan/file_walk.rs:212-240` only has the tests/benches boundary;
there is no semantic embedded-template boundary. No `skills` symbol is present
in the source tree. The branch cannot currently produce a Skills unit or
validate any of the eight repositories.

The implementation must be reviewed again at its exact committed head. Do not
accept a plan, fixture-only change, or generated YAML snapshot as the adapter.

### F-02 — detector must distinguish an invalid Skills candidate from an ordinary repo

The detector must have a structural candidate phase followed by strict
validation. If it claims only when the complete tuple is already valid, a
malformed catalog/frontmatter/provider manifest falls through to the generic
detectors and can produce no unit or a docs-only unit. That recreates the bug
class the adapter is meant to remove.

Recommended candidate evidence is at least the root `catalog.json` plus a
`skills/` tree containing a `SKILL.md`, or the equivalent catalog plus provider
metadata. Once a candidate is identified, missing/malformed catalog, skill,
docs, or provider files must be a hard scan error. A random unrelated
`catalog.json` must not claim a Skills repository merely because it has a field
named `skills`.

### F-03 — shared contract required; paired legacy/S2 copies must not diverge

The legacy and schema-2 scanners, unit enums, primitive registries, watch
graphs, and IR renderers are duplicated. Adding two hand-copied validators or
two independently evolving `skills.rs` implementations would create a second
generator framework in practice.

Keep one pure Skills contract/validator and thin adapters for each scan context
and provider model. Test both pipelines against the same fixtures. The new
kind must be wired exhaustively in both trees: enum parsing/id/label/group,
detector order, pipeline id/registry, watch graph, provider admission,
prepared-tool handling, IR/job names, config validation, and every exhaustive
match. A compile pass alone is insufficient; generated output must contain a
real Skills unit job and non-empty validation commands.

### F-04 — proposed template boundary is too narrow unless it covers actual nested templates

The eight repositories contain manifests/templates in both
`skills/<skill>/templates/**` and helper-owned paths such as
`scripts/macos-visual-qa/templates/**` and
`scripts/web-visual-qa/templates/**`. The latter include TypeScript, Swift, and
shell template/test assets. A boundary that only filters
`skills/<skill>/templates/**` is insufficient for helper inventory; blindly
checking every `scripts/**/*.ts` also executes template/test-support source.

Use one semantic path classifier, but define its scope by ownership:

- generic language detectors skip only embedded template/example manifests;
- real helper manifests and helper source outside those boundaries remain
  executable inputs;
- helper-owned `templates/**`, fixture/testdata, and explicitly non-executable
  support files are validated as data, not run as helpers;
- root `templates/**` and ordinary non-Skills repository templates are not
  globally hidden.

Apply the filtered file view to every detector that iterates `context.files`,
not only `files_named`: Docker, Swift, Docs, Homebrew, and future detectors must
not rediscover a nested template manifest. Add fixtures proving a real helper
manifest remains visible while a Cargo/package/Swift/Docker manifest under an
embedded template does not become a language unit.

### F-05 — executable-helper ownership is not defined

The current repositories have about 51 TypeScript files plus shell/Swift assets
under `scripts/`, but no root package manifest or lockfile. Some scripts are
entrypoints, some are helper libraries, some are test support, and some are
templates. The adapter must define an inventory rule or typed manifest for
which helpers receive syntax/build checks. It must not silently validate only
`generate-docs.ts`, and it must not execute arbitrary mutation/network helpers
from a pull request.

The Skills unit should run a pinned, bounded central validator plus deterministic
syntax/build checks for the declared executable helper set. Run the existing
`generate-docs.ts` helper in a temporary copy and compare output; never run it
against the checkout. Keep platform-specific visual helpers as explicit
capability-labeled checks rather than falsely claiming Linux execution proof.

### F-06 — frontmatter validation must be strict and independent of the helper parser

The checked-in `scripts/generate-docs.ts` has a hand parser that silently skips
malformed lines, overwrites duplicate keys, and only requires `name` and
`description` (see its `parseSkill` function). Successful docs generation is
therefore not proof of the frontmatter contract.

The central validator must parse the YAML frontmatter with duplicate-key errors
and enforce, at minimum: exact path/name/catalog identity; non-empty
description; argument-hint type; Apache-2.0 license; boolean
`user-invocable`; optional boolean `disable-model-invocation`; one document;
and a valid delimiter/body. Decide and test unknown-key policy. Add negatives
for missing delimiters, duplicate keys, wrong scalar types, blank required
values, name mismatch, unsupported license, and malformed YAML.

### F-07 — catalog/provider metadata needs structural and version-relational checks

Validate strict JSON shape and path safety: root catalog object, non-empty
unique skill names, no traversal/control characters/case-collision ambiguity,
one matching `skills/<name>/SKILL.md`, and deterministic catalog order. Compare
`docs/index.json` and generated docs to that exact order.

Validate root `plugin.json` and all provider manifests, including names,
skills-root paths, marketplace source `./`, and provider version agreement.
Do not hardcode the observed `0.28.0`; require valid versions to agree with the
marketplace entry and each provider while allowing a future reviewed bump. Root
plugin metadata intentionally has no version. Derive identity from metadata or
the repository contract; do not add owner/repository-name branches for these
eight repositories.

### F-08 — references need a real local-link resolver and explicit placeholders

Resolve local Markdown destinations from every source skill/reference document,
normalizing `.`/`..`, fragments, queries, percent-encoding, and path separators;
reject escapes and missing tracked targets. Ignore only deliberately external
schemes and anchors. The two observed roadmap examples use
`research/<topic>/...` placeholders. They need a generic, documented
placeholder/example marker or syntax rule; do not add two repository/path
exceptions. Arbitrary missing links must remain failures.

Validate links in nested reference/template documents as well as `SKILL.md`.
Generated docs' rewritten external links should be checked from generated
output separately, not mistaken for source-local references.

### F-09 — generated-doc drift is a required gate, not a docs lint check

The checked-in generator changed committed definitions in seven of the eight
clones when run in isolated research copies (TypeScript four, Rust one, macOS
two); three clones are dirty only from that generation. The gate must detect
this drift by running the repository helper in a disposable copy, comparing
every generated byte, and checking repeat-run stability. It must also run the
central metadata/reference/helper/template validator. A markdownlint-only or
catalog/docs-index-only unit remains a no-op for this category.

The temp-copy operation needs bounded execution, no network/secrets, preserved
tracked input, and a failure if the helper writes outside its generated docs
surface. Repair the seven stale snapshots before claiming all-eight migration;
do not weaken the drift check to accept them.

### F-10 — Bun/toolchain contract must be pinned centrally

The eight repositories do not pin a root Bun version. A generated command that
uses runner `bun`/latest is not reproducible. Choose one typed central Bun
runtime/setup with its version and artifact identity recorded in generation
inputs, or add an explicit reviewed metadata contract; never silently inherit
the host image. The command must be available on both eligible providers and
must not turn macOS-only helper content into a false macOS requirement.

### F-11 — preserve real helper manifests without creating duplicate units

If a real executable `package.json`/`Cargo.toml` exists outside an embedded
template boundary, the generic detector must retain it or the Skills contract
must explicitly own it and prove equivalent checks. Do not globally suppress
all nested manifests below `skills/` or `scripts/`. Conversely, manifests in
embedded templates must not trigger Rust/Bun language-wide invariants. Define
the ownership rule and assert it with paired fixtures: helper manifest,
template-only manifest, and ordinary non-Skills `templates/` manifest.

### F-12 — acceptance evidence must cover all eight without repository branches

Before migration after G2, require both legacy and schema-2 scans of all eight
exact SHAs, each producing one meaningful Skills unit (plus only explicitly
owned real executable units), no docs-only fallback, no template-induced Rust/
Bun/Swift units, and non-empty validation commands. Run generated workflow
render/parse checks and inspect the resulting aggregate/required-check graph.

Fixtures must cover malformed catalog/frontmatter/references, missing provider
metadata, stale generated docs, malformed helper syntax, path traversal,
duplicate keys, duplicate catalog names, template manifests, helper manifests,
and a non-Skills repository with ordinary templates. At least one synthetic
fixture must use a non-Tailrocks repository/plugin name to prove no hardcoding.

## Preserved behavior requirements

- Keep all existing catalog-declared skills and order; each matching skill gets
  frontmatter/docs/reference/template validation.
- Keep provider metadata files and current helper scripts as repository-owned
  assets; central generation supplies verification, not a replacement plugin
  format or copied per-repository workflow framework.
- Keep the generated-doc helper as the source of generated docs; run it in a
  temp copy and compare rather than reimplementing it or mutating CI checkouts.
- Keep macOS visual/design material as metadata/template content unless a
  helper's explicit contract proves a native workload. The Skills unit itself
  can remain Linux-portable.
- Do not add Velnor packaging/release requirements to these unrelated plugin
  repositories.

## Gate result

**Not approvable at `abe9ad82`.** The source has no adapter implementation, and
the design must close F-02, F-04, F-05, F-06, F-08, F-09, F-10, and F-11 before
the exact committed diff/tests review. Re-review the implementer's committed
head only after both scanner pipelines, central validation, template/helper
ownership, and all-eight fixture/clone evidence exist.

## Live implementer diff observed after initial review

The worktree later became dirty with an uncommitted schema-2-only attempt:

- `s2/mod.rs`, `s2/scan/mod.rs`, `s2/scan/skills.rs`, and schema-2 primitive
  files only; legacy `src/` remains unchanged.
- `cargo check -p velnor-workflow` fails before tests:
  `skills.rs:67` calls `unit` with four arguments instead of the required five,
  and `skills.rs:486` leaves the `BTreeMap` value type ambiguous. There is also
  an unused `Component` import.
- The detector's `has_plugin_marker` only looks for one of four provider files
  (`skills.rs:95-99`). A repository with catalog/skills/root metadata but all
  provider files missing can still fall through as an ordinary/no-op scan,
  violating F-02.
- `parse_frontmatter` (`skills.rs:447-504`) is still a custom line parser: it
  overwrites duplicate keys, accepts unknown keys, does not parse YAML types,
  and recognizes only a narrow folded-string subset. It does not satisfy the
  strict malformed-frontmatter contract in F-06.
- JSON parsing uses ordinary `serde_json::from_str`; duplicate JSON keys are not
  rejected. Catalog/provider duplicate-key negatives remain unhandled.
- `validate_links` (`skills.rs:335-381`) skips every target containing `<` or
  `>` and every link on a line containing `<conclusion-`. That is a broad silent
  bypass, not an explicit placeholder classification. It also does not decode
  percent escapes or parse Markdown destinations robustly.
- `template_files` (`skills.rs:404-415`) only filters
  `skills/<name>/templates/**`; helper-owned `scripts/**/templates/**` remain in
  `HELPER_SYNTAX_COMMAND`, which builds every `scripts/**/*.ts` including
  template/test-support code. This violates F-04/F-05 and leaves ownership
  undefined.
- The generated unit hard-codes shell commands (`DOC_DRIFT_COMMAND` and
  `HELPER_SYNTAX_COMMAND`) instead of exposing a typed central validator and
  pinned Bun contract. No exact Bun version/artifact identity is recorded.
- No detector/primitive integration tests or all-eight clone scans were added;
  only two frontmatter/path helper tests exist in the untracked module.

This live state strengthens the baseline gate result: do not commit or migrate
the schema-2-only attempt as a Skills adapter until the compile failure,
legacy/S2 parity, strict parser, candidate boundary, helper ownership, pinned
runtime, and negative fixtures are addressed and independently reviewed.
