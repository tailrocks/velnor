# Confinement audit — velnor-bootstrap (READ-ONLY)

Root audited: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor-bootstrap` (`.../velnor` untouched).
All paths below are relative to that root unless absolute. Every cited body was opened; probes ran under `/tmp` only.

---

## 5. Fork poisoning: impossible? — YES for shared state, with one flagged boundary

### 5.1 Producer triggers (exact)

| Producer | File:line | Trigger predicate |
|---|---|---|
| Runtime-product producer | `.github/workflows/ci-runtime-products.yml:19-22` | `push: branches:[main]` OR `workflow_dispatch` (dispatch needs write access; fork cannot fire on upstream) |
| Candidate producer (build+publish) | `.github/workflows/ci-pr.yml:5-7` + `ci-unit-rust.yml:490,547` | `pull_request` (any repo, incl. forks) AND `github.event.pull_request.head.repo.full_name == github.repository` at publish time |
| Preview producer | `.github/workflows/preview.yml:19` | `push: branches:[main]` (+path filter) |
| Release producer | `.github/workflows/release.yml:19-20` | `push: tags:["v*"]` + dispatch |
| Cache prune (destructive) | `.github/workflows/maintenance.yml:5-13,29-38` | `pull_request[closed]` / schedule / dispatch-on-main; deletes only `refs/pull/$PR_NUMBER/merge` caches; `PR_NUMBER` is server-set numeric |
| Nightly | `.github/workflows/nightly.yml` | schedule + dispatch only |

Fork-reachable triggers: `pull_request` (ci-pr), `pull_request_target` (ci-policy), `pull_request[closed]` (maintenance).

### 5.2 Candidate publish gates (exact predicates)

- Input declared in all five unit workflows (`ci-unit-rust.yml:99`, `ci-unit-bun.yml:99`, `ci-unit-docker.yml:99`, `ci-unit-docs.yml:99`, `ci-unit-opentofu.yml:99`) but **only rust implements** the prepare/publish steps; the other four declare a dead input (no `Prepare candidate` step found by grep).
- Sole `true` callers: `ci-pr.yml:976` (`github-rust-velnor-workflow`, lane=github) and `ci-main.yml:1109` (dead: ci-main triggers are push/dispatch only, so the `event_name == 'pull_request'` conjunct is always false there — harmless).
- Prepare gate `ci-unit-rust.yml:490`: `inputs.candidate_publish && (event==pull_request && head.repo.full_name == github.repository)`.
- Publish gate `ci-unit-rust.yml:547`: same + `steps.candidate.outputs.skip != 'true'`.
- Build binding inside prepare (`:493-545`): `PR_HEAD` from server context `:494,498`; base pin read from base tree `:507-509`; skip when `head_closure == base_closure` `:514-517`; merge-vs-head closure decides reuse of the unit's own `target/debug/velnor-workflow` vs a `PR_HEAD` worktree rebuild `:519-532`; self-report must equal head closure `:539-540`; manifest written `:541`; artifact name `velnor-workflow-candidate-${head_closure:0:16}-${RUNNER_OS}-${RUNNER_ARCH}` `:545`; `retention-days: 1` `:553`.
- Note: ci-pr's *runtime* artifact (`ci-pr.yml:83-108`) is NOT PR-built: it copies `command -v velnor-workflow` (setup-action pinned release product) and asserts its `--closure` equals the setup output `:99`. Fork-safe by construction.

### 5.3 Consumer gates (policy side — the predicates that actually bind forks)

`ci-policy.yml` (mirrored in `ci-main.yml:156-215` for push-main context):
1. `:63-64` pin extraction constrained to `[0-9a-f]{40}` by sed; `:65` fail-closed on empty.
2. `:68-72` same-closure short-circuit (no candidate needed, nothing PR-built executes).
3. `:73` hard fork refusal: `[[ "$HEAD_REPOSITORY" == "$GITHUB_REPOSITORY" ]]` else exit 1.
4. `:79` run filter is server-side: `select(.head_repository_id == .repository.id)` on `ci-pr.yml` runs at exact `head_sha`.
5. `:99` manifest schema + binding: `profile=="debug"`, platform, `.repository==$repo`, `.run_id==$run`, revision/closure/digest regexes.
6. `:100-106` digest recompute over downloaded bytes; `:108-109` `manifest.closure == pin_candidate` where `pin_candidate` was computed by the **trusted base binary** from the audited pin (`:74`); `:110-111` `binary --closure == manifest.closure`.
7. `gh run download --repo "$GITHUB_REPOSITORY"` (`:98`): upstream repo only.

Validator-side same-repo filters (generator source): `crates/velnor-workflow/src/policy.rs:2335-2340` (`is_generated_velnor_pr_gate` exact normalized strings, same-repo PR + default-branch push/schedule + optional merge_group/dispatch), `:2113-2118` (`has_safe_runner_gate`), `policy.rs:1415-1422` (`TreeComparison::{Pin,Candidate,Differences}`), `policy.rs:1547-1559` (`Command::new(binary)` render-and-compare — second candidate exec point), `lib.rs:105-110` (audited pin must descend from base `VELNOR_WORKFLOW_POLICY_REVISION`), `primitives/ir.rs:1480,1537` (gate templates), `ir.rs:3341` (velnor-lane fork-skip conjunct `(event != 'pull_request' || same-repo)`).

### 5.4 Cache save gates (exact predicates)

Universal unit-cache predicate (all `actions/cache/save` in `ci-unit-bun.yml:286`, `ci-unit-docker.yml:326`, `ci-unit-docs.yml:284`, `ci-unit-opentofu.yml:289`, `ci-unit-rust.yml:267,390,555`, plus `ci-unit-rust.yml:345` with tool/miss conjuncts):
`((event==push && ref==refs/heads/main) || event==schedule || (event==workflow_dispatch && ref==refs/heads/main)) && cache-hit != true`
- Setup-action runtime cache `setup-velnor-workflow/action.yml:183-188`: same shape but against `github.event.repository.default_branch`, plus consumer re-verification on every restore (`:150-181`: manifest+digest+self-report) — poisoned cache fails closed.
- Producer toolchain cache `ci-runtime-products.yml:125`: `push && ref==main && miss` (strictest).
- Guest seeds `preview.yml:469`, `release.yml:3623`: `push && ref==main && miss`.
- All `mise-action cache_save` (`ci-unit-rust.yml:279`, `preview.yml:297,519,662`, `release.yml:422,546,...` ×19): `(push&&main)||schedule||(dispatch&&main)` — fork PR runs restore but never save.
- Enforcement gap noted: `policy.rs` contains **no** cache-gate rule (grep: only `:72,:1292` mention cache, unrelated). Gates are enforced by the byte-identical regen check (`policy.rs:15,:460,:1425-1455` `regenerate_and_compare`; `lib.rs:4495-4505` `verify_declared_pin_renders_tree`) + branch protection requiring the policy context (`ci-policy.yml:114-125`, `[policy] ruleset_required_status_checks` in `.github-gen/velnor-workflow.toml`). Any merged weakening of a gate fails policy first — provided the ruleset premise holds (out-of-repo).

### 5.5 Bypass attempts (all executed as reasoning + grep/probe, none modifying the repo)

- **B1 — fork strips publish gates in its own `ci-pr.yml`/`ci-unit-rust.yml`** (legal: `pull_request` uses the merge ref). Fork-run artifact gets published. Consumer still excludes it: server-side `head_repository_id` filter (`ci-policy.yml:79`), `:73` repo equality, `run_id`+`repository` manifest binding (`:99`), digest + closure + self-report triple bind (`:106-111`). **FAILS.**
- **B2 — artifact name collision** (`closure16` is attacker-influenced). Download is scoped to a filtered `run_id` at exact `head_sha`; name is a lookup key, identity comes from manifest+digest+self-report. **FAILS.**
- **B3 — cache poisoning** (fork strips a save `if:`). Writes from a PR run land in the PR-ref cache scope, invisible to main-branch restores (platform scoping); every security-relevant restore re-verifies digests (setup action `:150-181`, runtime `:99`, producer transport `:207-217`). **FAILS.**
- **B4 — fork strips a velnor-lane `if:` to run on self-hosted runners.** In-file `if:` gates do NOT bind a fork that edits the workflow file itself (merge-ref semantics). The GitHub-lane-only-for-forks property therefore rests on (i) repo fork-PR approval settings (out-of-repo, unverified) and/or (ii) velnor executor job isolation (executor scope, not audited here). **FLAG — workflow-level gates are not a security boundary here; poisoning of *shared artifacts* still fails per B1–B3, but fork code reaching a persistent self-hosted host is not excluded by these files alone.**
- **B5 — same-repo branch attacker** (write access). Can publish a *valid* candidate for their pin and have it executed in policy — this is documented-by-design (`ci-policy.yml:21-28`). Confinement: ephemeral host, `contents:read`, no secrets. Residual: candidate inherits `GH_TOKEN` (contents:read) during the `:110` probe and can append `GITHUB_ENV`/`GITHUB_PATH` for later steps. Bounded; see §6.

**§5 verdict: fork poisoning of products, caches, and policy verdicts is impossible (defense holds at the consumer + platform layer even under fork workflow-edit). B4 (self-hosted reachability) is the one boundary these files alone do not establish — conditional PASS.**

---

## 6. `pull_request_target` execution in `ci-policy.yml` — every PR-controlled byte

### 6.1 Confinement premises (verified)

| Premise | Evidence | Status |
|---|---|---|
| Permissions | top-level `contents: read` `:13-14`; job `contents: read` `:29-30`; no `id-token/packages/attestations/actions` | ✅ |
| Secrets | grep `secrets\.` in file: **zero hits** | ✅ |
| `github.token` in env | `GH_TOKEN` on Acquire `:57` and Ruleset `:116` only; used for read APIs + artifact download | ✅-with-note (visible to candidate at `:110`, same-repo only) |
| Credentials | `persist-credentials: false` `:37` (only checkout) | ✅ |
| Ephemerality | `runs-on: ubuntu-24.04` `:19` (GitHub-hosted fresh VM); job has **zero** cache/artifact steps (grep count 0) → no persistence channel | ✅ |
| Workflow provenance | `pull_request_target` (`:5`) runs the **base** file; fork edits to it are inert | ✅ |

### 6.2 Exhaustive PR-controlled byte enumeration (in step order)

- **E1 — fetch + checkout** `:38-48`: `git fetch $GITHUB_SERVER_URL/$HEAD_REPOSITORY $HEAD_SHA` + `checkout --detach`. Data only; both expansions quoted; sha/server-provided. No execution.
- **E2 — setup action** `:49-53` (`./.github/actions/setup-velnor-workflow`, `lib.rs:78-79,4128-4133,3741-3748`). Resolves against the **workspace root**, which contains no checkout (checkout went to `policy-checkout/`). So: either the step fails closed (documented disk resolution, file absent) or it resolves to the base ref server-side. In **neither** branch does it load `policy-checkout/.github/actions/*` (PR bytes) — the dangerous form `./policy-checkout/...` is not used. **Not PR-controlled execution.** (If disk resolution holds, this job currently cannot pass its setup step — flagged as a possible live breakage, out of scope to fix read-only.)
- **E3 — pin/closure compute** `:62-67`: trusted base binary over git objects; pin regex-bound. No execution.
- **E4 — candidate `--closure` probe** `:110`: **EXECUTES PR-built bytes** (gated: same-repo only `:73,:79` + manifest/digest/closure binds `:99-109`). Inherits step env **including `GH_TOKEN`** (contents:read). First of two exec points.
- **E5 — gh fetch/download** `:79-98`: runs/artifacts APIs pinned to upstream repo + filtered run; bytes verified before use. No execution.
- **E6 — `velnor-workflow policy`** `:126-139`: trusted base binary **parses** the PR tree (`--workflow-root policy-checkout`) = parser exposure, not execution; then **re-executes the candidate** via `Command::new(binary)` (`policy.rs:1555`) to render-and-compare — second exec point, same-repo only, **without** `GH_TOKEN` in env (Enforce env has none; `GITHUB_ENV`-persisted vars only).
- **E7a — `mise exec actionlint` lint** `:146-148` with `cwd=policy-checkout`: **fork `mise.toml` `[env]` IS loaded into the linter's environment** (probe-proven, §6.3). `[tasks]` are not executed by `mise exec`; tool version is pinned on the command line. Env-only injection (e.g. `LD_PRELOAD`-style vars honored or ignored per the executed binary's own loader behavior).
- **E7b — actionlint itself**: statically parses fork `.github/workflows/*.yml` + fork `.github/actionlint.yaml`. Parser exposure only; actionlint executes nothing it lints.
- **E8 — ruleset contexts** `:114-125`: reads branch rulesets (admin-controlled, not PR-controlled). No execution.

Nothing else in the file touches PR bytes: `mise-action` setup (`:141-145`) is pinned (`jdx/mise-action@c2a8761`, `actionlint@1.7.12`, `cache: false`) with no PR-derived inputs.

### 6.3 Probes (mise 2026.9.9, `/tmp`; clean-room with isolated `MISE_CONFIG_FILE/DATA/CACHE/STATE_DIR`)

1. Fresh dir, `mise.toml` with `[env] CLEAN_PWN="yes-injected"`, status `untrusted` → `mise exec -- env` printed `CLEAN_PWN=yes-injected`, exit 0, **zero stderr bytes**. Trust status flipped to `trusted` afterward (auto-trust on use, silent).
2. Same for `[tasks.boom]`: `mise run boom` executed the untrusted task without resistance.
3. **Conclusion: mise trust does NOT block untrusted config `[env]` (or tasks) in non-interactive `exec`/`run`.** The generator comment claiming `mise exec actionlint@<ver>` involves "no config lookup" (`lib.rs:3765-3771`) is **false for `[env]`** — true only in the narrow sense that the *tool version* doesn't come from config.
4. Runner-version caveat: probes ran against workstation mise; the runner uses whatever `mise-action v4.3.0` provisions. Treat injection as assumed-present regardless of version.

### 6.4 Residual + safe-and-unavoidable justification

Residuals: (a) fork-controlled env vars in the lint step — but that step holds **no credentials** (`GH_TOKEN` is per-step env, absent there; `GITHUB_ENV` carries only non-secret paths/contexts), runs on an **ephemeral** host, and the only exfiltratable bytes are the PR tree the attacker already owns; (b) same-repo candidate execution with a read-scoped token + `GITHUB_ENV`/`GITHUB_PATH` writeback — by design, and same-repo implies write access (a stronger starting position than anything this yields).
**Justification HOLDS for the fork threat model: nothing fork-controlled executes with credentials, persistence, or ambient authority. The `mise.exec` env leak is real but consequence-free here; the comment at `lib.rs:3765-3771` should still be corrected.**

---

## 10. Hyphenated matrix keys + dynamic `runs-on` in generated workflows

Old validator: `policy.rs:2759-2767` (`matrix_field_reference`, charset `[A-Za-z0-9_]`); unresolvable reference → `dynamic: true` (`policy.rs:2684-2712`) → runner-gate failure. A hyphenated key can never resolve under the old validator, so any `matrix.<with-hyphen>` in `runs-on` = FAIL; elsewhere = unresolvable.

Exhaustive `matrix.*` reference census (`grep -ohE 'matrix\.[A-Za-z0-9_.-]+' *.yml`):
`matrix.arch`×51, `matrix.guest_arch`×4, `matrix.lane`×1, `matrix.os`×2, `matrix.platform`×1, `matrix.runner`×4, `matrix.target`×14 — **zero hyphenated references in any file.**

Strategy blocks (all 9) and dynamic `runs-on` (all 4):

| File:line | Block | Keys defined | Verdict |
|---|---|---|---|
| `ci-runtime-products.yml:84-94` | `matrix:` | `os, arch, runner` | no hyphens ✅ |
| `ci-runtime-products.yml:95` | `runs-on: ${{ matrix.runner }}` → `ubuntu-24.04`, `ubuntu-24.04-arm` | `runner` resolves, static | **PASS** ✅ |
| `preview.yml:256-264` | `matrix:` | `arch, target, runner` | no hyphens ✅ |
| `preview.yml:252` | `runs-on: ${{ matrix.runner }}` → `ubuntu-24.04`, `ubuntu-24.04-arm` | resolves, static | **PASS** ✅ |
| `preview.yml:611-620` | `matrix:` | `arch, target, runner, guest_arch` | no hyphens ✅ |
| `preview.yml:913-917` | `matrix:` | `arch` | no hyphens ✅ |
| `release.yml:2877-2884` | `matrix:` | `target, lane, runner` | no hyphens ✅ |
| `release.yml:3020-3027` | `matrix:` | `arch, platform, runner` | no hyphens ✅ |
| `release.yml:3028` | `runs-on: ${{ matrix.runner }}` → `ubuntu-24.04`, `ubuntu-24.04-arm` | resolves, static | **PASS** ✅ |
| `release.yml:3412-3419` | `matrix:` | `arch, target, runner` | no hyphens ✅ |
| `release.yml:3408` | `runs-on: ${{ matrix.runner }}` → `ubuntu-24.04`, `ubuntu-24.04-arm` | resolves, static | **PASS** ✅ |
| `release.yml:3642-3649` | `matrix:` | `arch, target, runner, guest_arch` | no hyphens ✅ |
| `release.yml:3951-3954` | `matrix:` | `arch` | no hyphens ✅ |

No `strategy:` and no `${{ }}` `runs-on` in: `ci-main.yml`, `ci-policy.yml`, `ci-pr.yml`, all `ci-unit-*.yml`, `maintenance.yml`, `nightly.yml`, `ci-release-package-signer.yml` (verified by grep; `ci-pr`/`ci-main` use explicit job-per-unit, no matrix).

**§10 verdict: 0 hyphenated keys; 4/4 dynamic `runs-on` resolve to static labels under the old validator — PASS.**
