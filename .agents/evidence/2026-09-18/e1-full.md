Done. All reads complete. Returning the analysis.

---

# E-followup step 1: C6 confirmation against renderers

## Sources pinned

- `/tmp/jackin-render` HEAD: `0be3fcf95cd33c14fd3fe47af08026f17135a157` (2026-09-17). **HEAD has no `jackin-dev.yml`** — the clean-room regen (#992) dropped all static side files. The jackin-dev shape survives at **`ab5b0c4`** ("restore static velnor overrides"): `git show ab5b0c4:.github/workflows/jackin-dev.yml` (401 lines). That is the reference below.
- Renderers: `crates/velnor-workflow/src/primitives/release.rs` (both families live here — `Release::render` :142, `Preview::render` :171), pin check `primitives/mod.rs`:1328-1339, task validation `config/mod.rs`:2963-3036, gate-step shape `check_profiles.rs`:401-406, `resolve-mode` `runtime.rs`:3623-3682, `version_bump_matches` `runtime.rs`:2286.

## 1. Jackin-dev shape (ab5b0c4, `name: jackin-dev`)

**Triggers** (`on:`): `push` branches `[main]` paths `[crates/jackin-dev/**]`; `pull_request` paths `[sign-and-attest action, self yml, Cargo.lock, Cargo.toml, crates/jackin-dev/**, mise.toml, rust-toolchain.toml]` (no branch filter); `workflow_dispatch` inputs `lanes: velnor|github|both` (default velnor).
**File concurrency**: `group: ${{ github.workflow }}-${{ github.ref }}`, `cancel-in-progress: ${{ event == PR }}`.
**Job graph** (5 jobs):
| job | needs | gate | does |
|---|---|---|---|
| `validate-version-bump` | — | PR-only | checkout fetch-depth 0, paths-filter classify (lock vs artifact), cargo-tree closure base-vs-head compare + manifest sed version compare → fail if artifact inputs changed w/o bump. **No formula check here** (memo's Q2b body list is slightly off — formula is in assert-version). |
| `version` | — | always | outputs `version` (sed from `crates/jackin-dev/Cargo.toml`) + `runner-configs` JSON (lanes input → matrix configs) |
| `assert-version` | version | always | on main: `gh release view jackin-dev-v$VERSION` + formula version match (error if release exists but formula differs); off main: `published=false`. Outputs `published`. |
| `build` | version, assert-version | non-PR && published≠true | `config × target` matrix (4 targets, zigbuild), package tarball+sha256, sign/SBOM/attest, upload `jackin-dev-<lane>-<target>` |
| `publish` | version, assert-version, build | main && published≠true | **job** `concurrency: {group: homebrew-tap-publish, cancel: false}`; `gh release create jackin-dev-v$VERSION` (immutable, no clobber) with tarball+sha256+bundle+SBOM |

## 2. Current renderer shapes

**`release` family** (`render_release` :2358 → per-kind; `render_binary_release` :2605 for rust-binary): `name: Release`; `on:` push tags `["v*"]` (+dispatch w/ runner+mode inputs only when tarball bindings inject it, ~:2732); concurrency `release-${{ ref }}`/never-cancel; jobs `verify` (checkout depth-0, `verify-tag --branch --package`, + `release-<lane>-<unit>` unit jobs) → `build` (target×lane matrix, `cargo build`, `package-binary --version ${VERSION#v}`, attest, upload) → `publish` (env `github-release`, `gh release create ${{ ref_name }} --verify-tag --generate-notes`). **Version source = tag.**
**`preview` family** (`render_preview` :2145 tarball; `render_native_preview` :1711; bound via `inject_preview_triggers` :1692 + `inject_binding_jobs`): `name: Preview`; `on:` push branches `[default]` + `release_watch_paths` + **bare** dispatch (+`workflow_run` only when producer-bound); concurrency `preview-<repo>` cancel-**true** (tarball) / false (native); jobs tarball: `build` → `publish` (rolling `preview` prerelease, `--clobber`, push-main-only); native: identity→metadata→debian→[sign]→publish (atomic delete-recreate, monotonicity guard); bound adds `source` + `publish-gate` (`resolve-mode --event…`: PR→`validate`, push-main→`publish`). **No PR trigger, no gate/version/assert jobs, rolling not immutable, commit identity not manifest version.**

## 3. Trigger-by-trigger / job-by-job diff

| axis | jackin-dev | `release` | `preview` | fits? |
|---|---|---|---|---|
| push-main + paths | ✅ main + product paths | ❌ tags only, no paths | ✅ branches+paths | preview only |
| PR trigger | ✅ + product paths | ❌ none | ❌ none | **neither** |
| dispatch inputs | ✅ lanes (velnor/github/both) | runner (github/velnor/both) | bare, or modes when bound | closest: release |
| file concurrency | per-ref, PR-only-cancel | per-ref, never-cancel | per-repo, always/never | **neither** |
| PR version gate job | ✅ validate-version-bump | ❌ | ❌ | **neither** |
| manifest version job | ✅ version (sed+runner-configs) | ❌ (tag = version) | ❌ (commit identity) | **neither** |
| published-reuse assert | ✅ assert-version (release+formula) | ❌ (verify-tag only) | ❌ (monotonicity guard, rolling) | **neither** |
| target×lane matrix build | ✅ (+zigbuild, sign/SBOM) | ✅ cargo+package+attest | ✅ cargo+package+attest | both (modulo zigbuild) |
| immutable versioned publish | ✅ `gh release create <prefix>$VERSION` | ✅ `create ${{ ref_name }}` | ❌ rolling clobber/recreate | release only |
| job-level publish mutex | ✅ `homebrew-tap-publish` | ❌ (env protection only) | ❌ | **neither** |
| hardcoded `name:` | `jackin-dev` (per-file) | `Release` | `Preview` | blocks 2nd row in both |

## VERDICT: confirm — NEITHER family fits as-is

`preview` matches triggers (push-main) but mismatches publication semantics (rolling vs immutable-versioned) and has no gate/version/assert/PR machinery. `release` matches publication semantics (immutable `gh release create`) and build×publish mechanics but is tag-driven with tag-as-version and no PR/manifest/assert machinery. The 5-job graph (gate + version + assert + matrix build + mutexed publish) exists in neither renderer.

**New shape: new KIND inside the `release` family, not a new family.** Rationale: it reuses `declared_spec` parsing, `release_contract_complete`, `release_lanes`/matrix, package/attest/upload/publish machinery, and the `[release]`-contract omission rules. Concretely `kind = "versioned-tool"` (name negotiable; must read as main-branch-driven, not tag-driven):

- **Row args** (extend `Release::schema` :119-140 + `declared_spec` :250 + `ReleaseSpec`): existing `package/binary/targets/modes/producer_*`; new: `version_manifest` (path to version source, e.g. `crates/jackin-dev/Cargo.toml`), `version_prefix` (tag prefix, e.g. `jackin-dev-v`), `publish_group` (job concurrency group, e.g. `homebrew-tap-publish`), `version_gate_tasks` (list, see Q2b), `assert_tasks` (list, main-only product checks incl. formula), `push_paths`/`pull_request_paths` (lists; default = bound unit watch + manifest + toolchain files + self file — TBD in slice, no silent empty).
- **Rendered file** (new `render_versioned_tool_release(config, release)` called from `render_release` :2369 dispatch): `name:` per-row (see identity); triggers push-main+paths / PR+paths / dispatch+lanes; file concurrency per-ref PR-only-cancel; jobs `validate-version` → (`version` → `assert-version` → `build` → `publish`) exactly per the table above; publish `gh release create <prefix>$VERSION` immutable + `concurrency.group: <publish_group>`.
- **Open decision for the slice (flag, don't guess)**: build-cell body — jackin-dev needs zigbuild cross-builds (L14: zigbuild stays a named task). Either (i) matrix cells run declared `build_tasks` (recommended — genericity law), or (ii) extend generic build with zigbuild. The slice must pick one.
- **Pin change** (`mod.rs`:1328-1339): allow `file != canonical` for `primitive == "release"` **only when the row's `kind` is main-branch-driven** (`versioned-tool…`); tag-triggered kinds stay pinned to `release.yml` (keeps the guardrail the challenge defends). `RELEASE_SIDE_FILES` keeps `("release.yml", RELEASE)` for the default row; `push_default_side_rows` (:947, file-based skip) already suppresses the default when any declared row names `release.yml` — lock with a test per the C8 correction.
- **Per-row identity uniqueness rule** (new validation in `validate()`, same function as the pin): across all release-side rows, (a) rendered workflow `name:` must be unique → else `` `[[declare]]` primitive `release` renders duplicate workflow name `X`; give each publisher its own `name` ``; (b) `version_prefix` must be unique across rows → else `…publishes tag prefix `X`, already owned by file `Y``. Duplicate-file and unknown-primitive errors stay fail-closed (untouched).
- **Validation rules**: `versioned-tool` completeness = package+binary+real targets (reuse `release_contract_complete` :474) + non-empty `version_manifest` + `version_prefix` matching `^[A-Za-z0-9_.-]+-v$` (or documented tag-prefix grammar) + non-empty `version_gate_tasks` (plain refs, same predicate as `valid_check_profile_task`) + `publish_group` non-empty + both path lists non-empty. `verify-tag`-style tag checks must NOT render for this kind (no tag to verify — that path is dead code for it).

## Q2b gate placement (precise)

- **Renderer**: `crates/velnor-workflow/src/primitives/release.rs`, new `fn render_version_gate_job(release: &ReleaseSpec) -> String`, called only from the new `render_versioned_tool_release`. Job: id `validate-version`, `if: github.event_name == 'pull_request'`, checkout with `fetch-depth: 0` (PR version-range policy), pinned mise setup (mirror `render_tool_steps`), then one step per task. Placement of the gate *call* mirrors `resolve-mode`'s PR→validate mapping (`runtime.rs:3653-3664` — PR resolves to secret-free validate, publish-from-PR is a hard refusal); the gate is the enforcement side of that mapping.
- **Named-task reference shape** (mirror `[[check_profile]] tasks` exactly): row arg `version_gate_tasks: ["check-jackin-dev-version", …]` — a `Vec<String>` of plain mise task refs, validated non-empty with the same "no whitespace or shell syntax" predicate as `validate_check_profile_row` (`config/mod.rs`:3013-3033, reuse `valid_check_profile_task`, don't fork it), rendered as `mise run <task>` steps exactly like `render_profile_job` (`check_profiles.rs`:401-406). Same shape for `assert_tasks` (rendered inside `assert-version` on the main branch only). Task *bodies* (paths-filter sets, cargo-tree closure, manifest compare, formula check) stay Jackin-owned mise tasks — generic code owns placement, PR-only `if`, fetch-depth, and failure propagation only.
- **`version_bump_matches` (`runtime.rs`:2286, caller :2193)**: do NOT port it into rendered YAML. It stays the single cargo-specific classifier for *local-run selection* (`workflow.version_bump_units` allowlist = the one shared declaration); the CI gate is *enforcement* via the product task. No second classifier is created because generic code classifies nothing in CI. Record the split (selection ≠ enforcement, corrected L8) in the slice, no code change to `runtime.rs`.

## Files/functions to touch (implementer checklist)

1. `primitives/release.rs`: `Release::schema` (+7 args), `declared_spec` (new kind + fields), `release_contract_complete` (new arm), `render_release` (dispatch arm), new `render_versioned_tool_release` + `render_version_gate_job` (+ assert/build/publish job fns or one builder), per-row `name:` generalization for release files.
2. `primitives/mod.rs`: pin exemption (:1332-1339, kind-conditional) + duplicate name/prefix validation in `validate()` + default-row suppression test.
3. `config/mod.rs`: `ReleaseSpec` new fields + `RELEASE_MODES` reuse for dispatch modes input; task-ref validation reuse.
4. Jackin migration (waits for slice): one `[[declare]] primitive="release" file="jackin-dev.yml"` row with the args above + named mise tasks extracted from the ab5b0c4 bodies.