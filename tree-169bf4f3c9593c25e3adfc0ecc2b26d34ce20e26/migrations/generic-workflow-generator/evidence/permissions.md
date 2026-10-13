# Velnor workflow-generator migration — GitHub preflight

Captured **2026-09-13T16:19:05Z**. Read-only. No repository mutations.

## Auth

- Host: `github.com`
- User: **donbeave** (`Alexey Zhokhov`), id `139017`
- Token scopes: `admin:org`, `delete_repo`, `gist`, `repo`, `workflow`
- Org roles: jackin-project **owner/admin**, tailrocks **owner/admin**, ChainArgos **owner/admin** (all `active`, direct membership)

## Compact table

| repo | id | default_branch | sha | push | admin | delete | open_migration_prs | blockers |
| --- | ---: | --- | --- | --- | --- | --- | --- | --- |
| `jackin-project/jackin-the-architect` | 1199879399 | main | `0dc1b98bd27b` | yes | yes | yes | #438 #434 #432 #427 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/tablerock` | 1301508644 | main | `d25b3466ec8c` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `jackin-project/jackin` | 1197700841 | main | `a506eee0581e` | yes | yes | yes | #965* | ruleset protect-main requires pull request to default branch; required checks: validate, docs-link-check, DCO, docs-required, construct-required, ci-required; protect-main current_user_can_bypass=always (RepositoryRol... |
| `jackin-project/jackin-agent-smith` | 1197909684 | main | `c34ffdb522e0` | yes | yes | yes | #190 #188 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `jackin-project/jackin-sentinel` | 1252589990 | main | `89cefcb9df1b` | yes | yes | yes | #124 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `jackin-project/jackin-role-action` | 1205101915 | main | `52ee2604aa66` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/parallax` | 1235761953 | main | `ec0eea1c7e14` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/velnor` | 1255367013 | main | `3975207f4e84` | yes | yes | yes | #739\* #737\* | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset); open generator-related PR(s): #739, #737 |
| `tailrocks/parallax-telemetry-playground` | 1277301638 | main | `05820ac7910e` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/velnor-actions-fixture` | 1256201624 | main | `efd569568d93` | yes | yes | yes | #147 #146\* #145\* #143\* #141\* | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset); open generator-related PR(s): #146, #145, #... |
| `tailrocks/holla` | 1262209244 | main | `fca7d0cc41e1` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/holla-apt` | 1262993132 | main | `d76974ffeac2` | yes | yes | yes | #80 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/homebrew-holla` | 1262212487 | main | `7b9d8e9b91d7` | yes | yes | yes | #136 #135 #130 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/termrock` | 1302045151 | main | `07e519e0573c` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset); classic branch protection on main; enforce_... |
| `tailrocks/homebrew-ruxel` | 1328281709 | main | `c8c8b573a38c` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/homebrew-tablerock` | 1307223747 | main | `adeb74331637` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/pg-bigdecimal` | 1247026498 | main | `613134dc83e0` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/ruxel` | 1265722009 | main | `110ed62b6df8` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/schemalane` | 1168023899 | main | `87442491fa8c` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `tailrocks/tracing-request-level` | 1247026496 | main | `76e2c892b733` | yes | yes | yes | 0 | ruleset protect-main requires pull request to default branch; required checks: DCO, ci-required; current_user_can_bypass=never on protect-main (admin cannot bypass ruleset) |
| `jackin-project/velnor-actions` | 1310831714 | main | `aba4e83dfd93` | yes | yes | yes | 0 | live/search workflow consumers still in 20 targets: jackin-project/jackin, jackin-project/jackin-agent-smith, jackin-project/jackin-role-action, jackin-project/jackin-sentinel, jackin-project/jackin-the-architect, tai... |
| `ChainArgos/velnor-actions` | 1310831990 | main | `76a459854130` | yes | yes | yes | 0 | live/search workflow consumers still in 20 targets: jackin-project/jackin, jackin-project/jackin-agent-smith, jackin-project/jackin-role-action, jackin-project/jackin-sentinel, jackin-project/jackin-the-architect, tai... |
| `tailrocks/velnor-actions` | 1310641212 | main | `06288eb65f3e` | yes | yes | yes | 0 | live/search workflow consumers still in 20 targets: jackin-project/jackin, jackin-project/jackin-agent-smith, jackin-project/jackin-role-action, jackin-project/jackin-sentinel, jackin-project/jackin-the-architect, tai... |

\* = `likely_generator_migration` (title/body contains velnor-workflow / github-gen / generation-input). Other keyword hits are mostly Renovate bodies mentioning `workflow`.

`delete` is inferred (REST `permissions` has no delete bit): repo `admin` + org owner + token `delete_repo`. DELETE was not executed.

## Identity and org policy

| org | role | members_can_delete_repositories | Actions | plan |
| --- | --- | --- | --- | --- |
| jackin-project | admin | False | enabled_repositories=all allowed_actions=all | free |
| tailrocks | admin | True | enabled_repositories=all allowed_actions=all | free |
| ChainArgos | admin | False | enabled_repositories=all allowed_actions=all | team |

Org owners can always delete repositories. `members_can_delete_repositories=false` on jackin-project and ChainArgos only restricts non-owner members.

jackin-project and tailrocks org rulesets API: `403 Upgrade to GitHub Team`. ChainArgos org rulesets: `[]`.

## Permissions (all 23)

Every listed repo: `visibility=public`, collaborator `role_name=admin`, `permissions.admin/maintain/push/triage/pull=true`, Actions `enabled=true` `allowed_actions=all`, Actions runs listable, workflows listable, can create PRs, can merge PRs (squash enabled on all; merge-commit/rebase vary), can administer rulesets/protection (admin).

### Merge methods and default-branch protection

| repo | squash | merge | rebase | auto-merge | classic protection | protect-main checks | bypass |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `jackin-project/jackin-the-architect` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/tablerock` | yes | no | no | yes | no | DCO, ci-required | never |
| `jackin-project/jackin` | yes | no | no | yes | no | validate, docs-link-check, DCO, docs-required, construct-required, ci-required | always |
| `jackin-project/jackin-agent-smith` | yes | no | no | yes | no | DCO, ci-required | never |
| `jackin-project/jackin-sentinel` | yes | no | no | yes | no | DCO, ci-required | never |
| `jackin-project/jackin-role-action` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/parallax` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/velnor` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/parallax-telemetry-playground` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/velnor-actions-fixture` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/holla` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/holla-apt` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/homebrew-holla` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/termrock` | yes | no | no | yes | yes (enforce_admins) | DCO, ci-required | never |
| `tailrocks/homebrew-ruxel` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/homebrew-tablerock` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/pg-bigdecimal` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/ruxel` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/schemalane` | yes | no | no | yes | no | DCO, ci-required | never |
| `tailrocks/tracing-request-level` | yes | no | no | yes | no | DCO, ci-required | never |
| `jackin-project/velnor-actions` | yes | yes | yes | no | no | unprotected main | n/a |
| `ChainArgos/velnor-actions` | yes | yes | yes | no | no | unprotected main | n/a |
| `tailrocks/velnor-actions` | yes | no | no | no | no | PR required, no status checks | never |

Almost every product repo uses repository ruleset `protect-main` (PR required, no-ff, no deletion) plus `protect-tags`. Required checks are typically `DCO` + `ci-required`. **jackin-project/jackin** also requires `validate`, `docs-link-check`, `docs-required`, `construct-required`, and is the only protect-main with `current_user_can_bypass=always` (RepositoryRole id 5 = admin).

**tailrocks/termrock** additionally has classic branch protection on `main`: `enforce_admins=true`, `allow_force_pushes=false`, `allow_deletions=false`, `required_conversation_resolution=true`, no classic required status checks (checks live in the ruleset).

Deletion-target default branches:

- `jackin-project/velnor-actions` and `ChainArgos/velnor-actions`: **no protect-main**, `branch.protected=false`, tag ruleset `immutable-fleet-tags` only.
- `tailrocks/velnor-actions`: protect-main PR+no-ff+no-delete, **no required status checks**, plus tag rulesets.

## Open PRs (keyword filter)

Open PRs across 23 repos: **70**. Keyword matches (`workflow` / `velnor-workflow` / `github-gen` / `migration`): **19**. Likely generator-migration subset: **7**.

### Likely generator-migration

- [`tailrocks/velnor-actions-fixture#146`](https://github.com/tailrocks/velnor-actions-fixture/pull/146): fix(audits): unswap baseline/runner labels in bind_baseline
- [`tailrocks/velnor-actions-fixture#145`](https://github.com/tailrocks/velnor-actions-fixture/pull/145): fix(ci): run rust-default on ordinary cargo without explicit mbx
- [`tailrocks/velnor-actions-fixture#143`](https://github.com/tailrocks/velnor-actions-fixture/pull/143): feat(ci): recover R1-cmd/R1-cond dual-lane pin workflows as repo-owned
- [`tailrocks/velnor-actions-fixture#141`](https://github.com/tailrocks/velnor-actions-fixture/pull/141) *(draft)*: rearch: move velnor-actions-fixture generation input into the repository
- [`tailrocks/velnor#739`](https://github.com/tailrocks/velnor/pull/739): docs(fleet): WP4/WP5 capturing evidence after #732/#723
- [`tailrocks/velnor#737`](https://github.com/tailrocks/velnor/pull/737): fix(workflow): keep Maintenance and Planning on GitHub-hosted runners
- [`jackin-project/jackin#965`](https://github.com/jackin-project/jackin/pull/965): ci(velnor): adopt velnor-workflow generator for CI surface

### All keyword matches

| repo | # | draft | keywords | title |
| --- | ---: | --- | --- | --- |
| `tailrocks/velnor-actions-fixture` | [147](https://github.com/tailrocks/velnor-actions-fixture/pull/147) | no | workflow | feat(ci): add fault-injection and soak suites to the required path |
| `tailrocks/velnor-actions-fixture` | [146](https://github.com/tailrocks/velnor-actions-fixture/pull/146) | no | velnor-workflow, workflow | fix(audits): unswap baseline/runner labels in bind_baseline |
| `tailrocks/velnor-actions-fixture` | [145](https://github.com/tailrocks/velnor-actions-fixture/pull/145) | no | velnor-workflow, workflow | fix(ci): run rust-default on ordinary cargo without explicit mbx |
| `tailrocks/velnor-actions-fixture` | [143](https://github.com/tailrocks/velnor-actions-fixture/pull/143) | no | velnor-workflow, workflow | feat(ci): recover R1-cmd/R1-cond dual-lane pin workflows as repo-owned |
| `tailrocks/velnor-actions-fixture` | [141](https://github.com/tailrocks/velnor-actions-fixture/pull/141) | yes | github-gen, velnor-workflow, workflow | rearch: move velnor-actions-fixture generation input into the repository |
| `tailrocks/velnor` | [739](https://github.com/tailrocks/velnor/pull/739) | no | velnor-workflow, workflow | docs(fleet): WP4/WP5 capturing evidence after #732/#723 |
| `tailrocks/velnor` | [737](https://github.com/tailrocks/velnor/pull/737) | no | velnor-workflow, workflow | fix(workflow): keep Maintenance and Planning on GitHub-hosted runners |
| `tailrocks/holla-apt` | [80](https://github.com/tailrocks/holla-apt/pull/80) | no | workflow | chore(deps): update dependency gh to v2.100.0 |
| `tailrocks/homebrew-holla` | [136](https://github.com/tailrocks/homebrew-holla/pull/136) | no | workflow | chore(deps): update jackin-project/velnor-actions action to v2026.8.33 |
| `tailrocks/homebrew-holla` | [135](https://github.com/tailrocks/homebrew-holla/pull/135) | no | workflow | chore(deps): update chainargos/velnor-actions action to v2026.8.33 |
| `tailrocks/homebrew-holla` | [130](https://github.com/tailrocks/homebrew-holla/pull/130) | no | workflow | chore(deps): update tailrocks/velnor-actions action to v2026.8.33 |
| `jackin-project/jackin-agent-smith` | [190](https://github.com/jackin-project/jackin-agent-smith/pull/190) | no | workflow | chore(deps): update node.js to v24.21.0 |
| `jackin-project/jackin-agent-smith` | [188](https://github.com/jackin-project/jackin-agent-smith/pull/188) | no | workflow | chore(deps): update dependency prek to v0.5.3 |
| `jackin-project/jackin-the-architect` | [438](https://github.com/jackin-project/jackin-the-architect/pull/438) | no | workflow | chore(deps): update node.js to v24.21.0 |
| `jackin-project/jackin-the-architect` | [434](https://github.com/jackin-project/jackin-the-architect/pull/434) | no | migration | chore(deps): update dependency rtk-ai/rtk to v0.49.0 |
| `jackin-project/jackin-the-architect` | [432](https://github.com/jackin-project/jackin-the-architect/pull/432) | no | workflow | chore(deps): update dependency github:open-telemetry/weaver to v0.26.1 |
| `jackin-project/jackin-the-architect` | [427](https://github.com/jackin-project/jackin-the-architect/pull/427) | no | workflow | chore(deps): update dependency prek to v0.5.3 |
| `jackin-project/jackin-sentinel` | [124](https://github.com/jackin-project/jackin-sentinel/pull/124) | no | workflow | chore(deps): update dependency prek to v0.5.3 |
| `jackin-project/jackin` | [965](https://github.com/jackin-project/jackin/pull/965) | no | github-gen, velnor-workflow, workflow | ci(velnor): adopt velnor-workflow generator for CI surface |

## Outside-scope velnor-actions consumers

Live workflow GETs (authoritative) show reusable-workflow `uses:` of all three `*/velnor-actions` SHAs. Files are marked `Generated by velnor-actions-generator. DO NOT EDIT.`

### Verified `uses:` outside the 20 migration targets

| consumer | path | uses |
| --- | --- | --- |
| `jackin-project/homebrew-tap` | `.github/workflows/package-update.yml` | `jackin-project/velnor-actions/.github/workflows/package-updater.yml@cfb0c475b2f235ed0d2b9d5e40e52a9f4ff05986` |
| `jackin-project/homebrew-tap` | `.github/workflows/package-update.yml` | `tailrocks/velnor-actions/.github/workflows/package-updater.yml@77d323dcfdb176b332edc24bfc92cb625b3ab4c8` |
| `jackin-project/homebrew-tap` | `.github/workflows/package-update.yml` | `ChainArgos/velnor-actions/.github/workflows/package-updater.yml@36d568abb89b4f53aa828fe1740fbb3411ffcb87` |
| `ChainArgos/jackin-agent-brown` | `.github/workflows/ci.yml` | `jackin-project/velnor-actions/.github/workflows/ci-code.yml@796dfcd26d4110319c8363155d2eae6885114893` |
| `ChainArgos/jackin-agent-brown` | `.github/workflows/ci.yml` | `tailrocks/velnor-actions/.github/workflows/ci-code.yml@c222e52030fee9ea6eae573a5769770be01d8438` |
| `ChainArgos/jackin-agent-brown` | `.github/workflows/ci.yml` | `ChainArgos/velnor-actions/.github/workflows/ci-code.yml@77173e8e71aa18e60d21f9f0d1ae57c0695d0233` |
| `tailrocks/homebrew-parallax` | `.github/workflows/package-update.yml` | `jackin-project/velnor-actions/.github/workflows/package-updater.yml@6669eac8693ec14957d2f55ae3b67756d1184e77` |
| `tailrocks/homebrew-parallax` | `.github/workflows/package-update.yml` | `tailrocks/velnor-actions/.github/workflows/package-updater.yml@77d323dcfdb176b332edc24bfc92cb625b3ab4c8` |
| `tailrocks/homebrew-parallax` | `.github/workflows/package-update.yml` | `ChainArgos/velnor-actions/.github/workflows/package-updater.yml@36d568abb89b4f53aa828fe1740fbb3411ffcb87` |
| `ChainArgos/blockchain-nodes` | `.github/workflows/ci.yml` | `jackin-project/velnor-actions/.github/workflows/ci-code.yml@851ef541d67f9cabebf2ddb2a2a02f51f6c54130` |
| `ChainArgos/blockchain-nodes` | `.github/workflows/ci.yml` | `tailrocks/velnor-actions/.github/workflows/ci-code.yml@3d108dd476d10e4327e5f34295b7324aa6207130` |
| `ChainArgos/blockchain-nodes` | `.github/workflows/ci.yml` | `ChainArgos/velnor-actions/.github/workflows/ci-code.yml@77173e8e71aa18e60d21f9f0d1ae57c0695d0233` |
| `tailrocks/homebrew-parallax` | `.github/workflows/ci.yml` | `jackin-project/velnor-actions/.github/workflows/ci-tap.yml@796dfcd26d4110319c8363155d2eae6885114893` |
| `tailrocks/homebrew-parallax` | `.github/workflows/ci.yml` | `tailrocks/velnor-actions/.github/workflows/ci-tap.yml@c222e52030fee9ea6eae573a5769770be01d8438` |
| `tailrocks/homebrew-parallax` | `.github/workflows/ci.yml` | `ChainArgos/velnor-actions/.github/workflows/ci-tap.yml@77173e8e71aa18e60d21f9f0d1ae57c0695d0233` |
| `jackin-project/jackin-dev` | `.github/workflows/ci.yml` | `jackin-project/velnor-actions/.github/workflows/ci-code.yml@796dfcd26d4110319c8363155d2eae6885114893` |
| `jackin-project/jackin-dev` | `.github/workflows/ci.yml` | `tailrocks/velnor-actions/.github/workflows/ci-code.yml@c222e52030fee9ea6eae573a5769770be01d8438` |
| `jackin-project/jackin-dev` | `.github/workflows/ci.yml` | `ChainArgos/velnor-actions/.github/workflows/ci-code.yml@77173e8e71aa18e60d21f9f0d1ae57c0695d0233` |
| `jackin-project/homebrew-tap` | `.github/workflows/ci.yml` | `jackin-project/velnor-actions/.github/workflows/ci-tap.yml@796dfcd26d4110319c8363155d2eae6885114893` |
| `jackin-project/homebrew-tap` | `.github/workflows/ci.yml` | `tailrocks/velnor-actions/.github/workflows/ci-tap.yml@c222e52030fee9ea6eae573a5769770be01d8438` |
| `jackin-project/homebrew-tap` | `.github/workflows/ci.yml` | `ChainArgos/velnor-actions/.github/workflows/ci-tap.yml@77173e8e71aa18e60d21f9f0d1ae57c0695d0233` |

Outside-scope **product** consumers (not the three velnor-actions repos themselves):

- `ChainArgos/blockchain-nodes`
- `ChainArgos/jackin-agent-brown`
- `jackin-project/homebrew-tap`
- `jackin-project/jackin-dev`
- `tailrocks/homebrew-parallax`

These are also **not** in the 20-target list and still pin all three velnor-actions repos: `ChainArgos/blockchain-nodes`, `ChainArgos/jackin-agent-brown`, `jackin-project/homebrew-tap`, `jackin-project/jackin-dev`, `tailrocks/homebrew-parallax`.

In-scope workflow consumers still mentioning velnor-actions (search ∪ live GET): `jackin-project/jackin`, `jackin-project/jackin-agent-smith`, `jackin-project/jackin-role-action`, `jackin-project/jackin-sentinel`, `jackin-project/jackin-the-architect`, `tailrocks/holla`, `tailrocks/holla-apt`, `tailrocks/homebrew-holla`, `tailrocks/homebrew-ruxel`, `tailrocks/homebrew-tablerock`, `tailrocks/parallax`, `tailrocks/parallax-telemetry-playground`, `tailrocks/pg-bigdecimal`, `tailrocks/ruxel`, `tailrocks/schemalane`, `tailrocks/tablerock`, `tailrocks/termrock`, `tailrocks/tracing-request-level`, `tailrocks/velnor-actions-fixture`. `tailrocks/velnor` already on velnor-workflow (no `ci.yml`). Fixture `ci.yml` already velnor-workflow; other fixture workflows still appear in search.

`tailrocks/velnor` default branch has **no** `.github/workflows/ci.yml` (404). Workflows are already `ci-pr.yml` / `ci-main.yml` / unit splits. `tailrocks/velnor-actions-fixture` ci.yml header: `Generated by velnor-workflow. Regenerate; do not hand-edit.` with local `uses: ./.github/workflows/_*.yml`.

### Code search additional hits (docs / research, not live uses)

Search also hit documentation/research repos (not workflow `uses:`):

- `donbeave/github-actions-unified`: `RENDER-MANIFEST.toml`
- `donbeave/github-workflows-refactoring`: `PLAN.md`, `audit/ledger.jsonl`, `audit/task-000/operator-readiness.json`, `audit/task-005/mise-locked-runner.json`, `audit/task-007/stability-attribution.json`, `audit/task-013/fleet-tools.json`, `docs/optimized-workflow-reference.md`, `plan/02-contract-infra/017-conformance-fixtures.md`
- `donbeave/green-everything`: `TRACKER.md`, `scripts/repos.txt`
- `donbeave/projects-structure-refactoring`: `LANDING_PLAN.md`, `PROCESS.md`, `README.md`, `TASKS.md`, `UNIFIED_REPOSITORY_STANDARD.md`, `archive/cycles/v3.1/review/round3-synthesis.md`, `archive/cycles/v3.1/review/round5-final-amendments.md`, `archive/cycles/v3.1/tracks/03-rust-workspaces.md`
- `ovladon/seenrelay`: `docs/EXTERNAL_WORKLOAD_PRESCREEN.md`

Code search caveats: later queries returned HTTP 403 rate limit (then reset). `path:.github` queries failed; unscoped + `path:.github/workflows` queries succeeded. Slash in `org/repo` may tokenize. Do not treat search as complete; live file GETs above are the deletion-risk evidence.

Open Renovate PRs still bumping `*/velnor-actions` in in-scope repos (title match `velnor-actions action`):

- [`jackin-project/jackin-role-action#160`](https://github.com/jackin-project/jackin-role-action/pull/160): chore(deps): update tailrocks/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-role-action#158`](https://github.com/jackin-project/jackin-role-action/pull/158): chore(deps): update jackin-project/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-role-action#157`](https://github.com/jackin-project/jackin-role-action/pull/157): chore(deps): update chainargos/velnor-actions action to v2026.8.33
- [`tailrocks/holla-apt#79`](https://github.com/tailrocks/holla-apt/pull/79): chore(deps): update tailrocks/velnor-actions action to v2026.8.33
- [`tailrocks/holla-apt#74`](https://github.com/tailrocks/holla-apt/pull/74): chore(deps): update jackin-project/velnor-actions action to v2026.8.33
- [`tailrocks/holla-apt#73`](https://github.com/tailrocks/holla-apt/pull/73): chore(deps): update chainargos/velnor-actions action to v2026.8.33
- [`tailrocks/homebrew-holla#136`](https://github.com/tailrocks/homebrew-holla/pull/136): chore(deps): update jackin-project/velnor-actions action to v2026.8.33
- [`tailrocks/homebrew-holla#135`](https://github.com/tailrocks/homebrew-holla/pull/135): chore(deps): update chainargos/velnor-actions action to v2026.8.33
- [`tailrocks/homebrew-holla#130`](https://github.com/tailrocks/homebrew-holla/pull/130): chore(deps): update tailrocks/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-agent-smith#184`](https://github.com/jackin-project/jackin-agent-smith/pull/184): chore(deps): update tailrocks/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-agent-smith#182`](https://github.com/jackin-project/jackin-agent-smith/pull/182): chore(deps): update jackin-project/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-agent-smith#181`](https://github.com/jackin-project/jackin-agent-smith/pull/181): chore(deps): update chainargos/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-the-architect#421`](https://github.com/jackin-project/jackin-the-architect/pull/421): chore(deps): update tailrocks/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-the-architect#419`](https://github.com/jackin-project/jackin-the-architect/pull/419): chore(deps): update jackin-project/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-the-architect#418`](https://github.com/jackin-project/jackin-the-architect/pull/418): chore(deps): update chainargos/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-sentinel#120`](https://github.com/jackin-project/jackin-sentinel/pull/120): chore(deps): update tailrocks/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-sentinel#118`](https://github.com/jackin-project/jackin-sentinel/pull/118): chore(deps): update jackin-project/velnor-actions action to v2026.8.33
- [`jackin-project/jackin-sentinel#117`](https://github.com/jackin-project/jackin-sentinel/pull/117): chore(deps): update chainargos/velnor-actions action to v2026.8.33

## Velnor availability

### This workstation

- No `velnor` / `velnor-runner` / `velnorctl` on PATH.
- No `VELNOR_*` env vars.
- No systemd. Did not SSH anywhere.

### Live GitHub-registered Velnor runners (now)

| org | group | selected repos | runners | online | notes |
| --- | --- | ---: | ---: | ---: | --- |
| tailrocks | velnor-trusted (id 3, visibility=selected, restricted_to_workflows=True, allows_public=True) | 18 | 10 | 10 | labels include `self-hosted`,`velnor`,`velnor-target-mvp`,`velnor-trusted` |
| jackin-project | velnor-trusted (id 3, visibility=selected, restricted_to_workflows=True, allows_public=True) | 7 | 8 | 7 | labels include `self-hosted`,`velnor`,`velnor-target-mvp`,`velnor-trusted` |
| ChainArgos | velnor-trusted (id 4, visibility=selected, restricted_to_workflows=True, allows_public=True) | 3 | 4 | 4 | labels include `self-hosted`,`velnor`,`velnor-target-mvp`,`velnor-trusted` |

tailrocks extra: 2× `velnor-fixture-microvm-slot-{1,2}` online with labels `self-hosted`,`velnor-microvm` (not `velnor-target-mvp`).

jackin-project: `velnor-jackin-project-slot-7` is **offline**; other 7 slots online.

All 20 migration targets that belong to tailrocks or jackin-project are in the org `velnor-trusted` selected-repo list. ChainArgos `velnor-trusted` selected repos are `blockchain-nodes`, `jackin-agent-brown`, `java-monorepo` (none of the 20). None of the three velnor-actions deletion targets are in a velnor-trusted selected list.

### Docs / fleet config clues (not live-probed on sentry)

- Desired policy JSONs: group `velnor-trusted`, labels `velnor-target-mvp`, visibility selected, `restricted_to_workflows=true`.
- Velnor on `sentry.tailrocks.internal` is installed **only** via apt `https://velnor-apt.tailrocks.com/`. Runner version on fleet hosts must be verified live (not from removed evidence docs).
- Live runner names now are `velnor-{org}-slot-N` (and fixture microvm slots), not the `velnor-dogfood-slot-*` names in the WP6 note. Version of the currently-online runners was **not** read from job logs in this preflight.

## Deletion targets — can DELETE?

| repo | id | sha | admin | org owner | token delete_repo | org members_can_delete | inferred can_delete |
| --- | ---: | --- | --- | --- | --- | --- | --- |
| `jackin-project/velnor-actions` | 1310831714 | `aba4e83dfd931ef0a5049ef3af7f4cbdc2a69e79` | yes | yes | yes | False | **yes** (not executed) |
| `ChainArgos/velnor-actions` | 1310831990 | `76a459854130953c67fd2c8d33cce8b843732be7` | yes | yes | yes | False | **yes** (not executed) |
| `tailrocks/velnor-actions` | 1310641212 | `06288eb65f3ed0e967ea86841a2c4131093ee624` | yes | yes | yes | True | **yes** (not executed) |

Blocker for actually deleting: **live outside-scope consumers still `uses:` all three repos**. Also many in-scope repos still pin them (including open Renovate bumps).

## Evidence paths

All raw API payloads under `/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/grok-goal-9ebed578eb9f/implementer/preflight`: `raw/`, `orgs/`, `repos/`, `rulesets/`, `runners/`, `search/`, `workflows/`.
