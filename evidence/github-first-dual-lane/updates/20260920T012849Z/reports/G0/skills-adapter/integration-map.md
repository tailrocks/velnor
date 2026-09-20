# G1/G2 integration dependency and conflict map

Prepared 2026-09-20 from source commit
`2ec3502026a2461aa520f6da6ac0a3cef3f1a93d`. Read-only integration
preparation; no shared branch, source worktree, PR, or remote ref was changed.
The disposable clones were `/tmp/velnor-integration.vKOPii` and
`/tmp/velnor-conflicts.kmH6Ga`.

## Baselines

The requested hosted baseline is
`e713841bdb9c33d853b7a9af88ceac924af1b3b6`, parent
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`. It is not the fetched
remote `origin/main`: current remote main is
`0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, parent
`e713841bdb9c33d853b7a9af88ceac924af1b3b6`, one descendant. The
approved-order trial was run from both baselines.

Exact baseline commands:

`git fetch origin main --quiet`

`git rev-parse origin/main`

`git merge-base <candidate> e713841bdb9c33d853b7a9af88ceac924af1b3b6`

## Reviewed candidate ownership and dependency

Every candidate below is rooted at `abe9ad82`. A range means all
commits in chronological order; a tip means the earlier chain is already
represented by the preceding candidate.

| Candidate | Exact chain/tip | Parent/base | Owned paths |
| --- | --- | --- | --- |
| Skills adapter | `4479ab7130f68e4a6cf06eadef1506f947b49b62` → `422061451e6572e0c541c770bd4b60c1a52c92e3` → `2ec3502026a2461aa520f6da6ac0a3cef3f1a93d` | tip parent `422061451e6572e0c541c770bd4b60c1a52c92e3`; base `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | `crates/velnor-workflow/src/s2/mod.rs`; `crates/velnor-workflow/src/s2/primitives/ir.rs`; `crates/velnor-workflow/src/s2/primitives/mod.rs`; `crates/velnor-workflow/src/s2/primitives/pipeline.rs`; `crates/velnor-workflow/src/s2/primitives/watch.rs`; `crates/velnor-workflow/src/s2/scan/mod.rs`; new `crates/velnor-workflow/src/s2/scan/skills.rs` |
| Lane compare | `3ad452acd8004c06cb866f75392bb5749adfe818` → `627c36f592437e4aef5f0d7d7f19ec85d391bac3` | tip parent `3ad452acd8004c06cb866f75392bb5749adfe818`; base `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | `crates/velnor-tools/src/lane_compare.rs` |
| Estate scope | `6387532f03c230f26795975ba5ff3034f2862eb7` → `7922da75adc2c034f396bd9a384182c776dfc084` → `58bd26f905624820042ff944997ea21476fc0127` → `0c56f753030fbe1cd77518100d12ab15ca90f256` → `e5b475b80110447469fdeb4a24b7967008b39aca` | tip parent `0c56f753030fbe1cd77518100d12ab15ca90f256`; base `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | `config/estate-repositories.json`; `crates/velnor-tools/src/audit_ci.rs` |
| Strict JSON | tip `b09d5a76133fb9d4e552e2d5253876b3988567f6` after estate scope | parent `e5b475b80110447469fdeb4a24b7967008b39aca`; review pending | `crates/velnor-tools/src/audit_ci.rs`; `crates/velnor-tools/src/main.rs`; new `crates/velnor-tools/src/strict_json.rs`; new `crates/velnor-tools/tests/fixtures/strict-json/nested-duplicate-conflicting.json`; new `crates/velnor-tools/tests/fixtures/strict-json/nested-duplicate-equal.json`; new `crates/velnor-tools/tests/fixtures/strict-json/root-duplicate-conflicting.json`; new `crates/velnor-tools/tests/fixtures/strict-json/root-duplicate-equal.json`; new `crates/velnor-tools/tests/strict_json_cli.rs` |

## Trial order and result

The candidate order preserving gate ownership was:

1. Hosted baseline `e713841b` (also repeated from `0dc79895`).
2. Estate scope range through `e5b475b8`.
3. Strict JSON tip `b09d5a76`.
4. Lane compare range through `627c36f5`.
5. Skills adapter range through `2ec35020`.

Exact trial operation:

`git cherry-pick --no-edit $(git rev-list --reverse <base>..<tip>)`

The full order applied cleanly from both baselines. Disposable final heads:

- `e713` trial: `d3e14e7b94409ab5caf9375c3443cb43ae97fc77`.
- `origin/main` trial: `c003590209d45a16190b166a78f1fa5f30abf2bd`.

These are disposable-clone cherry-pick results, not proposed merge commits.

## Non-integrated collision probes

Each probe reset to the approved disposable result, used
`git cherry-pick --no-commit`, then aborted/reset. No rejected branch
was retained.

| Branch/tip | Actual result against approved state | Ownership/decision |
| --- | --- | --- |
| Checker `a613e04108d088e6d5159aa3f3a593d7ab03e777` | Clean; paths are evidence checker tools/docs and `crates/velnor-tools/src/main.rs` | Checker owner; review/gate separately, do not fold here |
| Bootstrap experiment `78a39cf32e66e055c504247530b635640c5c42a7` | Clean; generated workflows, runner admission/manifest, release/policy and S2 primitives | Bootstrap owner; experiment remains unintegrated |
| Hosted security `9ae363b3aceb69b2217880bf371533a55eed4946` | Conflict: `.github-gen/velnor-workflow.toml` provider recovery/default-dispatch hunk | Hosted G1 owner; resolve against current hosted policy after review |
| APT `93efe18e5b7d4218487ce4a628b56cf7293d1aa7` | Conflict: `crates/velnor-workflow/src/s2/primitives/release.rs` at six test initializers (`jobs: Vec::new()` vs `..ReleaseSpec::default()`) | APT/G2 owner; do not resolve in Skills lane |
| Rust scanner `eca2460e9b8b3eddfc5883f0c1f0c8850cc36484` | Clean; Cargo/lib/Rust include scanner paths | Rust consumer owner; separate review |
| Action/generator `122fd50a60f55c66f34bed1ab035233a0d4b5744` | Clean; S2 policy and release primitives | Generator/action owner; separate review |
| Native product `934451bb3b1a087b67f2e356e3c958bf976a22eb` | Clean; runner product manifest and native product primitive | Native product owner; separate review |
| Native routing `746745c56a6798a13d176341b4996f66cdee2958` | Clean; Swift capability/platform scanners | Native routing owner; separate review |
| Native policy `38f71725df9998f125a0e3cffeff3105a5644061` | Clean; broad native contract and S2/native scanner paths | Native policy owner; separate review |

Clean means only that Git could apply the branch in the disposable probe; it
is not approval, review completion, or gate completion.

## Integration boundary

No generated consumer files, fleet branches, release refs, or rejected
candidate changes were integrated. The Skills and lane commits remain on their
own pushed branches. Strict JSON review remains pending. Any eventual
integration must rebase/retest the exact reviewed tips against the then-current
remote main and preserve the scope → G1/G2 → G3 gate order.

## Current-main collision refresh — hosted `9ae363b3` vs `origin/main 0dc79895`

Prepared 2026-09-20 from the exact hosted source tip
`9ae363b3aceb69b2217880bf371533a55eed4946` (chain from the common
`e713841b` baseline: `a38e459c`, `1c5eb2aa`, `d70a88a2`, `3aecc6ed`,
`9ae363b3`). The current fetched main is
`0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, whose parent is
`e713841bdb9c33d853b7a9af88ceac924af1b3b6`. The local `main` checkout was
older (`abe9ad82`) and was not treated as current main.

I created disposable worktree `/private/tmp/velnor-main-approved-map` at
`0dc79895`, applied the approved scope range through `e5b475b8`, lane-compare
range through `627c36f5`, and Skills adapter range through `2ec35020` with
`git cherry-pick --no-commit`. All three approved ranges applied cleanly onto
current main. No source worktree or remote ref was changed.

Applying the complete hosted range (`e713841b..9ae363b3`) then stopped at its
first commit `a38e459c` with exactly one unmerged path:
`.github-gen/velnor-workflow.toml`. The collision is the provider-recovery /
default-dispatch policy hunk:

| Current main `0dc79895` | Hosted `a38e459c` |
| --- | --- |
| Comment: `Recovery: hosted runs automatically; Velnor is manual opt-in.` | Comment: hosted recovery runs normal PR/push/schedule and omitted dispatch on GitHub-hosted while retaining Velnor for explicit qualification |
| `automatic_providers = ["github-hosted"]` | same |
| `default_dispatch_providers = ["github-hosted", "velnor"]` | `default_dispatch_providers = ["github-hosted"]` |

The approved scope/lane/Skills ranges do not touch this TOML hunk. Do not
resolve it by accepting one side silently: it is a current-main policy choice
about dispatch routing and Velnor qualification, not a mechanical merge. The
disposable probe was abandoned with conflict markers; no resolution or commit
was retained. This collision is independent of the hosted exact-source review
and must be settled by the lane/integration owner against the current routing
decision.

`git` clean application here means only that the approved ranges applied; it
does not approve those branches, the hosted source, or generated output. The
hosted exact review remains separately recorded at
`G1/reviews/hosted-provider-9ae363b3.md`; its tarball source-target blocker is
not erased by this collision map.

## Pre-merge pushed integration checkpoint — current-main content `b5a4b4af`

This section records the pre-approval/pre-merge state at `857646a0`; the
current post-approval state is recorded below.

After the read-only probes above, I created a fresh clone directly from
GitHub, fetched current `main`, and made the durable integration branch:

- clone: `/tmp/velnor-g3-integration-fresh.L1G7bH`
- branch: `codex/github-first-g3-integration`
- content baseline: `b5a4b4afaa6ca807927cacc03659b570a895dd5c`
- graph merge-base: `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`
- pushed head: `857646a09dc16506be4a1fe94fa73b2c0ab650a5`
- remote: `origin/codex/github-first-g3-integration` at the same SHA
- status: clean; branch is 17 commits ahead of current `origin/main` by commit
  graph

The content baseline and graph ancestry are intentionally different. The
integration branch was assembled from `0dc79895`, then the current-main
generated-tree patch was imported as cherry-pick `857646a0` (parent
`55a1e567`). Therefore `b5a4b4af` is not an ancestor of the integration
branch; `git merge-base HEAD b5a4b4af` is `0dc79895`. The current main ref was
`b5a4b4af` at this checkpoint, and its tree is present in the integration
tree.

PR956's pin commit `5c21a2a480e94da0f06ad56eb930aa34153a8b1a`
(`origin/pr-956`, parent `0dc79895`) is a sibling of `b5a4b4af`, not its
descendant. Its tree is byte/tree-equivalent to `b5a4b4af`
(`a14df926b9654566efa947dddc24c70a0c4e57c6`), but PR956 was not
cherry-picked or merged into this branch. No ancestry rewrite or merge was
performed while independent review was active.

The integrated order preserved the independently approved source ownership:

1. Estate scope chain: `6387532f` → `7922da75` → `58bd26f9` →
   `0c56f753` → `e5b475b8`.
2. Strict JSON tip: `b09d5a76`.
3. Lane compare chain: `3ad452ac` → `627c36f5`.
4. Skills adapter chain: `4479ab71` → `42206145` → `2ec35020`.
5. Rust consumer chain: `4320e628` → `08d19cf0` → `d2395fc7` →
   `eca2460e`.

All candidate commits were preserved by normal `cherry-pick -x`; original
signed-off and Codex co-author trailers remain in the imported commits. Two
additional branch commits are present and scoped:

- `55a1e567ab49f3a5e76b8ddb400f12c2fed9c222` scopes test-only Clippy
  expectations to the skills fixture module; it adds no production behavior.
- `857646a09dc16506be4a1fe94fa73b2c0ab650a5` imports the then-current main
  generated-tree pin patch from `b5a4b4af`, so the branch tree contains the
  exact current-main content. It is an imported main commit, not a consumer
  rollout; graph ancestry remains rooted at `0dc79895`.

At that pre-merge checkpoint, `git diff origin/main..HEAD` was limited to 24
source/config/test paths:
estate scope, strict JSON, lane compare, S2 primitives, the Skills scanner,
and Rust include scanning. It contains no generated workflow or consumer
documentation files. No merge, release, fleet rollout, or rejected candidate
had yet been performed.

### Verification

Passing checks on the pushed head:

- `rtk cargo fmt --all -- --check`
- `git diff --check`
- `rtk cargo clippy --workspace --all-targets -- -D warnings`
- `rtk cargo clippy -p velnor-workflow --no-default-features --lib -- -D warnings`
- `rtk cargo test -p velnor-tools --all-targets`: 246 passed
- `rtk cargo test -p velnor-workflow --no-default-features --lib`: 1,744
  passed; two known base-equivalent closure feature tests fail
- `rtk cargo test --workspace --exclude velnor-runner`: 3,019 passed

The default full workspace run has one unrelated performance-sensitive
failure, `workflow_command::tests::workflow_command_parse_benchmark`.
The same targeted benchmark passes on both this branch (2.46s) and a fresh
current-main `0dc79895` baseline (4.01s); therefore the full-run failure is
host contention/flakiness, not an integration regression. A serial full
workspace attempt was interrupted after 3348 passed and emitted no failure;
it is not claimed as a pass.

### Skills scanner evidence on pinned real repositories

The eight real research clones and pinned SHAs are recorded in
`implementation.md`. On each, the current branch's schema-2 dry-run produced
exactly one Skills execution unit and no nested units; counts were:
code-quality 12, macos 15, open-source 5, pull-request 6, roadmap 15, rust
15, skill-authoring 4, and typescript 12. Bun helper syntax validation under
Bun 1.4.0 passed for all eight.

The docs generator validator passed for all eight. Five consumers were byte
stable; three had pre-existing generated drift: macos (2 files), rust (1),
and typescript (4). Exact files and diffs are recorded in
`implementation.md`; no consumer files were changed on this branch.

This is a source-preparation checkpoint for independent integration review,
not an acceptance or G3-completion claim. Strict JSON and every integrated
source tip still require the separate review/gate sequence.

## Post-approval current-base checkpoint

Independent source integration review approved the exact `857646a0` source
tree in `G1/integration/review-857646a.md`. After that approval, the existing
branch was advanced with one normal, non-forced merge of fresh `origin/main`:

- merge commit: `5913fa6ff07ad08fb42dc78ae1530e9f0e11b308`
- merge parents: `857646a09dc16506be4a1fe94fa73b2c0ab650a5`,
  `b5a4b4afaa6ca807927cacc03659b570a895dd5c`
- merge tree delta against first parent: empty
- merge conflict resolution: none; the generated pin patch was already
  content-identical

The authoritative generator was then run with:

`mbx run --locked --manifest-path Cargo.toml -- --plain --force ../..`

from `crates/velnor-workflow`, after a dry-run review. It changed only the
generator-owned metadata files:

- `.github/ci/project.toml`
- `.github/ci/.github-actions-generator-state`

The only semantic generated change is detection of
`rust-test-targets:velnor-tools:1`; the matching scan and output hashes were
updated. No workflow YAML, consumer documentation, fleet, or rollout file
changed. Generator `--check` passes after regeneration.

The signed generated-metadata commit is:

- `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303`

Current pushed state:

- branch: `codex/github-first-g3-integration`
- head/remote: `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303`
- current `origin/main`: `b5a4b4afaa6ca807927cacc03659b570a895dd5c`
- graph merge-base: `b5a4b4afaa6ca807927cacc03659b570a895dd5c`
- status: clean; two commits ahead of current `origin/main`

The current source PR delta is the prior 24 source/config/test paths plus the
two generator-owned `.github/ci` metadata files (26 paths total). The branch
is prepared for the scoped source PR only; fleet rollout remains blocked by
the G1/G2 gates.

Exact delta from approved `857646a0` to current `5b9a16a6` is two files,
three insertions, and three deletions; the merge commit contributes zero tree
delta. This is the requested independent-review scope: approved source tree
plus normal current-main merge plus generator-owned metadata refresh.

Post-approval verification on the current head:

- generator `--check`: pass after authoritative regeneration
- `rtk cargo fmt --all -- --check` and `git diff --check`: pass
- `rtk cargo test -p velnor-tools --all-targets`: 246 passed
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: pass
- `rtk cargo test -p velnor-workflow --all-features --lib`: 1,804 passed
- `rtk cargo clippy -p velnor-workflow --all-features --all-targets -- -D warnings`: pass
- `rtk cargo test --workspace --exclude velnor-runner`: 3,019 passed

## Scoped PR and hosted gate

Opened PR [#961](https://github.com/tailrocks/velnor/pull/961) with base
`main` at `b5a4b4af` and head
`5b9a16a620951b65bbfe0a5cf7b1ffe04a317303`. Paginated review
endpoints showed no human reviews or inline review comments; the sole issue
comment is the Codex review-summary bot record marked completed without
findings.

Hosted results at the gate snapshot:

- Policy, Control/Planning, and all completed hosted source jobs passed.
- Two hosted jobs were still in progress: `velnor-runner` and
  `velnor-workflow`.
- DCO was `ACTION_REQUIRED`.

After polling completion, all 21 non-skipped hosted jobs passed (48 additional
lane jobs were skipped by the plan); no hosted job remains in progress. DCO is
the sole failing/action-required status.

The exact trailer audit over `origin/main..HEAD` found one unsigned
commit: `857646a09dc16506be4a1fe94fa73b2c0ab650a5`, the imported
current-main generated pin. All authored source, merge, and generated metadata
commits carry Signed-off-by and Codex co-author trailers. No force rewrite was
used; DCO remediation/override requires owner direction before any merge.

### DCO details and attribution audit

The DCO2 check details were retrieved through the PR GraphQL status payload.
It reports exactly one failure:
`857646a0` — `Sign-off not found`. It explicitly says:

- remediation commits are not allowed for this repository;
- the documented alternative is rebasing with `git rebase HEAD~19 --signoff`
  and force-pushing;
- no remediation commit or sign-off override was used.

The default main tree has no `.github/dco.yml`. The imported commit's
attribution is:

- `857646a0`: author/committer Alexey Zhokhov; only its
  `cherry picked from commit b5a4b4af` note is present;
- source `b5a4b4af`: author Alexey Zhokhov, committer GitHub, no
  trailers;
- PR956 head `5c21a2a4` has Alexey Signed-off-by plus Codex
  co-author, and its tree is identical to `b5a4b4af`.

Under the no-force/no-admin-override/no-fake-check constraints there is no
legitimate in-place fix on PR961. Owner choices are a separately authorized
history rewrite, enabling DCO remediation on the default branch before using
one, or an explicit maintainer override; none was taken.

## Signed replacement PR

Because DCO remediation is disabled and `857646a0` is redundant with the
current `main` tree, a replacement branch was created from the exact current
base without recreating or re-signing that third-party import. The original
branch and PR961 remain preserved.

- replacement branch: `codex/github-first-g3-integration-signed`
- base: `main` at `b5a4b4afaa6ca807927cacc03659b570a895dd5c`
- replacement head/remote: `fb78d85d464fd5082e5c161922afd7942380fabc`
- replacement PR: [#963](https://github.com/tailrocks/velnor/pull/963)
- PR963 state at opening: open, unmerged; `mergeStateStatus=BLOCKED` pending
  hosted checks and independent review

Only the 16 independently approved signed source units and one generated
metadata refresh were cherry-picked. No conflicts occurred. The replacement
tree is exactly equal to PR961 head `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303`:
`git diff` between the two heads is empty. Its delta from current `main` is 26
paths (24 source/config/test plus the two generator-owned metadata files), with
zero workflow YAML paths. All 17 replacement commits have both
`Signed-off-by` and `Co-authored-by: Codex <codex@openai.com>`; the audit found
`missing_trailers=0`.

Fresh local gates on PR963 replacement tree:

- `rtk cargo fmt --all -- --check`: pass
- `rtk git diff --check`: pass
- `rtk cargo test -p velnor-tools --all-targets`: 246 passed
- `rtk cargo test -p velnor-workflow --all-features --lib`: 1,804 passed
- `rtk cargo test --workspace --exclude velnor-runner`: 3,019 passed
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: pass
- `rtk cargo clippy -p velnor-workflow --all-features --all-targets --
  -D warnings`: pass
- authoritative generator `--check`: pass

At the first hosted snapshot, PR963 DCO passed; Policy was in progress and
Control/Planning was queued. Independent exact review and all hosted checks
remain required before considering PR961 superseded or closing it. No merge,
fleet rollout, or consumer migration was performed.

### PR963 exact review and hosted completion

Hosted PR963 run `35473797771` completed with every non-skipped source job
successful, including `ci-required`; DCO and Policy also passed. The final
replacement head remains `fb78d85d464fd5082e5c161922afd7942380fabc`.

The exact Codex review posted five findings; this is not acceptance-ready and
PR961 remains open:

1. P1: `include_str_paths` resolves `CARGO_MANIFEST_DIR` using the workspace
   root package even for member-crate source, so member assets can be watched
   at the wrong path. Resolve against the owning manifest or exclude nested
   package sources from the parent scan.
2. P1: Skills repositories are forced to Bun `1.4.0`; derive the version from
   target-owned inputs/configuration rather than hard-coding this repository's
   version.
3. P1: detection of one provider manifest requires unrelated Kimi/Claude
   manifests and marketplace files. Validate only provider manifests the
   target exposes, with shared metadata target-owned.
4. P1: template exclusion relies on exact prose substrings. Use catalogued
   structural template boundaries, including valid CommonMark references and
   unlinked template directories.
5. P2: helper validation samples unfiltered newest workflow runs; queued,
   in-progress, or failed runs can consume the limit and abort before enough
   successful completed runs are selected.

These findings require a reviewed source correction before any superseding or
merge decision. No source edits or consumer rollout were made in response.

## PR963 remediation exact head

The five findings were all treated as valid generic-generator defects. The
replacement branch was advanced with small signed pushes, then merged normally
with fresh `origin/main` (no force rewrite):

- current main/base: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- replacement branch: `codex/github-first-g3-integration-signed`
- exact pushed head: `6250b0feedcd4c3d5bcb64d266af350c73658945`
- merge commit: `491c6f7043b6fe69f2c0ba493284fd503bd6e062`
- PR: [#963](https://github.com/tailrocks/velnor/pull/963), still open and unmerged
- PR961 remains open; no consumer/fleet rollout or merge was performed

Correction commits and ownership:

1. `5b61ff10` — Rust include ownership: root-package include scans exclude
   nested package roots; member scans retain their own manifest root. Regression:
   `root_package_include_scan_excludes_nested_member_sources`.
2. `e3be1f17` — Skills provider projection, structural catalog template
   boundaries, nested helper-template path recognition, and target-owned Bun
   version selection. Regressions cover Codex-only provider manifests,
   unlinked catalog templates, conflicting target Bun pins, and all existing
   malformed metadata fixtures.
3. `327d2251` — `gh run list --status success` argument construction with a
   focused exact-argument regression.
4. `6250b0fe` — central fallback reads and validates only the exact Bun entry
   from the generator's `mise.toml` and matching `mise.lock` section. Full
   `mise.toml` parsing was removed because the repository's TOML parser rejects
   task-file constructs that mise/Python accept.

The fallback is not an ambient `latest` policy: target non-template
`package.json` files with `packageManager: "bun@<semver>"` determine the exact
version; conflicting pins fail closed; if no target-owned pin exists, the
generator's committed `mise.toml`/`mise.lock` exact pin is used. Template
package manifests are excluded before version selection. The eight snapshots
have no executable target Bun pin, so each correctly reports `skills-bun:1.4.0`.

Post-correction local checks on `6250b0fe`:

- `rtk cargo fmt --all -- --check`: pass
- `rtk git diff --check`: pass
- `rtk cargo test -p velnor-tools --all-targets`: 247 passed
- `rtk cargo test -p velnor-workflow --all-features --lib`: 1,813 passed
- `rtk cargo test --workspace --exclude velnor-runner`: 3,029 passed
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: pass
- `rtk cargo clippy -p velnor-workflow --all-features --all-targets -- -D warnings`: pass
- authoritative generator `mbx run --locked --manifest-path Cargo.toml
  --bin velnor-workflow -- --plain --check ../..`: pass; emitted notice only
  says the candidate render differs from declared generator pin and names the
  existing post-merge revision bump policy

The eight pinned research snapshots were dry-run with the rebuilt exact
generator binary using `--plain --dry-run`; no target files were written:

| snapshot | pinned SHA | catalog skills | detected jobs | Bun |
| --- | --- | ---: | ---: | --- |
| code-quality | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | 12 | 1 Skills | 1.4.0 |
| macos | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | 15 | 1 Skills | 1.4.0 |
| open-source | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | 5 | 1 Skills | 1.4.0 |
| pull-request | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | 6 | 1 Skills | 1.4.0 |
| roadmap | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | 15 | 1 Skills | 1.4.0 |
| rust | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | 15 | 1 Skills | 1.4.0 |
| skill-authoring | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | 4 | 1 Skills | 1.4.0 |
| typescript | `f434715f7c664af431f0be62982aa379400102de` | 12 | 1 Skills | 1.4.0 |

Each dry-run reported exactly one `Skills / Plugins` unit and no nested Rust or
Bun language units. The TypeScript template's `bun@1.4.0` is excluded as a
catalogued template, not used as an executable package claim.

Fresh hosted checks for `6250b0fe` were still in progress when this evidence
was recorded (`Policy`, `velnor-runner` GitHub lane, and `velnor-workflow`
GitHub lane); completed checks and DCO were successful. Independent exact-head
review is still required before superseding PR961 or merging PR963.

## PR963 fresh-review corrections exact head

Fresh exact-head review `5258386143` identified four additional valid source
issues. The same replacement branch was corrected with three signed commits:

- `fb88c407` — `std::include_str!` and `core::include_bytes!` remain visible
  to the Rust include scanner; arbitrary/user-qualified macros remain ignored,
  with direct standard-library qualification regression coverage.
- `25601cb0` — every Rust source is assigned to its nearest (longest matching)
  Cargo package root, preventing an outer package from scanning nested member
  sources; nested-package include regression added.
- `2cdce1cd` — Velnor job-log artifacts remain keyed by job through fetch,
  aggregation, and pair-stat calculation; pair-specific timestamp/group/ANSI
  evidence can no longer be inherited from another job.

Exact pushed state:

- branch: `codex/github-first-g3-integration-signed`
- base: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- head: `2cdce1cd954efe49dd6e7aea0901a5c78d846ca5`
- PR: [#963](https://github.com/tailrocks/velnor/pull/963), open/unmerged
- PR961 remains open; no consumer/fleet rollout or merge performed

Focused and affected local checks on this head:

- `rtk cargo fmt --all`: pass
- `rtk git diff --check`: pass
- standard-qualified include regression: pass
- nested-package ownership regression: pass
- per-job Velnor log-stat regression: pass
- `rtk cargo test -p velnor-workflow --all-features --lib`: 1,815 passed
- `rtk cargo test -p velnor-tools --all-targets`: 248 passed
- `rtk cargo test --workspace --exclude velnor-runner`: 3,032 passed
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: pass
- `rtk cargo clippy -p velnor-workflow --all-features --all-targets -- -D warnings`: pass
- authoritative generator `mbx run --locked --manifest-path Cargo.toml --bin
  velnor-workflow -- --plain --check ../..`: pass; existing declared-pin
  revision notice only

The exact-head Codex review was re-requested after these pushes. Independent
review remains the gate; green local/hosted checks do not authorize merge or
rollout.

## PR963 fresh-review corrections exact head `0c1ec757`

Exact-head review `5258451449` found three additional valid generic scanner
defects. They were corrected on the same branch with signed commits:

- `630c2c9e` — Rust include parsing accepts an optional trailing comma and
  recognizes `env!("OUT_DIR")` as a generated build output, skipping it rather
  than aborting repository scanning. Regression tests cover both forms.
- `ed0ebd8f` — schema-1 Rust scanning now passes the complete Cargo package
  census and assigns each source to its nearest package root, matching the
  schema-2 ownership boundary. Nested-package regression added.
- `0c1ec757` — the schema-1 test-only compatibility wrapper is marked
  `#[cfg(test)]` after production moved to the package-aware helper.

Exact remote state:

- branch: `codex/github-first-g3-integration-signed`
- base: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- head: `0c1ec75753cf9f8044a3a2ff01c2d144e9c59132`
- PR963 remains open/unmerged; PR961 remains open
- no consumer/fleet rollout or generated consumer writes performed

Verification on this head:

- `rtk cargo fmt --all -- --check`: pass
- `rtk git diff --check`: pass
- include parser focused suite: 10 passed
- schema-1/schema-2 nearest-package tests: 2 passed
- `rtk cargo test -p velnor-workflow --all-features --lib`: 1,818 passed
- `rtk cargo test --workspace --exclude velnor-runner`: 3,035 passed
- `rtk cargo clippy -p velnor-workflow --all-features --all-targets -- -D warnings`: pass
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: pass
- authoritative generator `mbx run --locked --manifest-path Cargo.toml --bin
  velnor-workflow -- --plain --check ../..`: pass; existing declared-pin
  revision notice only

The exact-head Codex review was re-requested after `0c1ec757`; independent
review and the resulting-main macOS runner gate remain outstanding.

## PR963 fresh-review corrections exact head `c440d4db`

Exact-head review `5258538555` found two additional valid generic defects:

- ownership roots for both Rust scanners must include every discovered Cargo
  package manifest, not only workspace-selected/generated units; otherwise an
  excluded nested package can be scanned as part of its outer package
- Skills frontmatter must accept provider-valid minimal `name`/`description`
  metadata and treat `argument-hint`, `license`, and invocation flags as
  optional typed fields, without imposing Apache-2.0 or `user-invocable: true`
  policy from this estate

Corrections were committed and pushed:

- `a96fa30a` — derive schema-1 and schema-2 ownership censuses from all
  discovered package manifests while retaining filtered unit generation
- `c440d4db` — require only `name` and `description`; validate optional
  string/boolean fields when present; add optional-policy and minimal-provider
  regressions

Exact remote state:

- branch: `codex/github-first-g3-integration-signed`
- base: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- head: `c440d4db3fd59a9e4abd396d7a75e670c4f3d862`
- PR963 open/unmerged; PR961 remains open; no rollout/consumer writes

Verification on this head:

- `rtk cargo fmt --all -- --check`: pass
- `rtk git diff --check`: pass
- Skills focused suite: 29 passed
- `rtk cargo test -p velnor-workflow --all-features --lib`: 1,820 passed
- `rtk cargo test --workspace --exclude velnor-runner`: 3,037 passed
- `rtk cargo clippy --workspace --all-targets -- -D warnings`: pass
- authoritative generator `mbx run --locked --manifest-path Cargo.toml --bin
  velnor-workflow -- --plain --check ../..`: pass; existing declared-pin
  revision notice only

Fresh exact-head Codex review was requested after `c440d4db`. Independent
review/hosted checks and the resulting-main macOS authority hold remain gates.

## PR963 fresh-review corrections exact head `8d04ecf3`

Fresh exact-head review `5258617104` found three additional valid generic
scanner defects on `c440d4db`:

- docs generation and docs-index validation were unconditional, rejecting a
  provider-valid Skills plugin that exposes no generated-docs surface
- Rust `env!` include expressions with the standard optional diagnostic string
  were rejected instead of being treated like the one-argument form
- Markdown link scanning toggled only backtick fences, so CommonMark tilde
  fences were scanned as live links

Corrections were committed as two signed commits and pushed without rewriting
the branch:

- `02beae25` — run generated-docs validation/checks only when the target
  exposes the docs surface; recognize matching backtick/tilde fences with
  their opening length; add docs-free provider and tilde-fence regressions
- `8d04ecf3` — accept `env!("NAME", "diagnostic")` while retaining strict
  literal validation; add the optional-diagnostic regression

Exact remote state:

- branch: `codex/github-first-g3-integration-signed`
- prior head: `c440d4db3fd59a9e4abd396d7a75e670c4f3d862`
- base at this checkpoint: `1048337062ea625fada1b4f7c07f2feed75f60c7`
- head: `8d04ecf308845fd811b75753f1db8d751082ce99`
- `HEAD` equals `origin/codex/github-first-g3-integration-signed`; worktree
  clean
- PR963 remains open/unmerged; PR961 remains open; no consumer/fleet rollout
  or generated consumer writes performed

Verification on this exact head before the required current-main merge:

- `rtk cargo fmt --all`: pass
- `rtk git diff --check`: pass
- Skills focused suite: 32 passed
- Rust include focused suite: 11 passed
- `rtk cargo clippy --locked -p velnor-workflow --all-features
  --all-targets -- -D warnings`: pass
- `rtk cargo test --locked -p velnor-workflow --all-features --lib`: 1,824
  passed
- `rtk cargo test --locked --workspace --exclude velnor-runner`: pass; all
  package, integration, and doc-test groups completed without failures

Hosted status observed before this push on `c440d4db`: DCO, Policy, CI
required, workflow-required, and all hosted package jobs were successful;
expected local/velnor jobs were skipped. The push invalidates that snapshot;
new CI and a fresh exact-head review are required. The resulting-main macOS
authority hold remains active.
