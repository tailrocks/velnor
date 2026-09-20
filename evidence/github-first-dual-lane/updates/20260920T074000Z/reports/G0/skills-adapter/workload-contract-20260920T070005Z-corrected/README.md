# Current exact-SHA Skills workload contract

Status: source-derived expectation only. Execution/gate status remains incomplete.

Captured UTC: `2026-09-20T07:10:25.299308Z`.
Raw capture: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/skills-raw-20260920T061548Z`; manifest status `False` (false is intentional because branch protection endpoints returned 404).

All eight exact default SHAs contain the same four source-required logical workload IDs:

- `tailrocks/tailrocks-typescript-skills` `f434715f7c664af431f0be62982aa379400102de`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-skill-authoring-skills` `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-rust-skills` `bcc31b1d935dac4de191a6b71b3091f628d204c0`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-roadmap-skills` `66d9c79f6472ddace0e265335dc8dd36cdeb8a86`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-pull-request-skills` `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-open-source-skills` `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-macos-skills` `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift
- `tailrocks/tailrocks-code-quality-skills` `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189`: skills-catalog-frontmatter, skills-docs-provider-metadata, skills-helper-template-reference-validation, skills-generated-doc-drift

The scanner model is one root `skills` unit, Linux x86_64, `untrusted-ok`, zero special capabilities. GitHub-hosted and GitHub-self-hosted are only platform candidates pending a reviewed validator/workflow; Velnor is explicitly not eligible because adapter proof is absent. Untrusted events remain static/hosted-only. Provider job names are model IDs, not observed jobs.

Source concerns are recorded per repository under `source_concerns`: schema/frontmatter, docs/provider metadata, license, package/runtime, and static lint/helper/template checks. Nested manifests remain fixture input and are explicitly excluded from Rust/Node/Bun/Swift production units.
Each repository also records `workload_source_evidence`: nonempty exact path lists and path/blob digests for every logical workload, joined to per-file Git/content digests in `normalized/source-files.jsonl`.
The accepted Skills scanner validates source shape, but does not validate docs source URL shape, provider repository/homepage URL relationships, or helper capability safety. Those are recorded as adapter requirements/unknowns, not passes.

No workflow/check/ruleset success is claimed. Current raw capture observed zero committed workflow paths, empty check/status rows, empty ruleset lists, and explicit branch-protection/required-status HTTP 404s. Required contexts/apps remain unknown.

Files: `contract.json`, `normalized/source-files.jsonl`, `normalized/digest-index.json`, and exact commit archives under `source-archives/`.

This is an explicit corrected successor of `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/skills-adapter/workload-contract-20260920T062700Z/contract.json` (SHA-256 `48bfaab27b6dc79a60433c7b298043c1969ee6037bfbdd548225cd10baf72bb5`); see `SUCCESSOR-CHANGELOG.md`.
