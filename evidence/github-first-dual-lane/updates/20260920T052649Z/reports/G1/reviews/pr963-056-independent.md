# Independent source review: PR #963 at `056362aadb`

Reviewed: 2026-09-20

## Verdict

**APPROVE source admission at this exact head.** This is source-only approval. It
does not approve merge, generated-output/pin adoption, rollout, or any hosted
gate beyond the status evidence recorded below. A later head needs a new
revision-bound review.

## Exact identity

- PR head: `056362aadb738279924ef05597f9a014392f48bf`.
- Exact parent: `b1563dadb022bbf8eff6b699c09f4e4267c53d81`.
- PR API base: `325719f1e05d3d46322c9fd3eeb9ad545e175638`.
- Current main used for applicability: `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`.
- Detached review checkout: `/tmp/velnor-pr963-056-review`, clean.
- Source diff from `b1563dad`: one file, `crates/velnor-workflow/src/s2/scan/skills.rs` (79 insertions, 51 deletions).
- Hosted tested merge: `3d450612adb6c12be3b6b811348555807861c7c7`, parents current main `89f82dd8` and head `056362aadb`; merge tree `211690886a5bf921f1e2739e13d037518660831d`. Local `git merge-tree --write-tree` produced the same tree.

The PR API base is historical relative to current main. Hosted CI tested the
explicit merge above; prior approvals do not transfer to this head.

## Strict frontmatter behavior

`crates/velnor-workflow/src/s2/scan/skills.rs` now matches the reviewed live
contract:

- Lines 1112-1119 allow exactly `name`, `description`, `argument-hint`,
  `license`, `user-invocable`, and optional `disable-model-invocation`.
- Lines 1121-1143 reject unknown keys and enforce YAML string/bool types.
- Lines 1144-1158 require nonempty `name`, `description`, and
  `argument-hint`.
- Lines 1160-1173 require `license == Apache-2.0` and actual YAML boolean
  `user-invocable == true`.
- Lines 993-1005 validate embedded `SKILL.md` templates under the same typed
  key/value rules while omitting catalog-name equality. The fixture at
  lines 1443-1464 uses the placeholder `<skill-name>` and the test at
  lines 1814-1820 accepts it without cataloguing it as a live skill.
- Unknown standard/provider extension fields (`compatibility`, `metadata`,
  `allowed-tools`) are intentionally rejected. This follows the authoritative
  G0 remediation contract, despite an older bot thread (`4055804131`) asking
  for those fields; provider-specific JSON fields remain provider-owned.

## Negative-input and production propagation proof

The rejection is not a silent empty-success path:

- `skills.rs:177-236` detects a plugin marker, validates catalog/manifests/
  skills/templates, and appends the Skills unit only after all validation
  succeeds. A frontmatter error returns before unit insertion.
- `s2/scan/mod.rs:67-72` calls `skills::detect(...)?`; the error leaves the
  production scan as `Err`, rather than returning a shape with zero units.
- `s2/mod.rs:1337` propagates that scan error through `scan_target`,
  `s2/mod.rs:5559-5565` propagates it through `render_tree`, and
  `s2/mod.rs:5605-5617` propagates it through the CLI generation path. The
  separate zero-unit guard at `s2/mod.rs:5561-5565` also fails closed.
- `crates/velnor-workflow/src/main.rs:5-12` maps any generator error to a
  nonzero exit (`ExitCode::from(1)`).
- `skills.rs:1619-1632`'s production detector helper asserts both an error and
  an empty unit set. The exact head's negative tests cover wrong YAML types,
  unknown/non-contract keys, duplicate keys, missing required fields, wrong
  license, false `user-invocable`, empty required strings, invalid optional
  bool, delimiters, malformed YAML, aliases, and tags (`:1634-1812`). The
  focused suite passed all 36 tests.

The parser validates actual `serde_yaml::Value` types before normalizing the
returned map. `Frontmatter` still stores normalized strings (`:166-169`,
`:1077-1088`), including the validated bool values. Only the live `name` is
consumed downstream (`:626-643`), so this does not reopen an invalid-input
acceptance path; a future typed-retention refactor may remove that internal
normalization if the G0 contract is enforced literally at representation level.

## Independent tests

All commands ran against detached exact head `056362aadb`:

- `rtk cargo test --locked -p velnor-workflow --lib s2::scan::skills::tests`: **36 passed**.
- `rtk cargo test --locked -p velnor-workflow --lib`: **1840 passed**.
- `rtk cargo test --locked -p velnor-workflow --test package_release`: **3 passed**.
- `rtk cargo test --locked -p velnor-tools --bin velnor-tools`: **248 passed**.
- `rtk cargo test --locked --workspace`: **5448 passed, 5 ignored**.
- `rtk cargo clippy --locked -p velnor-workflow --all-targets -- -D warnings`: no issues.
- `rtk cargo fmt --all -- --check`: pass.
- `rtk git diff --check b1563dad..056362aadb`: pass.
- Named serial reruns for `frontmatter_requires_all_live_fields`,
  `typed_frontmatter_rejects_wrong_yaml_types`, and
  `embedded_skill_template_is_validated_but_not_catalogued`: each passed.

The exact binary reports revision `056362aadb738279924ef05597f9a014392f48bf`
and closure `592a9d874a9d27ccc390b4cdcae6d55afd313ce667e73a77c6d32ca67c3ffd9e`.
`--plain --default-branch main --check .` exits 0 and reports unchanged
generated files matching the candidate render; it also reports the expected
post-merge pin follow-up from declared `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`.

## Hosted status and workload

At capture:

- CI run `35488557270` is completed success with head `056362aadb`; its tested
  merge checkout is `3d450612`.
- The planning log reports `scope=affected`, `VELNOR_PROVIDERS=github-hosted`,
  17 nonempty unit IDs, `full_units` equal to those 17, and `excluded=[]`.
- The CI run has 68 jobs: 20 success and 48 intentional skips (17 Velnor
  lanes, 17 Velnor shadows, 13 package `prepare-cargo` lanes, and `Control /
  Prepare Cargo`). DCO, Policy run `35488556411`, `ci-required`, and
  `Control / Required` are successful. Hosted Rust workflow reports 1973/1973
  tests passed; tools reports 250/250.

These statuses confirm revision-accurate, nonempty hosted work. They are
recorded as evidence only; this report does not grant merge authority.

## Feedback census

Fresh paginated REST/GraphQL reads found 9 bot review summaries, all
`COMMENTED` (no approval), 28 inline comments/threads, all unresolved, with no
page remainder; issue comments were also read. Seven inline comments target
this exact head, including the superseded standard-fields discussion. No new
review summary had reviewed `056362aadb` at capture. No source, PR, branch, or
GitHub state was modified.
