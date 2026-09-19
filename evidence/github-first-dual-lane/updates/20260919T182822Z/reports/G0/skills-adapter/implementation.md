# G3 skills/plugin adapter — implementation evidence

Source branch: `codex/github-first-skills-adapter`\
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`\
Implementation commit: `4479ab7130f68e4a6cf06eadef1506f947b49b62`

## Scope

- S2 only; no legacy scanner or fleet repository files changed.
- Added `UnitKind::Skills`, `skills-plugin-pipeline`, typed Bun provisioning,
  canonical watch globs, and `crates/velnor-workflow/src/s2/scan/skills.rs`.
- Detector validates provider manifests, strict duplicate-key JSON, catalog
  names/order, skill frontmatter, generated-doc index, local Markdown links,
  template JSON/TOML/YAML/frontmatter, and helper/doc verification commands.
- Only `skills/<catalog-name>/templates/**` is hidden from generic manifest
  detectors when the skill body explicitly establishes template context.
  Helper packages outside that boundary remain executable units.

## Exact repository scans

All scans used the G0-pinned HEAD SHAs and a schema-2 config in isolated copies.
Each exited 0 with exactly one `Skills / Plugins` unit and no nested Rust,
Bun, Node, Swift, Gradle, Docker, OpenTofu, Homebrew, or Docs unit.

| Repository | HEAD | Skills detected |
| --- | --- | ---: |
| `tailrocks-code-quality-skills` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | 12 |
| `tailrocks-macos-skills` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | 15 |
| `tailrocks-open-source-skills` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | 5 |
| `tailrocks-pull-request-skills` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | 6 |
| `tailrocks-roadmap-skills` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | 15 |
| `tailrocks-rust-skills` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | 15 |
| `tailrocks-skill-authoring-skills` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | 4 |
| `tailrocks-typescript-skills` | `f434715f7c664af431f0be62982aa379400102de` | 12 |

## Verification

- `cargo check -p velnor-workflow --no-default-features`: pass.
- Focused detector tests: 14 pass.
- `cargo clippy -p velnor-workflow --no-default-features --lib -- -D warnings`: pass.
- Full S2 library: 1,689 pass; 2 pre-existing closure tests fail because the
  checkout's `build.rs` spells default features as `""` while canonical closure
  expects `"tui"` (`closure::tests::stamped_features_match_dev_features` and
  `s2::closure::tests::stamped_features_match_dev_features`).
- Shared helper syntax command: 8/8 pass. A deliberately malformed
  `scripts/generate-docs.ts` exits 1 with Bun `Unexpected ;`.
- Generated docs command: 5/8 byte-stable. Current committed HEAD docs drift in
  `tailrocks-macos-skills`, `tailrocks-rust-skills`, and
  `tailrocks-typescript-skills`; no consumer docs were modified.

## Explicit remaining gate

The eight repositories expose no root Bun version pin (`package.json`,
`.bun-version`, or root tool manifest). The generated Skills unit reports this
limitation as no repository-proven version and uses the immutable setup-bun action pin without a
`bun-version` input. A central Bun version/metadata policy must be resolved
before rollout; no fleet rollout was performed.
