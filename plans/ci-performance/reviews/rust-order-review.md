# Rust validation order review

Review date: 2026-09-20. Scope: V-RUST-ORDER-001, its five-sample fixture
measurements, and the commands emitted for the active Velnor lane. Read-only
review; no generator implementation is accepted here.

## Verdict

**HOLD for a Velnor performance claim; candidate Clippy-first is suitable for
a controlled source experiment.** The fixture shows a large time-to-lint
failure reduction when Clippy runs before nextest. Its successful-path result
is effectively tied on a tiny project. The original fixture includes
`--no-deps` and invokes Cargo directly, while the active Velnor generated
commands invoke Mr. Boxington and remove `--no-deps`. Therefore the five
samples establish a failure-order mechanism, not a Velnor result.

## Command identity

Both Rust scanners construct the same source order:

```text
fmt
nextest (or cargo test when nextest is unavailable)
clippy --profile test --no-deps --all-targets --all-features ... -- -D warnings
```

The active Velnor generated config is different after provider rewriting. For
example, `rust-unit-collector` in `.github/ci/project.toml` emits:

```text
cd -- 'tools/unit-collector' && mbx fmt --manifest-path 'Cargo.toml' -- --check
cd -- 'tools/unit-collector' && mbx nextest run --locked --all-features --package 'unit-collector' --no-tests pass
cd -- 'tools/unit-collector' && mbx clippy --locked --profile test --all-targets --all-features --package 'unit-collector' -- -D warnings
```

There is no `--no-deps` in the generated Velnor command. This is deliberate
in the current implementation: `s2::mbxify_cargo_invocations` changes Cargo
subcommands to `mbx` and `strip_mbx_no_deps` removes `--no-deps` from `check`
and `clippy`, because the wrapper does not expose that Cargo option. The
legacy path has the same transformation. The source test asserts this exact
rewrite. The fixture's `--no-deps` measurements cannot be mapped directly to
the active Velnor lane.

The prerequisite path is a separate contract. For a selected Rust dependency
that is not a full unit, `prerequisite_commands` keeps only Clippy, rewrites it
to `check`, removes `-D warnings`, and removes `--no-deps` for the Velnor
wrapper. A full-unit order change must leave this path and its obligations
unchanged.

## Evidence review

The original controlled fixture is a lockfile-backed, dependency-free library
and binary with two integration tests. It uses five fresh target directories
per order and retains `--profile test`, `--all-targets`, `--all-features`,
`--no-deps`, and nextest `--no-tests pass`. Existing reported medians were:

| fixture | nextest then Clippy | Clippy then nextest | observation |
| --- | ---: | ---: | --- |
| green | 1.162 s | 1.164 s | same small-project success path within noise |
| deliberate Clippy warning | 1.780 s; tests ran | 0.429 s; tests did not run | clear synthetic fail-fast effect |

Those numbers are useful mechanism evidence. They do not measure Velnor's
`mbx` wrapper, its dependency graph, or a workspace-sized target.

The original five-sample runs shared this development host with concurrent
Cargo builds. The nextest-first failure range reached 8.984 s while the
Clippy-first range was 0.420–0.453 s. That load is a confounder for an
absolute timing comparison. It does not invalidate the sequencing observation
that the second order avoids starting nextest after a known lint failure, but
it cannot support statistical acceptance or profile equivalence.

I independently replayed the fixture with the generated Cargo flag shape and
without `--no-deps`, using five fresh target directories per order. The
replay used the pinned fixture toolchain and nextest 0.9.143; it did not claim
to reproduce MBX cache behavior. Raw per-command logs are under
`/private/tmp/velnor-rust-order-actual2-target-*`.

| fixture | order | median wall | range | work observed |
| --- | --- | ---: | ---: | --- |
| green | fmt → nextest → Clippy | 1.373 s | 1.302–1.642 s | nextest compiled once; Clippy produced no compile line |
| green | fmt → Clippy → nextest | 1.440 s | 1.359–1.490 s | nextest compiled once after Clippy |
| Clippy warning | fmt → nextest → Clippy | 1.447 s | 1.365–1.555 s | nextest passed, then Clippy failed |
| Clippy warning | fmt → Clippy → nextest | 0.325 s | 0.307–0.380 s | nextest was not invoked |

The corrected replay preserves the same direction: Clippy-first reaches the
lint failure about 1.12 s earlier on this tiny fixture. It also shows a
roughly 67 ms green-path median cost in this environment. The changed values
versus the original replay are expected from tool-wrapper, toolchain, and
machine state; they make an absolute Velnor speed claim less defensible.

The fixture has no dependency graph and no MBX object transport. It cannot
answer whether a real Velnor unit's test-profile artifacts are reused, whether
Clippy checks dependencies through MBX, or whether a large workspace pays a
second compile. The success result must remain a cost-risk observation, not a
proof of equivalence.

## Candidate experiment

The decisive bounded candidate is a source-only order change in both Rust
scanner copies:

```text
fmt
clippy --locked --profile test --all-targets --all-features [package] -- -D warnings
nextest --locked --all-features [package] --no-tests pass
```

If nextest is unavailable, retain the scanner's exact `cargo test` fallback
as the third command. The candidate must preserve every command and argument:
package or manifest selector, lock mode, all features and targets, test
profile, warning denial, and nextest's empty-test success behavior. The
provider rewrite must continue to produce the same `mbx` command shape; do
not reintroduce `--no-deps` into an MBX command that cannot accept it.

The candidate must also leave the prerequisite path unchanged: a partial
dependency unit still receives its one transformed `check` obligation, and
full units still execute all three commands exactly once. No command-vector
filter, implicit stage omission, or provider-specific coverage exception is
part of this experiment.

Acceptance evidence requires at least ten fresh successful cohorts and ten
deliberate Clippy-failure cohorts using the actual generated Velnor commands,
with the same runner, toolchain, MBX version, cache namespace, source, and
selection scope. Capture command start/end markers, compile counts, Cargo or
MBX hit/miss/bypass output, nextest test counts, total wall time, and billed
runner time. Include a format-failure control and a prerequisite-only unit.
Accept only if the failure-order gain survives those controls and the
successful path's total work and wall-time cost are measured separately.

The current order remains the safe baseline until that run. A split Clippy
and test job is a separate queue/cache experiment; it is not a consequence of
this fixture.
