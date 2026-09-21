# Apple CI in `velnor-workflow`: progress and handoff

Branch: `integrate/apple-ci-s2`. Goal: generic macOS/Swift CI (typed
scanner, product graph, planner, runtime) + Jackin migration off its opaque
`swift-package-native-ci` task. Research snapshot: 20 Sep 2026.

## Tested SHAs

- Velnor base: `59396040` (main, PR #982).
- Jackin base: `fce94cea` (main); PR #1013 head `997c18fe` (open,
  mergeable/blocked; Landlock P1 thread unresolved, no author reply).
- Evidence: run `35535696169` head `6713d014`, conclusion `cancelled`;
  native job attempt 2 ok (16m25s, 853.7s in `desktop xcframework`),
  attempt 1 killed by the 600s stall guard. Raw logs/JSON in `/tmp/ci-evidence`.

## Decisions

- Increment 1 replaces output-silence-as-failure with `exec::RunLimits`
  (`stall_warn` diagnostic + `wall` deadline), shared by schema-1 and
  schema-2 runtimes. Stall knob `VELNOR_RUN_CMD_STALL_SECS` kept,
  now warn-only; new `VELNOR_RUN_CMD_WALL_SECS` (default 3600).
- Children run in their own process group (`process_group(0)`, no `unsafe`);
  termination validates the group, then SIGTERM/grace/SIGKILL via rustix
  (added to `[target.'cfg(unix)'.dependencies]`).
- `try_wait` only peeks (waitid/WNOWAIT): every exit path reaps via `wait()`.
  Post-exit pipe drain uses a 1s idle window + 10s total cap so
  squatter grandchildren cost 1s, not 10s.
- `exec` returns message strings; each runtime wraps in its own private
  `GeneratorError` (s1/s2 error types are distinct).
- Increment 2 adds `outputs` (normal-form repo-relative paths) to s2
  `NamedProduct` + `[[units]]` product rows. `resolve()` runs
  `validate_product_graph` before materialization: normal-form checks,
  one-producer-per-path, self-edge and cycle rejection with closed paths.
  Unknown producer/product errors keep naming known units/offered products.
  Schema-1 product types intentionally untouched. Lib: 1841 tests green.

## Outstanding

- Increment 3: Swift/XcodeGen discovery without code execution. 3a done
  (`581e0d42`: static Package.swift facts, build-only units, local vs
  remote binaryTarget). 3b done (`24d225c6`: structural XcodeGen spec
  recognition, include closures with cycle/escape diagnostics, app +
  scheme selection, committed-project dedup; lib 1876 green, no
  Jackin literals in added lines). 3c done (`f4a4faef`: boltffi.toml
  producer facts mirroring boltffi_cli 0.30.1, path+module join to
  local binaryTargets, taskless NamedProduct/Prerequisite edges,
  conflict/mismatch/escape diagnostics; 16 new tests, lib 1892
  green, no Jackin literals). 3d done (`10709118`: colliding Xcode
  scheme unit ids gain their container path; lone schemes keep
  existing ids; 2 new tests, lib 1894 green). Increment 3 complete:
  static Package.swift/Xcode/XcodeGen/BoltFFI discovery with
  product edges, no project-code execution, no Jackin literals.
- Increment 4: cache/artifact execution. Grounding done: `Unit.cache`
  is one `CacheSpec` (`s2/mod.rs:774`: key_files/paths/purpose);
  `NamedProduct` is name/task/env/outputs (`s2/platform.rs:30`);
  handoff tests live in `tests/selection_artifact_handoff.rs` and
  `tests/prepared_tool_handoff.rs`. Slices: 4a done (typed
  `SwiftPmSources`/`XcodeIntermediates` purposes, `mise.lock` /
  `.swift-version` / `.xcode-version` key boundary, retention
  recognizes `swift-` keys in `unit-caches`; both layers share the
  kind-level `swift` key segment until callees partition by
  purpose; 5 new tests, lib 1899 green). 4b-i done (`89bee89d`):
  transitive Rust path-closure walker emits validated glob
  `inputs` + toolchain/lockfile pins into `NamedProduct`
  `inputs`/`inputs_unknown` at join time; unknown build-script or
  out-of-repo inputs fail closed; 10 new tests, lib 1909 green.
  4b-ii done: `InputsFacts.sources` extends the prepared-tool
  digest to closure bytes (golden pin proves empty-sources
  backward compat); scan expands closure globs over tracked
  files and stores hex `inputs_digest` on producer/product
  (`None` + diagnostic on gaps); `resolve` rejects malformed
  digests and digest-with-gaps; transitive edit invalidates,
  consumer-only edit preserves; 8 new tests, lib 1917 green.
  4b-iii done: `[targets.apple]` arch lists + `include_macos`
  parsed (multi-line arrays); slice dirs derived per verified
  BoltFFI 0.30.1 defaults; `output_files` (Info.plist, per-slice
  lib + modulemap) on producer/product; `resolve` enforces
  normal-form, dedupe, under-root containment; unknown arch
  and zero-slice configs fail closed with diagnostics; 8 new
  tests, lib 1925 green. 4c-i done: binding facts —
  `[targets.apple.swift] output`/`ffi_module_name`,
  `[targets.apple.spm] layout`, `[targets.apple]
  deployment_target` parsed; `bindings_dir`/`bindings_file`
  resolved per verified `generate swift` rules (split appends
  `BoltFFI`, stem `{PascalCase(crate)}BoltFFI.swift`);
  unknown layout / escaping output fail closed; FFI module
  honors `ffi_module_name`; facts flow to `NamedProduct`
  with together-or-empty validation; 8 new tests, lib 1933
  green. Recipe + join + tool done: `BoltffiRecipe`
  (profile/locked/verbose, digest excludes verbosity, wipe
  before pack, `--locked` iff Cargo.lock governs) renders
  producer commands; join escalates producer to
  macOS-arm64 + native cap and appends recipe once per
  product; `needs_boltffi`/`cargo:boltffi_cli` install +
  generation-time lock validation mirror nextest; 6 new
  tests, lib 1939 green, clippy clean. Drift done:
  `BoltffiDriftSurface` (bindings dir + generated
  `Package.swift` unless skipped; headers live in
  upstream scratch) with snapshot/diff render around the
  pack — `pack apple` has no output redirect, verified
  against pinned `boltffi_cli` CLI; 4 new tests incl a
  behavioral shell test, lib 1948 green, clippy clean.
  4d done (`d7544a3d`): cross-job product transport with
  guarded rebuild. 4e done: per-layer cache-state reporting
  — schema v3 `cache_outcomes` (`disabled`, `not_run`,
  `miss`, `compatible_seed`, `exact`, `invalid`, `saved`,
  `unknown` fail-closed); generator renders
  `cache_declared_layers` + per-layer step-outcome inputs;
  Rust decision-table oracle in `snapshot.rs` (test-only,
  the classifier ships in bash); report action rewritten
  (source + owned output); 5 workflows regenerated (+18
  lines, purely additive); lib 1995 green, workspace
  clippy/fmt clean. Runner note: reverted out-of-scope
  working-tree curl-transport edits in
  `scaleset/client.rs` (preserved outside the repo for
  separate review) — HEAD runner is green; the E0282 came
  from those edits, not from main or the merge.
  Next: Increment 5 hosted macOS integration.
- Merge `5cba78ac` (main #992 staged full-tree replacement):
  one conflict (generator-state scan hash), resolved by
  regeneration (hash-only change, no workflow edits); lib
  2015 + full_tree_replacement 35 green, clippy/fmt clean.
- Increment 5 scoping (verified, not assumed): macOS-ARM64
  runtime already publishes natively on macos-26
  (`ci-runtime-products.yml` matrix) and the setup action
  consumes `.products[macOS-ARM64]`; main routes hosted
  Apple jobs to macOS 26 (`d20d4d1d`); schema-1 Swift unit
  renders `swift build` + `swift test` with no Xcode
  selection/probe; no `[apple.toolchain]` policy, no Xcode
  build identity in cache keys, no s2 native e2e fixture.
  Breakdown: 5a Xcode toolchain contract (typed policy,
  DEVELOPER_DIR selection, version probe, cache/telemetry
  identity); 5b s2 native e2e fixture + generation test
  (SwiftPM + XcodeGen + BoltFFI producer ordering, ABI,
  artifact needs); 5c real hosted clean/warm runs with
  evidence and gate correctness.
  Follow-ups: `.build`
  intermediates need multi-layer cache support; scoped
  `derivedDataPath` per unit; runtime actual-Xcode probe (needs
  macOS runs).
- Increment 5a landed: `XcodeToolchain` type + `Unit.xcode`,
  root `.xcode-version` parse (MAJOR.MINOR[.PATCH], malformed
  fails scan), hosted Swift `Select Xcode <pin>` probe step
  (exact match else newest prefix, fail-closed, exports
  `DEVELOPER_DIR`, records `xcodebuild -version`), xcode in
  `CompatibilityFacts` digest (one-time v3 rotation,
  regen diff verified digest-only, no probe in own
  workflows), 7 new tests; lib 2022 + full_tree_replacement
  35 + runner 2386 green, clippy/fmt clean.
- Increment 5b landed: `tests/fixtures-s2/swift-native`
  (SwiftPM consumer + XcodeGen app + BoltFFI producer,
  renamed, no Jackin literals) generates 3 macOS units
  with product edge, guarded rebuild before
  `swift build`, and Xcode probe; `swift_native_e2e`
  (graph/ordering/ABI/placement, FFI-edit selection of
  producer + consumer, byte-identical regen). Structural
  fix: `toml_escape` in `s2/mod.rs` — the project.toml
  writer emitted raw newlines from multi-line commands,
  producing TOML `plan` cannot parse; self-regen diff
  verified empty. Lib 2022 + full_tree_replacement 35 +
  swift_native_e2e 3 green, workflow fmt clean; workspace
  clippy red only on branch scaleset dirt in
  `runner/scaleset/client.rs` (peer scope, untouched).
- Increment 5c in progress: drift-surface inputs seeded
  (committed bindings + Package.swift are producer inputs,
  so a bindings-only edit selects the producer;
  `6146879a`); fixture carries real boltffi 0.30.1 pack
  outputs + resolved lock (`.gitignore` negation for the
  fixture `dist/`, `fe596d81`, fresh-worktree e2e 3/3);
  fixture is now executable (SwiftPM sources + test,
  XcodeGen app sources + Info.plist + test host with
  event loop). Local full-graph run green on arm64 Mac
  (Xcode 27.0): pack 7.5s, drift diffs clean, one
  arm64 slice, `swift build` ok, XCTest 1/1 (serial
  verified; `--parallel` gate honest via negative
  control exit 1), xcodegen + xcodebuild build + test
  exit 0. Notes: CLI summary `runners=` line shows
  the base provider, not the macOS-specialized
  placement (generated files are correct); fixture app
  does not yet link the XCFramework (Xcode product
  edge is a follow-up). Hosted: run `35563785464`
  (Policy) failed only on candidate-publish starvation
  after push-cancels, not on code; awaiting a clean
  uninterrupted run on the final SHA.
- Increments 5-8 per goal: hosted macOS, Jackin migration (#1013
  incl. Landlock P1), native provider, proof/cleanup.
- Benchmarks: none yet; set latency goals after first controlled baseline.

## Continue

1. `git checkout integrate/apple-ci-s2`; `cargo test -p velnor-workflow`.
2. Next edit surface: 5c real hosted clean/warm runs with
   evidence and gate correctness.
   Verified layout facts
   (BoltFFI 0.30.1 @ `2e6320a`): slice dirs
   `macos/ios-{archs_joined}[-simulator]`, structural files
   `Info.plist` + per-slice `lib{crate}.a` (underscored) +
   `Headers/module.modulemap` declaring `module {ffi} `,
   exactly one lib, lipo arch match
   (Jackin `desktop.rs:758-789`).
3. Keep one branch per repo; merge main in, never rebase; `git commit -s`.
