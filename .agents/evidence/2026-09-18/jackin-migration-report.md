Draft complete. Branch `velnor/generic-ci-adoption-draft` in jackin, uncommitted, nothing pushed. Velnor2 untouched (all generator interaction via read-only `git show` + a `/tmp` clone build; note: velnor2's working tree changed under me — `migration_contract.rs` now dirty — that's concurrent work by someone else, not mine).

## Config changes (`velnor-workflow.toml` only, +96/−50)

| Ledger row | Declaration | Grounding |
|---|---|---|
| L2 pin | `revision = dc150d2f…` + `# DRAFT: re-pin to the #917 merge commit…` | dc150d2f verified on `origin/feat/pr994-generic-ci-capabilities`, so CI can obtain it |
| Q4 (A) | `swift-package-native`: `capabilities=["xcframework"]`; new row for `swift-package-native-design-prototypes-unifiedagentusage` (id from `project.toml`): `capabilities=["xcode"]` | Row-over-scan merge; both callers now `apple_executor: true`, `runs-on: macos-26`, setup-action provisioning |
| F2 | `depends_on=["rust-jackin-usage-ffi"]` on swift-package-native | Unit id verified in `project.toml`; caller keeps `needs:[plan]`, edge recorded as `unit_dependencies` |
| L5 (G) | `desktop-merge` (macos, 90m, 8 tools, `tasks=["desktop-merge"]`, Xcode env) + `desktop-scheduled` (same + `periphery`, cron `41 4 * * 1`) → `desktop-merge.yml` (`events=["push"]`) + `desktop-scheduled.yml` | Tasks verified in `mise.toml` (`[tasks.desktop-merge/scheduled]`); all 9 tool ids verified as `mise.lock` `[[tools.*]]` keys; cron/env/toolsets from copy `desktop-cadence.yml` |
| L11 (H) | `[renovate]`: schedule `0 6 * * *`, token `RENOVATE_TOKEN`, config `renovate.json` (exists), `validate=true`, `lanes="github"`, author + `signoff=true` → `renovate.yml` + `renovate-validate.yml` | All values from copy `renovate.yml` + live `renovate.json` (`commitBody` sign-off matches) |

## Regen stats
Generator built from pristine clone @ dc150d2f (`--revision` confirms byte-exact). `generate . --plain --force`: **17 files, 4 created** (`desktop-merge/scheduled`, `renovate`, `renovate-validate`), 8 updated (pin b9c→dc150d2f, 403-fallback gain, additive `apple_executor`/`unit_dependencies`/`unit_admission` inputs, H maintenance rewrite), `actionlint.yaml`/`nightly.yml`/fleet env byte-identical. `git status` shows **only** the toml + generated files. **Zero `[[static_files]]`.**
- **Idempotence:** 4 consecutive runs, tree hash `ee691c74…` identical every time (4th run: all 16 content files `= unchanged`).
- **Policy self-check:** `policy --workflow-root . --pin-build` → **PASS, 11 rules, 0 failed**, incl. `generated-tree` byte-identical at pin (needed `VELNOR_WORKFLOW_POLICY_REVISION=dc150d2f…` env, same as CI sets).

## Deferred (nothing invented)
- **Hygiene 16 jobs (L7), REUSE (L12), renovate-validator mise-arch check (C3):** copies run raw shell / `reuse lint` directly; no mise tasks exist and `tasks` is required. Needs named tasks first.
- **Docs pipeline (L6/L16):** copy uses per-job mise provisioning (bun/lychee/codebook) + prebuilt xtask; renderer emits checkout + bare shell only, and zero `docs:*` tasks exist. Declaring would render failing jobs.
- **Docker/construct (L4):** schema docker publisher is tag-triggered; construct is main-push + `VERSION`-file driven with PR rehearsal — no main-branch-driven image shape exists (same gap class as versioned-tool-for-binaries). Facts banked: `projectjackin/construct`, `docker/construct/{Dockerfile,VERSION}`, `docker.io` + `DOCKERHUB_USERNAME/TOKEN`, guard task `construct-assert-version-unpublished` (exists).
- **Release rust-binary (L10):** two packages with different target sets (jackin 4 incl. mac, capsule 2 linux) vs one-package contract; macOS sign/notarize + capsule + external signer + tap have no surface — declaring would publish unsigned artifacts.
- **Preview (L9):** single-package contract vs jackin+capsule copy; copy's `workflow_run` producer `CI` doesn't exist in the generated surface; same unsigned-publish concern.
- **Versioned-tool/jackin-dev (L8):** completeness gate requires `version_gate_tasks` + `build_tasks`; neither exists (gate is inline `cargo tree`+`sed` shell, build via deleted composite action). Facts banked: package/binary `jackin-dev`, 4 targets, manifest `crates/jackin-dev/Cargo.toml` (v0.1.52 verified), prefix `jackin-dev-v`, group `homebrew-tap-publish`, push/PR paths.
- **Prepared-tool (L17/L18), Q5 script deletion, `[maintenance]`/`[policy]` additions:** no remainder requires a specific declaration / out of task scope / defaults already render.

## Flags for the migration PR
1. **Generator bug (new, blocks names):** multi-word `scheduled-checks` `name` renders `run-name: "Desktop merge cadence" · ${{…}}` — invalid YAML, aborts generation. Worked around by omitting `name` (stem defaults); comment in toml. Upstream fix needed.
2. **Trigger widening:** `events=["push"]` renders bare `push:` — merge tier runs on every-branch pushes, not main-only. Needs branch-scoping or acceptance.
3. **Renovate lanes = `github`** (copy defaulted velnor): velnor/both need `velnor_trusted_label`, which has no in-repo fact. Also dropped: copy's `mise` unsafe-execution allowance (no surface); validator now runs `--strict` schema check the copy deliberately omitted (false-negative history) — CI will prove it.
4. Check-profile mise steps pass no `github_token` (copy used `GH_READONLY_TOKEN`) — anonymous rate-limit exposure on shared runners.
