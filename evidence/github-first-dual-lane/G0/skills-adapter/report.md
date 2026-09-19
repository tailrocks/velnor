# G3 skills/plugin adapter — G0 read-only evidence

Status: investigation only. No source-tree, remote, or fleet changes. Do not roll
this category out before G2. Velnor source was inspected at
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

## Runtime and scope proof

- Codex runtime: `/Users/donbeave/.codex/config.toml` has
  `model = "gpt-5.6-luna"`, `model_reasoning_effort = "max"`, and
  `[agents] default_subagent_model = "gpt-5.6-luna"`,
  `default_subagent_reasoning_effort = "max"`.
- `rtk 0.49.0`; source worktree stayed `main...origin/main` with no tracked
  changes. Untracked `velnor-github-first-dual-lane-goal.md` and the unrelated
  `G1/reviews/seed-pin.md` were preserved.
- Remote snapshots (`main`, `git ls-remote` on 2026-09-19):

| Repository | SHA | Skills | Refs | Templates | `scripts/` files |
| --- | --- | ---: | ---: | ---: | ---: |
| `tailrocks-typescript-skills` | `f434715f7c664af431f0be62982aa379400102de` | 12 | 64 | 14 | 82 |
| `tailrocks-skill-authoring-skills` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | 4 | 22 | 4 | 81 |
| `tailrocks-rust-skills` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | 15 | 84 | 14 | 82 |
| `tailrocks-roadmap-skills` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | 15 | 47 | 6 | 82 |
| `tailrocks-pull-request-skills` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | 6 | 14 | 1 | 82 |
| `tailrocks-open-source-skills` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | 5 | 15 | 0 | 82 |
| `tailrocks-macos-skills` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | 15 | 117 | 12 | 82 |
| `tailrocks-code-quality-skills` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | 12 | 38 | 4 | 82 |

Clones are isolated under `G0/skills-adapter/research/`, outside Velnor and the
user's existing trees. The three clones used to run `bun scripts/generate-docs.ts`
are intentionally dirty only in generated docs; the other five remain clean.

## Actual category contract

All eight repositories share this shape:

1. Root `catalog.json` with a unique `skills` array.
2. `skills/<catalog-name>/SKILL.md` with frontmatter (`name`, non-empty
   `description`, `argument-hint`, Apache-2.0 license, `user-invocable`), plus
   optional `disable-model-invocation`.
3. Generated `docs/index.json`, `docs/README.md`, and one `docs/skills/*`
   `index.md`/`definition.md` pair.
4. Root `plugin.json` (Antigravity metadata, no version), and provider metadata:
   `.codex-plugin/plugin.json`, `.kimi-plugin/plugin.json`,
   `.claude-plugin/plugin.json`, and `.claude-plugin/marketplace.json`.
   Provider versions are all `0.28.0`; names match the repository; Codex/Kimi
   point at `./skills/`; marketplace source is `./` and version is `0.28.0`.
5. Bundled `references/`, `templates/`, and TypeScript/helper scripts. There is
   no root package manifest, lockfile, workflow config, or `.github-gen` config.

The catalog, skill directories, frontmatter names, and docs index agree for all
8/8 repositories (12/4/15/15/6/5/15/12 skills respectively). The shared
`scripts/generate-docs.ts` is byte-identical in all eight (SHA-256
`d063fc40321c6b25c59ec6ea62195b45b463a51fdda98e0edc0073a3d77d2f2d`). Script
path comparison found 82 paths total, 81 common and byte-identical. The only
variant is `scripts/refresh-template-pins.ts`, absent from skill-authoring and
present in the seven repositories that carry its pin-refresh contract; do not
make this helper mandatory for every skill repository.

Relative source links are valid except two roadmap examples that are not meant
to resolve as checked-in files:

- `skills/tailrocks-idea/references/roadmap-item-format.md:189` uses
  `research/<topic>/README.md`.
- `skills/tailrocks-research/references/research-playbook.md:144` uses the
  illustrative `pure-rust-macos-ui/README.md` row.

The validator must require an explicit placeholder/example classification; it
must not silently ignore arbitrary missing links.

## Generated-doc drift found

Running the checked-in generator in isolated clones (`bun scripts/generate-docs.ts`)
was byte-stable for five repositories. It changed committed definitions in:

- TypeScript: `tailrocks-tanstack-project-audit`, `...-migrate`,
  `...-remediate`, `tailrocks-web-design`.
- Rust: `tailrocks-tui-design`.
- macOS: `tailrocks-macos-design`, `tailrocks-macos-visual-baseline`.

The regenerated definitions add current `version-policy.md`/`design-pipeline.md`
links that the remote HEAD docs omit. This is evidence that a docs-index/catalog
check alone is insufficient. The category gate needs a non-mutating generated
output drift check (or generate in a temporary copy and compare).

## Existing Velnor scan result

Command: `target/debug/velnor-workflow <isolated-clone> --runners github --dry-run --plain`.

| Repository | Current result | Failure/misclassification |
| --- | --- | --- |
| TypeScript | exit 0; one Bun unit | Claims `skills/tailrocks-tanstack-project-setup/templates/package.json` as executable `bun-your-app`; emits install/lint/typecheck/build/test for an example template. |
| Skill-authoring | exit 1 | No supported manifest/project shape. |
| Rust | exit 1 | Embedded `skills/*/templates/**/Cargo.toml` triggers the repository-level “no Rust toolchain pin” error. |
| Roadmap | exit 1 | No supported manifest/project shape. |
| Pull-request | exit 1 | No supported manifest/project shape. |
| Open-source | exit 1 | No supported manifest/project shape. |
| macOS | exit 1 | No supported manifest/project shape. |
| Code-quality | exit 1 | No supported manifest/project shape. |

The TypeScript false positive is concrete: generated commands `cd` into the
template and run `bun install` without a lockfile. This is not a valid category
verification unit.

## Why the scanner permits this bug class

- Legacy detector pipeline is fixed in `crates/velnor-workflow/src/scan/mod.rs:9-74`;
  schema-2 duplicates it in `src/s2/scan/mod.rs:9-78`. Neither has a skills/plugin
  detector.
- Both file walkers use tracked files, but `is_test_support_path` only excludes
  `tests`/`benches` (`src/scan/file_walk.rs:212-240` and the S2 mirror). It has no
  semantic embedded-template boundary.
- Node scans every `package.json` not under tests/benches; Rust scans every
  `Cargo.toml` not under tests/benches (`src/scan/node.rs`, `src/scan/rust.rs:1203-1212`).
  Thus a nested template manifest is treated as a production unit, and Rust's
  root-toolchain invariant aborts before any useful result.
- Docs only creates a unit when a markdownlint config exists
  (`src/scan/docs.rs:9-46`), so these repositories cannot accidentally receive a
  meaningful docs-only unit. Signals only knows cargo-deny/nextest/Renovate
  (`src/scan/signals.rs:6-25`).
- Unit kinds are duplicated in legacy/S2 (`src/lib.rs:561-610`,
  `src/s2/mod.rs:537-586`), with no skills kind. Per-kind rendering is likewise
  duplicated (`src/primitives/pipeline.rs:149-220`,
  `src/s2/primitives/pipeline.rs:148-219`).

## Central change contract (implementation later, not done here)

Recommended name: `UnitKind::Skills`, id `skills`, label `Skills / Plugins`,
root `.`. This is a single root unit per canonical skills/plugin repository; each
skill is a validated member, not a separate CI unit. Keep the unit Linux-portable
and provider-eligible; macOS visual/design content is metadata/template input, not
proof that this adapter needs a Darwin executor.

### Scanner

1. Add one detector in both pipelines (`src/scan/skills.rs` and
   `src/s2/scan/skills.rs`) and run it before generic manifest detectors. Claim
   only the canonical tuple: root `catalog.json` with unique non-empty skills,
   all matching `skills/*/SKILL.md`, `docs/index.json`, and the provider plugin
   manifests. Validate names/version/source relationships statically.
2. Add a shared semantic `is_embedded_template_path` to both file-walk modules,
   and make `files_named`/all manifest detectors skip manifests under
   `skills/<skill>/templates/**`. The skills detector still inspects those files
   as templates/examples, but they never become executable units or trigger
   language-wide invariants.
3. The detector's static contract must cover catalog/frontmatter, docs index and
   generated-doc drift, recursive local references, bundled helper syntax/build,
   and template manifest shape. Placeholder/example links require an explicit
   marker or documented syntax. Do not use Markdown lint alone.

### Primitive/IR/runtime

Update both legacy and schema-2 copies:

- `UnitKind`, `from_prefix`, `id_prefix`, labels/groups, lane/provider admission,
  and every exhaustive kind match (`src/lib.rs`, `src/s2/mod.rs`).
- Add `skills-plugin-pipeline` constant, registry/`PIPELINES` rows, and pipeline
  macro registration in `src/primitives/mod.rs` + `pipeline.rs` and S2 mirrors.
- Add Skills watch globs in `src/primitives/watch.rs` and S2: catalog, all
  `skills/**`, docs index/README/docs skills, provider manifests, and scripts.
- Add kind-file/IR recognition in `src/primitives/ir.rs` and S2. The new unit
  must require Bun setup for the helper checks, but the current repos do not pin
  a root Bun version. Resolve that as an explicit central policy/metadata task;
  do not silently inherit “latest”.
- Add regression fixtures for: canonical skill repo; template-only Cargo/package
  manifests; malformed catalog/frontmatter; stale generated docs; invalid local
  link; helper syntax failure; and a non-skills repo with ordinary `templates/`.

Preferred validation command is a central, pinned validator exposed by the
generator/runtime (for example `velnor-workflow validate-skills --root .`),
rather than copying eight repo-specific workflow files or trusting each helper's
ad-hoc behavior. It must return failure on any contract violation and perform
real catalog/frontmatter/reference/helper/template checks. If the implementation
chooses existing helpers, run `generate-docs.ts` in a temporary copy and compare
the resulting docs; never leave CI's checkout mutated.

## Bounded implementation/verification tasks

1. Implement and unit-test the two scanner detectors plus embedded-template
   boundary; verify all eight produce exactly one `skills` unit and no Bun/Rust/
   Swift units from nested templates.
2. Implement the paired legacy/S2 Skills primitive, Bun setup/version contract,
   watch graph, and IR/runtime kind handling.
3. Add the central validator and fixture tests listed above. Validate all eight
   current SHAs locally; repair the seven stale generated-doc snapshots and
   decide the two roadmap example-link markers before any migration PR.
4. Regenerate only isolated research clones as proof. Require byte-stable output,
   meaningful helper/template checks, and hosted dry-run/CI evidence first; defer
   Velnor-lane and macOS execution until the spec's G2/G4 gates permit it.

No per-repository raw workflow YAML, fleet rollout, or source-tree edits were
performed in this investigation.
