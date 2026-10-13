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

## Outstanding

- Increment 2: typed product contract (`platform.rs` evolution).
- Increments 3-8 per goal: discovery, cache/artifact, hosted macOS, Jackin
  migration (#1013 incl. Landlock P1), native provider, proof/cleanup.
- Benchmarks: none yet; set latency goals after first controlled baseline.

## Continue

1. `git checkout integrate/apple-ci-s2`; `cargo test -p velnor-workflow`.
2. Next edit surface: `crates/velnor-workflow/src/s2/platform.rs`.
3. Keep one branch per repo; merge main in, never rebase; `git commit -s`.
