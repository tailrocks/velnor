# Skills adapter remediation contract

Reviewed: 2026-09-19T18:20:22Z  
Candidate evidence: `dual-lane-evidence/G0/skills-adapter/implementation.md`  
Candidate SHA: `4479ab7130f68e4a6cf06eadef1506f947b49b62`  
Scope: schema2 Skills validation only. No source or consumer edits were made.

This is the minimum contract for the source owner’s next implementation. It is
grounded in the eight pinned consumer snapshots. It does not add a legacy
schema1 implementation or require unrelated products to adopt Velnor packaging.

## 1. Observed consumer contract

The eight pinned snapshots contain 84 catalog-listed live `skills/*/SKILL.md`
definitions:

| consumer | catalog/live skills | `disable-model-invocation` present |
| --- | ---: | ---: |
| code-quality | 12 | 11 |
| macos | 15 | 13 |
| open-source | 5 | 5 |
| pull-request | 6 | 6 |
| roadmap | 15 | 14 |
| rust | 15 | 10 |
| skill-authoring | 4 | 4 |
| typescript | 12 | 10 |
| **total** | **84** | **73** |

All 84 observed definitions use these shapes:

- `name`: YAML string; exact catalog/path name.
- `description`: nonempty folded string (`>-`), followed by indented text.
- `argument-hint`: nonempty double-quoted YAML string.
- `license`: YAML string exactly `Apache-2.0`.
- `user-invocable`: YAML boolean `true` (not a quoted string).
- `disable-model-invocation`: optional YAML boolean; all 73 present values are
  `true`.

The skill-authoring snapshot also contains
`skills/tailrocks-skill-create/templates/skill/SKILL.md`, whose frontmatter is
typed like a live definition but deliberately uses the placeholder name
`<skill-name>`. It must be syntax-validated as an embedded template, not
counted as a catalog skill or forced to equal a live catalog name.

## 2. Minimum strict frontmatter/YAML contract

Implement one typed parser with an explicit mode:

### Live definition mode

1. Require the first and closing `---` delimiters. Parse the body as one YAML
   mapping; reject malformed YAML, duplicate keys, aliases/tags that cannot be
   represented by the typed schema, and trailing non-YAML content in the
   frontmatter block.
2. Allow exactly these top-level keys:

   `name`, `description`, `argument-hint`, `license`, `user-invocable`, and
   optional `disable-model-invocation`.

   Unknown top-level keys fail. Adding a future field requires an intentional
   schema change, not silent acceptance.
3. Deserialize actual YAML types. Do not stringify values, strip quote
   characters manually, or treat a YAML sequence/map as a scalar.
4. Require `name`, `description`, and `argument-hint` to be nonempty strings;
   require `license == "Apache-2.0"`; require `user-invocable == true`; if
   present, require `disable-model-invocation` to be a YAML boolean.
5. After parsing, enforce the existing path/catalog invariant: `name` equals
   the catalog entry and the directory component, with the existing safe-name
   rules (no slash, backslash, control, or whitespace).
6. Preserve folded description semantics. A valid folded block is a string;
   block style is not a reason to weaken type checks for the other fields.

### Embedded-template mode

Use the same typed mapping and key/type rules, but do not require `name` to
equal a catalog entry. The observed `<skill-name>` template is valid in this
mode. Template files remain excluded from generic language claims.

### Required negative fixtures

Each fixture must assert a nonzero detector result and no generated Skills
unit:

- `argument-hint: ["..."]`, `description: {text: ...}`, and
  `user-invocable: "true"` (wrong YAML types).
- duplicate `name` and duplicate nested mapping keys.
- unknown top-level field.
- missing opening/closing delimiter, malformed folded block, and malformed
  YAML.
- wrong license, false `user-invocable`, empty required strings, and invalid
  optional boolean.
- valid `<skill-name>` embedded template accepted in template mode but not
  emitted as a live catalog unit.

The already-proven sequence-valued `argument-hint` bypass is the regression
fixture that must become a hard failure.

## 3. Minimum Markdown reference contract

Observed relative destinations are predominantly `references/*.md`,
`templates/` directory links, and cross-skill paths such as
`../tailrocks-swift-project-setup/templates/project.yml` from nested reference
files. One intentional unresolved template reference exists in the roadmap
snapshot:

`../../research/<topic>/README.md`

The live corpus also contains external HTTPS links, anchors, fenced code
examples, and ordinary inline code containing angle placeholders. The parser
must distinguish those from local references.

### Resolver rules

1. Parse CommonMark inline link destinations, including balanced/nested
   parentheses, an optional angle-bracket destination, and an optional title.
   Do not locate a destination by taking the first `)`.
2. Skip fenced code blocks and inline code. Skip external `http`, `https`, and
   `mailto` destinations and fragment-only anchors. Unknown schemes, network
   paths (`//...`), and absolute paths fail closed.
3. For a local destination, separate path from query and fragment first. A
   query/fragment never changes the file lookup. Percent-decode the path once;
   reject malformed escapes, decoded control characters, backslashes, and
   encoded separators that could bypass traversal checks.
4. Resolve relative to the source Markdown file, normalize `.` and `..`, and
   reject repository-root escape. Check the normalized tracked-file set. A
   directory destination is valid only when a tracked descendant exists.
5. Preserve the actual roadmap template link by allowing a placeholder only
   when an entire path component matches a narrow token such as
   `<topic>` (`<[A-Za-z][A-Za-z0-9_-]*>`). Reject malformed/broad angle
   bypasses such as `topic<name>` or control characters. Placeholder targets
   are intentionally unresolved references, not proof that an arbitrary path
   exists.
6. Apply the same safe path resolver to `docs/index.json` relative document
   fields. Generated docs content is additionally covered by the byte-drift
   gate.

### Required link fixtures

Pass:

- `references/policy.md`, `templates/`, and a valid `../other-skill/...` path.
- `references/policy.md "title"` and a balanced parenthesized filename.
- Existing `../../research/<topic>/README.md` placeholder.
- A tracked `foo%20bar.md?view=full#section` resolving to `foo bar.md`.
- external HTTPS, `mailto`, and fragment-only links (ignored as non-local).

Fail:

- missing local file/directory, `../../../outside.md`, `/absolute.md`,
  `//host/path`, backslash traversal, malformed `%` escape, encoded traversal,
  unknown scheme, malformed destination/title, and broad angle placeholders.

## 4. Provider manifest compatibility semantics

All eight snapshots use provider version `0.28.0` exactly in:

- `.codex-plugin/plugin.json`
- `.kimi-plugin/plugin.json`
- `.claude-plugin/plugin.json`
- `.claude-plugin/marketplace.json` plugin entry

The root `plugin.json` has a `name` but no version. Provider field sets are
deliberately not identical: Codex/Kimi have `skills: "./skills/"`; Claude’s
plugin manifest has no `skills` field; marketplace has `plugins`, `owner`, and
`metadata`; provider manifests carry provider-specific interface/author fields.

The compatibility contract is therefore:

1. Root `plugin.json` requires a nonempty safe `name`; do not invent a root
   version requirement.
2. Codex, Kimi, and Claude plugin names equal the root name.
3. Each provider version is a strict SemVer 2.0 string. Preserve the exact
   version string, including any permitted prerelease/build components, and
   require exact equality across Codex, Kimi, Claude, and the marketplace
   plugin entry. Current all-stable value is `0.28.0`.
4. Codex and Kimi require `skills == "./skills/"`. Claude may omit `skills` as
   all current snapshots do; if a future Claude schema supplies it, validate
   its declared value under that schema instead of applying the Codex shape.
5. Marketplace requires exactly one plugin entry with matching name/version
   and `source == "./"`.
6. Keep recursive duplicate-key rejection for every JSON object. Do not apply
   one global unknown-key allowlist: actual provider-specific keys differ and
   are meaningful. Validate the typed compatibility fields above and preserve
   provider-specific fields.

### Required provider fixtures

Pass the eight current shapes, including root-without-version and Claude
without `skills`. Fail missing provider, non-string/empty version, malformed
SemVer, any cross-provider version mismatch, wrong names/source/skills, zero or
multiple marketplace plugins, and duplicate keys at any nesting level.

## 5. Exact Bun selection and reproducibility

The eight consumer roots do not contain a committed root `package.json`, Bun
lockfile, `.tool-versions`, or `mise.toml` that can define their helper runtime.
Seven snapshots contain shared `scripts/refresh-template-pins.ts` policy code;
the skill-authoring snapshot does not. The independent exact pin evidence is:

- Velnor `mise.toml:3`: `"aqua:oven-sh/bun" = "1.4.0"`.
- Velnor `mise.lock:127-164`: version `1.4.0`, pinned Aqua backend, and
  platform checksums/URLs for Linux, macOS, and Windows.
- The TypeScript Skills canonical setup template,
  `skills/tailrocks-tanstack-project-setup/templates/package.json:5`, says
  `"packageManager": "bun@1.4.0"`; its version policy names this template as
  the exact Bun pin source and requires synchronization with `mise.toml` and
  `mise.lock`.

Selection: use exact Bun `1.4.0`. This is independently present in the current
locked Velnor toolchain and the canonical Skills setup template; it is not an
ambient-machine inference and not a nested example-template inference.

Acceptance requirements:

- Keep the setup-bun action immutable-pinned and emit `bun-version: "1.4.0"`
  (or use the exact locked `mise` artifact) in the generated workflow.
- Run docs generation and helper syntax checks under that exact runtime; assert
  `bun --version == 1.4.0` before the checks.
- Derive the emitted version from one central typed toolchain source. Do not
  duplicate mutable `latest` or silently use the runner’s PATH Bun.
- Do not require the eight unrelated consumer repositories to add Velnor’s
  package manager, lockfile, or release metadata. Their generated CI unit may
  receive the exact runtime pin without changing product packaging.

## 6. Consumer docs drift ledger

The generated docs check is green for code-quality, open-source, pull-request,
roadmap, and skill-authoring. It reports stale generated definitions in three
snapshots:

| consumer | exact source SHA | files needing generated update | expected delta |
| --- | --- | --- | --- |
| macos | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | `docs/skills/tailrocks-macos-design/definition.md`; `docs/skills/tailrocks-macos-visual-baseline/definition.md` | add `design-pipeline.md` links; +4 lines |
| rust | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | `docs/skills/tailrocks-tui-design/definition.md` | add `design-pipeline.md` link; +3 lines |
| typescript | `f434715f7c664af431f0be62982aa379400102de` | three `tailrocks-tanstack-project-*` definitions and `docs/skills/tailrocks-web-design/definition.md` | add `version-policy.md` references and `design-pipeline.md`; +12/-6 lines |

Later consumer-owner task: run the repository’s `scripts/generate-docs.ts`,
review the exact generated additions above, commit them on the consumer’s own
branch, then rerun the byte-drift command. Do not hand-edit or broaden the
patch beyond generator output. Acceptance requires zero diff for all 8/8
snapshots.

## 7. Next-SHA acceptance matrix

The source owner’s next exact SHA is ready for review only when it proves:

1. Typed frontmatter pass/fail fixtures above, including the sequence bypass
   regression.
2. Markdown parser fixtures above and all eight pinned scans with one Skills
   unit, no nested language units, and no false fallback on malformed markers.
3. Provider compatibility fixtures with current root/provider shape preserved.
4. Generated workflow carries the exact Bun pin and local/CI checks report
   `1.4.0`.
5. Generated docs drift is zero for all eight consumers after the three later
   consumer patches land.
6. Ordinary standalone language packages and real helper manifests remain
   claimed; only embedded templates/support trees are excluded.

The two known full-library closure failures remain a base-equivalent issue and
are not part of this remediation contract.
