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
  purpose; 5 new tests, lib 1899 green). 4b exact native-product
  identity (transitive input digest + per-file output manifest +
  validation); 4c staging + binding-drift check before install;
  4d verified cross-job artifact transport; 4e per-layer
  cache-state reporting (`snapshot.rs`). Follow-ups: `.build`
  intermediates need multi-layer cache support; scoped
  `derivedDataPath` per unit; runtime actual-Xcode probe (needs
  macOS runs).
- Increments 5-8 per goal: hosted macOS, Jackin migration (#1013
  incl. Landlock P1), native provider, proof/cleanup.
- Benchmarks: none yet; set latency goals after first controlled baseline.

## Continue

1. `git checkout integrate/apple-ci-s2`; `cargo test -p velnor-workflow`.
2. Next edit surface: 4b exact native-product identity —
   `s2/platform.rs` (identity/output manifest), `s2/scan/rust.rs`
   (input closure facts).
3. Keep one branch per repo; merge main in, never rebase; `git commit -s`.
