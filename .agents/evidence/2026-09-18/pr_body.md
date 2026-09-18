True remainder of d2a (`375c0787` "provider-first schema rebased", parent `8b7b4ac1`) onto main. Per-hunk triage against `7aa4b4e0`: R2 (`eb0303a3`), d2b (#926), and the main line already carry everything except the items below. Additive only: **schema stays 1, pin stays `a6fa8d4a5096e6df37250b59a8105abfebdb9013`**.

## Ported (this PR)

| # | File(s) | What | Proof it was missing |
|---|---|---|---|
| 1 | `src/s2/scan/mod.rs`, `src/s2/scan/swift.rs` | Swift `detection_contract` → portable `(LinuxX64, default caps)`; drop redundant swift.rs override | s2 carried d9's stale `MacosArm64+native` arm while swift.rs overrode only the platform → LinuxX64 units kept `native_macos_arm64=true`, failing admission on local providers (`provider_pairing` failed pre-fix with `requires unsupported capabilities ... native-macos-arm64`, 6/6 post-fix) |
| 2 | `src/s2/config/mod.rs` (+ its test) | velnor `check_profile` validation defaults missing `[workflow] providers` to ALL, with d2a error text/fixtures | s2 parsed missing providers as `[]` → `has no velnor entry` on default universes, contradicting s2 `validate_workflow`'s own ALL default; d2a-added test case 2 pins the default |
| 3 | `src/s2/mod.rs` (xcframework test) | portable-Swift `provider_supports_unit` assertions (hosted + velnor) | d2a-added `plain_swiftpm_packages_stay_portable` assertions absent from s2's split test |
| 4 | `tests/provider_pairing.rs` (new) | s2 black-box pairing suite, 6 tests, self-contained via `CARGO_BIN_EXE` + schema dispatch (+1 clippy `expect` for length) | no s2 pairing coverage on main; `lane_pairing.rs` keeps s1 coverage |
| 5 | wording (11 lines) | stale `lane` → d2a provider/runner text in s2 `docs_site`, `check_profiles` (2), `ir` (2), `release` (4), `mod` (2) | d2a renamed the concept; R2 left stale words (zero behavior) |

## Deliberately not ported

- **Tree flip** (`schema = 2`, pin `08ea1b07`, generated `.github/*`, `project.toml`, generator-state, `actionlint.yaml` labels): excluded by brief (b)(c)(d); regen proof below shows byte-identical output.
- **promote flow** (`promote.rs`, `promote_command`, `RenderedTree`, `promote_atomic` test): modules absent from main; R2 scoped s2 without them; d2a hunks are unportable `--runners`→`--providers` churn.
- **apt feature** (config fields, `apt-*` runtime subcommands, feed writer/tests): predates d2a (in A and d9), untouched by the d2a diff, excluded by R2; s2 keeps simpler `apt` feed-kind handling.
- **pin self-fetch** (`ensure_pin_present`, tests, ir fetch-step removal, provisioner `(checkout)` form): predates d2a; main's fail-closed + lane-fetch + baked-revision strategy carried by R2 ("producer revision verification" drift).
- **`render_kind_units` removal**: d2a deleted it; main kept+evolved it ("head-candidate rendezvous" drift, also in s2). Deleting from s2 would regress main drift.
- **Apple split graft** (`apple_executor`, kind-split jobs): machinery predates d2a (in A and main s1); s2's coarse whole-job-on-macos handling is correct, R2-owned.
- **`info_id` string** (`any-local-trust-gated` vs s2 `any-local-trusted`): R2 hand-rewrote the fn (`&'static str`); no test/spec arbiter; changing rendered contract strings without an oracle is churn.
- **`ci_lane`→`ci_provider` rename**: BLOCKED by brief condition — s1 renderer emits `ci_lane:` (`s1/primitives/ir.rs:1552`), 5 generated unit workflows pass it, s1+s2 lib tests pin `inputs.ci_lane`. R2m (flip) owns it. (Known s2 gap: s2 callers pass `ci_provider:` to the shared `ci_lane` action — to fix at flip time, neither s1-break nor shim acceptable now.)
- **s1→s2 test conversions** (`velnor_first_ci` 522 lines, `platform_prerequisites`, `migration_contract`, `closure_reuse`/`selection_artifact_handoff` schema-3 runtime fixtures, `synthetic_surface`, `versioned_tool` shell-election form, all `tests/fixtures/*.toml`, contract tests/fixtures): converting them would destroy s1 coverage; s2 counterparts are d2b scope. B's shell writer-election and `matrix.config.writer`-less form are regressions vs s2's main-drift matrix design.
- **Dead code correctly omitted**: `Args::flag`, mod-level `json_string` (both lost all callers in d2a; porting would trip `-D warnings`).

## Per-file triage (76 d2a files)

`S1` = main schema-1 path, `S2` = `src/s2/*` via R2 `eb0303a3` unless noted.

| d2a file | In main? | Ported? |
|---|---|---|
| `src/config/mod.rs` | S2 + drift (apt excluded, renovate-lanes test added) | #2 only |
| `src/consumer_negatives.rs` | absent (test-only module, no pub items; R2 scope) | no |
| `src/estate.rs` | S2 mechanical + clippy reword | no |
| `src/lib.rs` | S2 + drift (dispatch, rendezvous, baked revision, fixtures) | #3 + 2 wording |
| `src/platform.rs` | S2 (cosmetic only; `default()==UntrustedOk`) | no |
| `src/policy.rs`, `policy/tests.rs` | S2 (fetch/tests predate d2a; promote-msg reword) | no |
| `src/primitives/aggregate.rs`, `plan.rs`, `providers.rs`, `template_memory.rs`, `cache.rs`, `pipeline.rs`, `snapshot.rs` | S2 byte-equal modulo `crate::s2::` | no |
| `src/primitives/check_profiles.rs` | S2 ⊃ B (branch-scoping + cancel drift) | 2 wording |
| `src/primitives/docs_site.rs` | S2 behaviorally equal (empty universe unreachable) | 1 wording |
| `src/primitives/ir.rs` | S2 + drift (fetch, rendezvous, coarse macos, `render_kind_units`) | 2 wording |
| `src/primitives/lanes.rs` | deleted in B; correctly absent from S2, kept in S1 | no |
| `src/primitives/mod.rs` | S2 minus B-dead `flag`/`json_string` | no |
| `src/primitives/prepared_tools.rs` | S2 ⊃ B (`need_records` drift) | no |
| `src/primitives/release.rs` | S2 + drift (matrix writer, local-only, feed inputs, quoting); apt excluded | 4 wording |
| `src/primitives/renovate.rs` | S2 newer (#923 token gating) | no |
| `src/primitives/runtime_products.rs` | S2 (freshness fork predates d2a) | no |
| `src/promote.rs`, `src/runners.rs`(+del), `src/provider.rs`, `src/runtime.rs` | promote absent (scope); runners kept for S1; provider semantics-equal; runtime apt/promote excluded | no |
| `src/scan/gradle.rs`, `rust.rs` | S2 mechanical | no |
| `src/scan/mod.rs`, `swift.rs` | S2 has d9-stale Swift arm + override | #1 |
| `src/tui/mod.rs`, `view.rs` | S2 mechanical | no |
| `tests/provider_pairing.rs` | absent (new file) | #4 (new) |
| `tests/lane_pairing.rs` | S1 keeps A's (s1 needs it) | no |
| `tests/promote_atomic.rs` | absent with promote flow | no |
| 12 other `tests/*.rs` + 8 fixtures | S1 keeps s1-world versions; B's are conversions | no |
| `velnor-workflow-contract` tests + fixture | tree-state (s1 tree); capability (`KeySegments`) already in S2 | no |
| `.github-gen/velnor-workflow.toml`, `.github/ci/*`, `.github/workflows/*`, `actionlint.yaml`, report action ×2 | flip / regen-only / rename-blocked | no |

## Verification

- `cargo test -p velnor-workflow`: 1581 lib + all integration bins green (incl. new `provider_pairing` 6/6; pre-fix baseline was 5/6 with the #1 admission error).
- `cargo test -p velnor-runner`: green (2166 passed, 4 ignored pre-existing).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean.
- `cargo fmt --check`: clean. `actionlint`: clean (rc 0).
- Regen: `./target/debug/velnor-workflow --plain --force` → `.github/workflows` **byte-identical** to base (`git diff --quiet 7aa4b4e0 -- .github/workflows/`); `--plain --dry-run` → **0 files**; no `.github/*` changes; pin line untouched.
