# D2a rest branch evidence — feat/d2-provider-schema-rest

Commit: 375c0787 (one content commit on 8b7b4ac1, DCO-signed)
Pushed: origin/feat/d2-provider-schema-rest (regular push, no force)
Base: 8b7b4ac1 (latest origin/docs/bastion-final-plan at build time)
Content: certified d9b9ce45 rebased; 8db9e5ac self pin-bump DROPPED.

## Pin / schema state

- `[generator] revision` = 08ea1b07 (the TIP's value, kept; no d9 self-bump —
  `grep d9b9ce45` over .github-gen + .github: zero hits).
  - Note: the brief named pin 7341ef4b, but the tip has since moved to
    08ea1b07 (7341ef4b is its ancestor). Invariant held: no unmerged-pin bump.
- Generation config schema = 2; runtime project.toml schema = 3.
- 76 files changed, +13607/−11474. lanes.rs/runners.rs/lane_pairing.rs
  deleted; provider.rs/provider_pairing.rs in place.

## Gates (all observed this session)

- `cargo test -p velnor-workflow`: lib 826/0, integration 110/0 (15 suites),
  total 936 green. Includes no-legacy mechanical test, checked_in
  byte-for-byte, repeat-generation determinism.
- Contract crate (manifest path): 6/0.
- `cargo clippy -p velnor-workflow --all-targets`: exit 0, 0 errors
  (warnings only; one pre-existing d9-shape attr-gap warning left as is).
- `cargo fmt -p velnor-workflow --check`: clean.
- `cargo check --workspace --all-targets`: ok.
- `policy --workflow-root . --base-revision 08ea1b07`: 10/11 PASS.
  The one failure is `generated-tree`: the PIN binary (old-gen) cannot
  parse the schema-2 tree — exactly the rendezvous-covered case
  (e6839cfa, still unmerged on the campaign branch; see Blockers).
- `generate --plain --force`: clean, 21 files; dogfood template memory
  7.13 MiB < 8 MiB ceiling. `--check` exits 1 via the pin binary for the
  same rendezvous-covered reason (CI D19 guard falls through to the
  head-closure candidate path per §3(c)).
- `actionlint -shellcheck=`: clean, 0 findings. Full actionlint (with
  shellcheck) hung downloading shellcheck in this environment and was
  terminated; all changed `run:` bodies are tip-verbatim shell.
- Legacy sweep (`RunnerMode|RunnerLane|DispatchChoice|LANE_ADMITTED|
  lanes.rs|runners.rs|inputs.lanes|inputs.runner|velnor_labels|
  velnor_trusted_label|github_runner|macos_runner|requires_trusted =|
  automatic_lanes|runners = "|automatic = "`) over src + tests +
  .github-gen + workflows + project.toml: zero hits (one `concat!`-split
  negative assertion).

## Tip behavior carried in provider vocabulary (authored, not in d9)

- Apple executor split: hosted collapsed members partition by macOS
  platform; `verify-github-hosted-apple` + `apple_executor` input/gate
  (port of base `verify-github-apple` mechanism).
- Swift detection: SwiftPM portable (LinuxX64) by default; Xcode scheme
  units + XCFramework consumers Apple-bound (MacosArm64). d9 mapped all
  Swift to macOS; the rebase keeps the tip refinement. New pin test:
  `plain_swiftpm_packages_stay_portable`.
- `ProviderAdmission::info_id` + `Default` (hosted default) for the
  dependency-info step; `UNIT_LANE`/`inputs.lane` → `UNIT_PROVIDER`/
  `inputs.provider`.
- `[[check_profile]]` velnor validation mirrors `[renovate]`: velnor in
  universe + declared `[workflow.selectors.velnor]`; `lanes_input`
  dispatch dropped (scheduled-checks dispatch stays bare).
- Runtime `fingerprint` recipe keys `affected`/`full` (was per-lane).
- Release pins re-pinned ONLY after render-diff review against d9 dumps:
  mold/curl hardening, tag-immutability step, image platform-set check
  (ported — was missing), GHCR quoting, cache-delete bounds, native
  image/provider evolution. All divergences verified tip-origin.
- Size stress test padding 25→20 units (60 callers); the 8 MiB ceiling
  HOLDS per template_memory.rs ("not raising this again").
- Real defect fixed (present in d9 too, d9 tree reproduces the
  actionlint failure): report-velnor-ci-outcomes action input
  `ci_lane` → `ci_provider` to match d9's call sites. Telemetry env
  `VELNOR_CI_LANE` + report `lane` field kept stable (velnor-model
  contract owns that schema).

## Blockers for landing (campaign sequencing, out of scope)

1. Rendezvous e6839cfa NOT merged into 8b7b4ac1. Until it lands, CI
   policy `generated-tree` and the pin-validator `--check` fail on ANY
   render-changing generator PR (v-d2a §3(c) states this). This branch
   is shaped for the post-rendezvous fall-through (pin kept old,
   schema-2 tree, head-closure candidate validates byte-identically).
2. Full actionlint-with-shellcheck could not run locally (stuck
   download); CI runs it.
