# velnor-workflow

Velnor-owned static GitHub Actions workflow generator and CI runtime.

The binary scans repository evidence without executing project code, renders
owned workflows plus `.github/ci/project.toml`, and runs the declared contract
on GitHub-hosted or Velnor runners. Runtime commands replace the former large
generated `run.sh`, `policy.sh`, and `release.sh` helpers:

```sh
velnor-workflow REPOSITORY --plain
velnor-workflow REPOSITORY --runners both --plain
velnor-workflow promote --rev HEAD
velnor-workflow plan --config .github/ci/project.toml
velnor-workflow run --config .github/ci/project.toml --scope affected
velnor-workflow test-crates --config .github/ci/project.toml
velnor-workflow policy --workflow-root . \
  --head-sha <pr-head-sha> --base-sha <base-sha> \
  --base-revision <sha of the validator the base branch pins> \
  --ruleset-contexts ci-required,DCO
velnor-workflow release verify-tag
```

`policy` is the trust validator the base branch's `ci-policy.yml` runs under
`pull_request_target` against a pull request's tree. It never compares the
tree with its own rendering: it reads the generator commit the tree declares
(`[generator] revision` in `.github-gen/velnor-workflow.toml`, the D19 pin),
proves the pin is reachable from the head and does not regress the base
branch's validator, regenerates the tree with the generator built at that pin
and requires a byte-identical result, and evaluates its own semantic rules
(only the entrypoint on `pull_request_target`; every self-hosted job gated;
every action SHA-pinned; the entrypoint on `contents: read` with no secrets;
the ruleset's required contexts emitted). Every rule prints `PASS`/`FAIL`
with a one-line reason. Bump the pin with `velnor-workflow promote --rev HEAD`
after the last generator change: it verifies the running binary renders with
the pin's own source closure, then stamps the pin and regenerates the whole
tree in a single commit; `--check` verifies the pinned generator renders the
tree.

Runtime commands are derived from scanned capabilities, not from config-supplied
shell arrays. GitHub-hosted execution is the automatic and omitted-dispatch
default; Velnor runs only when dispatch selects `velnor` or `both`. The binary
owns selection, dependency ordering, policy, release validation, and every
`.github/workflows/*.{yml,yaml}` file it emits. Foreign workflow bodies are
never imported. The ownership sidecar stays at
`.github/ci/.github-actions-generator-state`.

Repositories may pin their generation inputs in an optional
`.github-gen/velnor-workflow.toml` (`schema = 1`): the repository slug, runner
and branch overrides, scan excludes, policy switches, and `[[declare]]` render
primitives. Generation is a function of the scanned repository shape, this
config, and the generator revision (`GENERATOR_REVISION`); all three are
recorded in the ownership sidecar (`schema = 2`) and `--check` fails when they
no longer match the current run, even if every generated file is unchanged.

## Skills plugin scanning

Repository scanning supports these provider contracts:

- Portable Agent Plugins use root `plugin.json` and the fixed `skills/` root.
  This manifest takes precedence over `.codex-plugin/plugin.json`; a portable
  manifest's unknown fields, including `skills`, are reported unsupported and
  never change discovery. The parser checks known fields but does not claim
  complete JSON Schema validation. A non-object `extensions` field is reported
  and ignored under the spec; other invalid known-field types fail scanning.
- Antigravity uses its distinct root `plugin.json` provider and `skills/` root.
  Metadata is checked against Velnor's narrow local compatibility contract;
  full validation against the published Antigravity schema is not performed,
  and locally accepted fields may fail that schema. The local contract accepts
  `$schema`, `name`, `description`, `homepage`, `repository`, and `keywords`;
  any other top-level field is rejected. A root manifest without `$schema` is
  classified as Antigravity only when it fits that contract, includes a name,
  and has a direct `skills/<name>/SKILL.md`; generic manifests without this
  layout are not classified. Its directory-form skills require a description;
  `name` may be omitted from the skill frontmatter and then comes from the skill
  folder. Skills and resources beneath generic-walk exclusions such as
  `target/`, `dist/`, and `coverage/` are recovered only inside recognized
  component roots for plugin validation; they remain private to Skills scanning.
- A Codex sidecar is used only when no portable Agent Plugins root manifest is
  present. Velnor's Codex compatibility contract accepts its `skills` field as
  a relative path string or an array of relative path strings. Missing, null,
  or empty `skills` selects the default `skills/` root; a non-empty explicit list
  replaces that default. The pinned [manifest parser](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/core-plugins/src/manifest.rs#L45-L60)
  keeps `skills` optional, and the [Codex loader](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/core-plugins/src/loader.rs#L1015-L1041)
  defines the default and explicit-root selection and ignores a default root
  that is not a directory. Codex path parsing accepts `./.` and `././` for the
  plugin root. Bare `.` is rejected, while `./` contributes no explicit root
  and therefore selects the default when no other paths remain
  ([manifest path resolution](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/core-plugins/src/manifest.rs#L594-L659)).
  A non-directory root, including a root file named `SKILL.md`, produces an
  empty discovery
  ([filesystem walk](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/exec-server/src/local_file_system.rs#L761-L765)).
  Codex legacy discovery walks recursively
  through directory depth six, includes `skills/SKILL.md`, prunes hidden child
  directories, and does not prune `node_modules`
  ([discovery walk](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/ext/skills/src/loader/discovery.rs#L61-L79)).
  Velnor restores files omitted by its generic walk only inside the declared
  directory roots, applies configured excludes, ignores file-valued roots,
  and keeps recovered files private to Skills validation. A file-valued root
  yields no skills; link checks resolve its target independently. It follows directory
  symlinks only when their target stays inside the canonical repository root;
  file symlinks are skipped. Visible out-of-root directory aliases fail closed.
  Resolution checks each symlink component within that root and never stats or
  reads out-of-root targets. Hidden or over-depth external aliases are pruned
  without target metadata, so they do not consume response bytes. Depth,
  hidden-directory, and exclude checks use the visible
  alias path, while canonical directory identity stops cycles. Skill, Markdown,
  template, and linked-resource reads use the same bounded resolver. Link existence
  checks do not use the skill-discovery
  depth limit, so a depth-six skill can link to a resource under `node_modules`
  or a deeper directory. They still honor tracked-index membership, configured
  excludes, hidden-directory pruning within declared roots, and the repository
  symlink boundary. A linked target beyond discovery depth is checked for
  presence only; its contents are not recursively parsed. Git worktrees use only index-tracked files,
  tracked directory-symlink aliases, and tracked canonical targets, so
  untracked files cannot satisfy links; non-Git directories use the bounded
  physical walk. Each root fails closed above Codex's 2,000-directory
  (including the root), 20,000-entry, or 4 MiB inventory limits. Velnor also
  caps alias fallback checks at 20,000 per root to bound in-repository link
  resolution. It fails closed on malformed path declarations, where the Codex
  loader can ignore invalid entries.
  The inspected [Codex plugin parser](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/core-plugins/src/manifest.rs#L121-L127)
  accepts an omitted or blank plugin `name` and derives it from the plugin-root
  basename ([field defaults](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/core-plugins/src/manifest.rs#L38-L45),
  [legacy name fallback](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/core-plugins/src/manifest.rs#L288-L291));
  Velnor accepts any string plugin name and rejects only non-string names.
  Skill frontmatter `name` may be omitted, blank, or null, in which case Velnor
  uses the skill directory name; an explicit name can override that directory name.
  Velnor normalizes whitespace and enforces the runtime's 64-character limit
  ([frontmatter parser](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/skills/src/parser.rs#L43-L85),
  [directory-name fallback](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/ext/skills/src/loader/host.rs#L344-L404)).
  Velnor type-checks the runtime's `name`, `description`, and
  `metadata.short-description` fields. Codex's typed `metadata` object defaults
  when omitted, rejects explicit null, and allows `short-description` as a
  string or null. Velnor requires string keys in top-level and metadata maps,
  then ignores unknown string-keyed fields and their values—including
  `metadata.custom: [x]`—like Codex's Serde parser
  ([typed fields](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/skills/src/parser.rs#L6-L19),
  [parse behavior](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/skills/src/parser.rs#L44-L85)).
  A null `version` behaves as omitted at the plugin parser boundary. The Codex authoring
  sample documents only the string form
  ([sample field guide](https://github.com/openai/codex/blob/5c5308fc9a9ee789049d646ef11e5400384b9c6f/codex-rs/skills/src/assets/samples/plugin-creator/references/plugin-json-spec.md#L46-L61));
  Velnor follows the inspected runtime behavior.
- Claude plugin manifests use default `skills/` plus declared skill paths.
  A Claude `skills` path accepts both `.` and `./` for the plugin root. Velnor
  currently applies the same normalization to marketplace `metadata.pluginRoot`,
  where `.` and `./` also mean the marketplace root. Claude docs require this
  value to be a relative path inside the marketplace and show `./plugins`; they
  do not name a special root token
  ([relative path rules](https://code.claude.com/docs/en/plugin-marketplaces#relative-paths)).
  A declared `commands` field replaces the default `commands/` root; list
  `./commands` explicitly to retain it, and `./` scans root Markdown commands,
  as described by the
  [path behavior rules](https://code.claude.com/docs/en/plugins-reference#path-behavior-rules).
  Claude skill frontmatter may omit `name` or `description`, and a plugin
  skill's `name` can override its directory name. Local marketplace entries
  can add local skills and commands; object sources are reported as uninspected.
  Marketplace and plugin names must use the documented kebab-case form without
  control or bidirectional formatting characters; duplicate plugin names fail
  scanning
  ([marketplace validation](https://code.claude.com/docs/en/plugin-marketplaces#marketplace-validation-errors)).
  For marketplace entries with `strict:false`, a `channels` declaration in
  `plugin.json` is also rejected: it is a behavior-bearing plugin declaration,
  while the marketplace entry is the complete component definition. Top-level
  and `experimental` `themes`/`monitors` are treated as components too, are
  reported as uninspected on ordinary plugin manifests, and conflict with
  `strict:false`.
  Relative source strings must start with `./` (use exactly `./`
  for the marketplace root); a bare single path component (including Unicode
  or underscore-prefixed names) requires `metadata.pluginRoot`
  ([source path rules](https://code.claude.com/docs/en/plugin-marketplaces#relative-paths)).
  Claude skill and command frontmatter accepts its
  documented boolean spellings and `allowed-tools` string or YAML-list forms
  ([frontmatter reference](https://code.claude.com/docs/en/skills#frontmatter-reference));
  the `background` field must use one of the documented boolean spellings.
- Kimi Code documents both root `kimi.plugin.json` and
  `.kimi-plugin/plugin.json`; the root file takes precedence when both exist
  ([manifest selection](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/app/plugin/manifest.ts#L38-L57)).
  Kimi trims the manifest name, ignores non-string versions, and warns then
  omits malformed `commands` fields or paths; Velnor follows those behaviors.
  When no `skills` field is given, the CLI falls back only to a root `SKILL.md`
  if one exists; it does not infer a `skills/` directory
  ([root fallback](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/app/plugin/manifest.ts#L100-L107)). The pinned discovery path parses that file in
  `root-skill-only` mode and returns without walking the repository; Velnor
  keeps this fallback scoped to `SKILL.md`, so unrelated Markdown, resources,
  and templates are not traversed
  ([root-only discovery](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L40-L53));
  declared entries must start with `./` (`./` names the plugin root), resolve
  as directories, and reject direct file paths and bare `.`
  ([path resolution](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/app/plugin/manifest.ts#L161-L211)).
  The CLI recurses through directory depth 8 and checks each walked
  directory's immediate child Skills before pruning dot-prefixed and
  `node_modules` directories, so a direct child Skill can sit at relative
  directory depth 9 below the declared root;
  those directories can contribute a direct `SKILL.md`, but deeper descendants
  are not scanned
  ([depth limit](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L11-L15),
  [walk guard](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L37-L44),
  [child check before pruning](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L69-L78),
  [recursion](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L129-L139)).
  Velnor's bounded recovery adds `node_modules/SKILL.md` for a `node_modules`
  directory encountered below an ordinary declared root. If a declared root
  is itself beneath a pruned `node_modules` ancestor, Velnor recovers candidates
  through the CLI's depth-8 traversal. It checks a child's `SKILL.md` before
  pruning hidden or `node_modules` directories, then requires each discovered
  parent Skill's `has-sub-skill` or `hasSubSkill` flag before including deeper
  nested Skills. Recovered paths honor configured scan excludes; symlinks are
  not followed. For a declared root inside `node_modules`, Velnor also recovers
  the root's lowercase `.md` flat Skills. Kimi command entries may be a direct
  `.md` path or lowercase `.md` files collected recursively from a directory,
  including hidden and `node_modules` subdirectories; recovered command paths
  honor scan excludes and do not follow symlinks
  ([command entries](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/app/plugin/manifest.ts#L398-L435),
  [recursive collection](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/app/plugin/manifest.ts#L443-L460)).
  A nested Skill beneath another Skill directory is scanned only when its
  parent sets `has-sub-skill` or `hasSubSkill` to `true` at the frontmatter
  root or inside `metadata`
  ([subskill gate](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L220-L231)).
  Flat lowercase `.md` Skills are read only from the top level of each declared
  root, including hidden filenames; a direct file path is ignored
  ([top-level scan](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L96-L126),
  [direct file rejection](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/app/plugin/manifest.ts#L202-L211)).
  Directory
  `SKILL.md` files, including a root fallback `SKILL.md`, require non-empty
  `name` and `description` after parser normalization; invalid definitions are
  skipped individually. The CLI
  [parser](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/parser.ts#L83-L99)
  uses the frontmatter name without requiring it to match the directory.
  The [Help Center](https://www.kimi.com/en/help/plugins-and-skills/use-skills-in-code)
  says those fields may be omitted, which conflicts with the
  [CLI reference](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/skills.html#frontmatter-fields);
  scanning follows the current CLI parser. Kimi plugin
  [`commands`](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/plugins.html#declaring-commands-the-commands-field)
  accept file or directory paths, with lowercase `.md` files collected
  recursively from a directory. Kimi skill `type` may be omitted; if present,
  a non-empty trimmed string must be `prompt`, `inline`, `flow`, or `reference`.
  Missing, blank, and non-string types are treated as omitted; an unsupported
  string type skips only that skill. Flat Markdown skills may omit or use
  non-string `name` and `description` values and fall back to filename/body
  values. Other frontmatter values, including nested metadata, are passed
  through without string-only validation
  ([supported types](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/types.ts#L89-L91),
  [parser normalization](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/parser.ts#L65-L87),
  [per-skill error handling](https://github.com/MoonshotAI/kimi-code/blob/9d07f634be94ebeb1deba2f55d247807cf729315/packages/agent-core-v2/src/features/skill/catalog/fileSkillDiscovery.ts#L170-L189)).

For Agent Plugins, Antigravity, and Claude, the Skills adapter also restores
files omitted by generic `target/`, `dist/`, or `coverage/` pruning when they
sit inside recognized component roots. These files stay private to plugin
validation and do not become language-detector inputs. Physical command and
support-resource recovery fails closed after 20,000 entries per recovery pass.

The scanner validates recognized frontmatter in discovered skills and
commands. Kimi skill parse failures skip that individual skill, matching its
CLI discovery behavior. In modes with optional frontmatter, a leading `---` without a
matching closing delimiter remains Markdown; required-frontmatter definitions
fail if the delimiter is missing. For `.md` files it checks parsed CommonMark
links and images, including links inside tables, strikethrough, task lists, and
footnotes. Bare GFM autolink literals,
MDX content (including JSX URL attributes), and raw HTML URL attributes are
outside the parser contract. Only `http`, `https`, and `mailto` external URI
schemes are accepted; local links are confined to the plugin root. Codex links
use the in-repository symlink boundary described above; other providers reject
symlink traversal.

Only an immediate `templates/` directory beside a discovered `SKILL.md` is
treated as inert automatically. A sibling template directory explicitly linked
from a skill or command definition can also be inert inside that component
collection, unless it contains another discovered definition. Links from
support Markdown do not change template ownership. Other directories named
`templates/`, including flat-skill roots and monorepo package content, remain
visible to normal language detectors. `skills-count` is the number of unique
discovered skill-definition files; it is not a count of consuming repositories.
Provider tags identify recognized manifest adapters. Without a provider
manifest, `skills-provider:claude` can also be inferred from Claude's default
`skills/` or `commands/` layout, or a root `SKILL.md`, when no other provider
claims the files. A tag can be present when that adapter discovers zero skill
files.

This validation runs during repository scanning as workflows are generated.
The generated workflows do not emit a provider-independent Skills CI job:
there is no runtime command in generated repositories that repeats this
provider-specific scan. Invalid plugin metadata, recognized skill or command
frontmatter, parsed Markdown links, embedded `SKILL.md` frontmatter, and
malformed JSON/TOML/YAML templates fail workflow generation instead of silently
falling through to a language detector. Other template languages and prose are
not parsed. A plugin-only repository with no other supported workflow
shape is rejected by the existing no-supported-project-shape gate; the scanner
does not emit an empty workflow for static plugin metadata.

## `[renovate]` — self-hosted dependency updates

Scan evidence alone (`renovate.json`, `renovate.json5`, or `.github/renovate.json*`)
records `renovate-configuration` but does not emit workflows. A repository opts
in explicitly:

```toml
[workflow]
runners = "velnor"
velnor_labels = ["self-hosted", "example-lane"]
velnor_trusted_label = "example-trusted"
velnor_trusted_runner_available = true
files = [..., "renovate.yml", "renovate-validate.yml"]

[renovate]
enabled = true
reason = "Repository-local Renovate on trusted Velnor runners."
# schedule = "0 6 * * *"   # optional; default daily at 06:00 UTC
# token = "GH_RENOVATE_TOKEN" # optional; default GH_RENOVATE_TOKEN
# validate = true           # optional; default true — emits renovate-validate.yml
# cache = true              # optional; default true — repository-scoped actions cache

[[declare]]
primitive = "renovate"
file = "renovate.yml"

[[declare]]
primitive = "renovate-validate"
file = "renovate-validate.yml"
```

Generated `renovate.yml` runs on trusted Velnor runners only (`schedule` and
default-branch `workflow_dispatch`), uses `secrets.GH_RENOVATE_TOKEN` by default,
pins `renovatebot/github-action` and Renovate OSS `44.93.6`, and keys the
repository cache under `velnor-renovate-${{ github.repository }}-`. The
`renovate-validate.yml` job validates the config with
`ghcr.io/renovatebot/renovate:44.93.6` on the hosted runner. Renovate settings
are generation-time only and are not written into `.github/ci/project.toml`.
The scan reads the git index (tracked files only), so untracked CI runtime
artifacts, scratch files, and linked-worktree `.git` files never enter the
recorded scan input. A sidecar written by an older schema is never parsed:
rerun generate on a byte-matching tree to move it to schema 2.

Generated jobs install the runtime through the versioned composite action
(mise-action model: declare a revision, get the binary on PATH, cached)
instead of an inline `cargo install`, so toolchain setup stays centralized:

```yaml
- name: Set up Velnor workflow runtime
  if: ${{ runner.environment == 'github-hosted' }}
  uses: tailrocks/velnor/.github/actions/setup-velnor-workflow@<full-SHA>
  with:
    rev: <full-SHA>
```

The action isolates the install from job-level toolchain wrappers (for
example an `RUSTC_WRAPPER` pointing at an `sccache` that is set up later in
the job) and caches the cargo install keyed by revision and runner OS.
