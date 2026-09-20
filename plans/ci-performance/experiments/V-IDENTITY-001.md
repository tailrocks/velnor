# V-IDENTITY-001: working-tree identity and Cargo freshness

Status: **diagnosed; bounded implementation present in the shared checkout; independent final review and clean CI validation pending**.

## Reproduction

Fixture: `/tmp/velnor-identity-kMtqNC/repo`, cloned locally from Velnor
`f0fb1c012adc2b7e604eaab332785eb5bf780caa`; isolated target directory
`/tmp/velnor-identity-kMtqNC/target`. The fixture used the pinned Rust 1.98.1
toolchain and the existing default `tui` debug profile.

1. Clean build stamped revision
   `f0fb1c012adc2b7e604eaab332785eb5bf780caa` and closure
   `b510e2700de756686a995968ad999cbfd1b14d170d21e1a18b3e76ed366875e5`.
2. Appending a comment to tracked `crates/velnor-workflow/src/closure.rs` while
   leaving `HEAD` unchanged triggered one crate compile (18.02 s). The binary
   still reported the old revision and old closure. After committing that same
   file in the fixture, the expected `git ls-tree` digest was
   `0a3252c54330ad14f3984b9cf5e1ff6fc8f632243a0487e2b7f4339dfa44da0e`, while
   the dirty build had reported `b510e...`: the stamped closure described
   `HEAD`, not the source bytes Cargo compiled.
3. Building the committed fixture stamped revision
   `0eb175a4a5e4fd015b085edc44c7f92232141646` and closure `0a3252...`.
4. A no-source-change build with
   `CARGO_LOG=cargo::core::compiler::fingerprint=info` reported:
   `stale: missing .../velnor-workflow-source-sha.always-rerun`, marked the
   build script and both crate targets dirty, and recompiled them. The direct
   Cargo run took 2.57 s (`real`), after the warm target was already built.
   The preceding repeated warm invocation also recompiled (`real` 2.50 s).
5. An unrelated README commit changed `HEAD` to
   `c803e41374be843a7bf25671a14959e479cbf776` but left the closure at
   `0a3252...`; the build recompiled the crate in 2.98 s and stamped the new
   revision. This is avoidable work when the product contract accepts equal
   closure identities.

The exact verbose Cargo evidence is retained in the temporary fixture's
`noop-cargo-2.log`, `docs-build.log`, and `fingerprint.log`. No shared branch,
index, or generated workflow was changed.

## Root cause

`build.rs` hashes `git ls-tree -r HEAD` while Cargo compiles the working tree.
The missing-file `cargo:rerun-if-changed` sentinel deliberately forces the
script stale on every invocation. Because the script emits `rustc-env` values,
Cargo recompiles the library and binary even when all values are unchanged.
`SOURCE_SHA` also makes unrelated commits invalidate the executable despite an
unchanged source closure. The architecture conflates provenance (`HEAD`) with
compiled-content identity and has no explicit dirty-tree state.

## Remedies

1. **Working-tree content identity plus separate provenance (recommended).**
   Move canonical closure hashing into a shared, no-cycle identity helper used
   by `build.rs` and runtime. Hash the actual closure files (path, type/mode,
   and bytes), including tracked dirty and relevant untracked files, with the
   same feature/profile footer. Stamp `SOURCE_CLOSURE` from that digest and
   expose `SOURCE_REVISION` as the actual `HEAD` when known plus an explicit
   dirty marker. A clean immutable producer requires a clean tree; local Cargo
   builds remain usable and cannot falsely claim the clean HEAD closure. This
   preserves exact artifact binding while removing the stale identity bug.
2. **Explicit build receipt input.** Have the existing build/runtime wrapper
   compute the working-tree digest and pass `VELNOR_WORKFLOW_CONTENT_ID` and
   provenance through the environment; `build.rs` validates and embeds them.
   Direct Cargo development would need a fallback filesystem digest, so this
   cannot be wrapper-only. A typed receipt can also retain requested pin,
   actual build revision, closure, platform, features, and profile for artifact
   consumers.
3. **Input-driven Cargo freshness without the sentinel.** Enumerate every
   closure file and emit `cargo:rerun-if-changed` for each, plus the minimal
   Git metadata files needed to observe a changed provenance revision. Re-run
   only when an input changes, and use the closure digest as the compile-time
   identity so docs-only commits reuse the binary. This removes the unconditional
   build-script execution, but dynamic Git ref tracking and untracked files
   need careful fail-closed handling; it must not be the sole content identity.
4. **Separate binary and manifest.** Stop embedding the changing `HEAD` in the
   executable; write provenance to a sidecar build receipt and bind consumers to
   the immutable closure digest. This maximizes reuse but changes the existing
   `--revision` contract and is only safe after every policy and artifact path
   consumes the receipt.

The strongest incremental implementation combines (1) and (3): real
working-tree closure identity, an explicit clean/dirty provenance field, and
file-driven Cargo invalidation. It must preserve a distinct actual build
revision from a requested reviewed revision and never accept a dirty local
binary as an immutable release product.

## Canonical evidence and acceptance boundary (2026-09-20)

The reproducibility values and raw-log fingerprints are recorded in
[`identity-20260920.json`](../observations/identity-20260920.json). The
fixture remains at `/tmp/velnor-identity-kMtqNC`; the JSON records the source
log paths and SHA-256 values so the ephemeral fixture can be checked when it
is available.

The bounded implementation now exists in the shared checkout. Focused local
proof passed: identity helper tests (2), old policy tests (78), source-2
policy tests (38), and old closure tests (17); library Clippy passed with
`-D warnings`. These are implementation checks only. Independent parent
review, all-target Clippy, clean generated-tree/actionlint validation, and
real CI remain pending. No Cargo sentinel removal, timing claim, immutable
runtime release, or campaign acceptance follows from these local results.

The implementation preserves the clean v1 Git-tree digest and adds a typed
local worktree binding. A dirty local binary remains ineligible for external
candidate provenance; the producer must still prove a clean checkout,
manifest identity, binary digest, and attestation.

The follow-up Cargo audit found two unrepresented source classes and repaired
them in the helper: source symlinks resolving outside the closure, and ignored
files under nested `target` directories. Actual Cargo fixtures changed the
binary value while the old Git lines stayed equal; the repaired helper rejects
both. Root `target/` remains the only output prefix accepted by the predicate,
and the existing `CLAUDE.md -> AGENTS.md` link remains valid through the
general in-closure target rule. This repair has focused local proof only;
independent all-target and real-CI validation remains pending.
