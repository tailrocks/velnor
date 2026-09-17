# Prepared-tool handoff, slice B phase 2: handoff wiring

Date: 2026-09-17. Base: `33688938`. Phase 1 (core types + verifier +
taxonomy + bounds, `primitives/prepared_tools.rs` ~1356 lines) copied
unchanged from the retained phase-1 worktree; phase 2 wires it into the
real chain. Worktree left dirty for the integrator.

## What phase 2 added

Declaration. New `[[declare]]` unit-contract primitive `prepared-tool`
(schema `tools`, `recipes`): `tools` maps tool id to authorized producer
slugs, `recipes` maps tool id to build recipe. The primitive validates
slugs, discovers each scoped unit's governing lockfiles, binds an inputs
digest, and records one `PreparedToolNeed` per tool on the unit. Empty
`units` means every unit, like other contract rows.

Discovery. `governing_locks(files, unit_root)` walks the scan's file
list: nearest ancestor-or-self lock per kind, filenames as vocabulary
only (`mise.lock`, `Cargo.lock`, `bun.lock[b]`, `package-lock.json`,
`Package.resolved`, `.terraform.lock.hcl` — each already known to a scan
detector). Below-root matches only `SwiftPM`'s tool-owned `.swiftpm/`
subdir, which the scan reads like the package root. No estate paths.

Binding. `inputs_digest` is canonical SHA-256 over governing lock bytes,
declared recipe, and toolchain pin. Platform ABI renders into keys as a
runner expression and is proven by the manifest check. Trust boundary:
read path (producer identity + API outcome + integrity) is enforced at
install; write path (trusted-event save gate) belongs to the producer
slice — see wiring points. `GENERATOR_REVISION` untouched (`49`).

Consumer rendering. `render_consumer_steps` emits per need: cache
restore (exact current-run key + historical fallback prefix as two
distinct values) and an install step that fails closed as a miss without
a manifest, else runs the runtime verb and prepends `bin/` to `PATH`.

ir.rs integration (two paths). The live kind path unions distinct need
records across lane members in `render_collapsed_lane_steps`: one
restore+install block per record, ungated when unanimous, else gated by
`contains()` over the new `prepared_tools` caller input
(`tool:digest:producers` records via `LaneStepFacts`, `unit_lane_facts`,
`input_values`). The kind header declares the input only when a member
needs it, so undeclared headers are byte-identical. The legacy per-unit
path renders the same steps from `render_tool_provisioning`.

runtime.rs integration. New `prepared-tool-install` verb: read manifest
(absent is a miss) and arrived files, `classify_and_verify` (resolve for
exact-vs-fallback + keys, verify for proof — a mismatched manifest gets
the precise corrupt/denied verdict, never a generic miss), API outcome
check for historical bundles only (exact hits skip it), atomic install,
per-file audit lines, and `GITHUB_OUTPUT` records
(`requested-key`/`resolved-key`/`is-exact`/`save-key`/`outcome`). The
outcome fetch pages no listing (single-run GET consumes one result
page); attempts and total waits are bounded by `TransferBounds::DEFAULT`
with 1s/2s/4s… backoff, and `curl` runs `--retry 0 --max-time 30` so the
Rust loop is the only retry authority. `--check-save-key` gates a save
step: verifies, then allows/denies the candidate key via `save_is_legal`
(fallback bytes under the requested exact key fail as corrupt, at save
time). API taxonomy: 2xx ok, 404/410 miss, 429 or rate-limited 403 or
5xx transient, anything else denied; header-aware 403-vs-rate-limit
split.

Retention. `RetentionPolicy::with_prepared_tools` appends the
`prepared-tools` class (marker `prepared-tool-v1-`, 256 MiB, two
generations: live + fallback). `default_policy` is untouched; the
runtime's `retention_policy_for_plan` extends only when the discovered
generation config declares a `prepared-tool` row.

`allow(dead_code)` removed. Every phase-1 item now has a prod caller
(`is_retryable` drives the retry loop, `check_pages(1)` floors the
fetch, `save_is_legal` backs `--check-save-key`, `manifest()` and
`install_plan()` feed the verb's outcome check and audit lines).

## Proof

`cargo test -p velnor-workflow`: 536 lib + 57 integration, all green,
including the pre-existing goldens and the deny-list test. `cargo clippy
-p velnor-workflow --all-targets`: 0 findings. `cargo fmt --check`:
clean. New tests: discovery layouts/ties, digest stability/sensitivity,
declaration shapes, classify verdicts (wrong-producer/wrong-ABI/
incomplete/tampered), API matrix + rate-limit headers, fetch
retry/exhaust/refuse/budget/unreadable, key/record/outputs separation,
consumer-step pins/flags/miss text, verb installs + save-key gate +
historical outcomes + curl split, retention class + conditional, kind
union/gates/header, contract-phase binding incl. relock sensitivity,
plus binary-level declared/undeclared integration tests. Undeclared
output proven byte-identical: base vs new binaries `diff -r` clean on
all three fixtures (`synthetic-workspace`, `synthetic-release`,
`polyglot`); `Pins` untouched.

## Files touched

- `src/primitives/prepared_tools.rs` (new, ~2900 lines): phase-1 core
  plus discovery, digest, declarations, classify, outputs, API
  taxonomy, outcome fetch, key expressions, consumer rendering,
  `PreparedTool` primitive, all unit tests.
- `src/primitives/mod.rs`: `PREPARED_TOOL` id, registry, contract
  classification, module visibility (allow removed).
- `src/primitives/ir.rs`: kind union + gates + conditional header,
  `lane_input::PREPARED_TOOLS` + `contains_gate`, `LaneStepFacts` field
  + caller values, legacy provisioning hook, render tests.
- `src/runtime.rs`: `prepared-tool-install` verb (+ `--check-save-key`,
  `--curl` seam), curl executor + response split, retention
  conditional, all verb tests.
- `src/primitives/snapshot.rs`: `with_prepared_tools` + test.
- `src/lib.rs`: `Unit.prepared_tools` (`skip_serializing`, invisible
  to pinned Planning runtimes) + literal updates.
- `src/scan/*`, `src/tui/*`, `src/primitives/{release,runtime_products}.rs`:
  `prepared_tools: Vec::new()` on `Unit` literals only.
- `tests/prepared_tool_handoff.rs` (new): binary-level declared +
  undeclared tests.

## Wiring points for the integrator

1. Producer save step (producer slice): gate on trusted events AND the
   producer-build output, call `velnor-workflow prepared-tool-install
   ... --check-save-key "$KEY"`, then save the cache entry under the
   `save-key` output it records — never under a rebuilt requested key.
   That renders the trusted-event write path promised above.
2. Producer manifest stamping: stamp `manifest_sha256` via
   `ToolManifest::canonical_digest` over the exact fields phase 1
   covers; producer run id must equal the saving run id or no consumer
   save-check will ever allow it.
3. `mise_lock_keys_for_root` per-unit gap (`config/mod.rs`): reuse
   `governing_locks` to consult nested locks; deliberately left
   untouched here (validation surface, separate goldens).
4. Optional hardening, not required: a manifest `trust_boundary` field
   the consumer requires as `trusted`. manifest shape change; phase-1
   tests would need fixture updates.
5. `check_historical_outcome`'s GH_TOKEN read lives in the verb's
   historical branch only; exact-path installs need no token. The
   `--curl` flag names the HTTP client.
