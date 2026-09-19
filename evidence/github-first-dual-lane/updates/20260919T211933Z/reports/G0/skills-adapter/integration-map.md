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
