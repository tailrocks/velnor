# Independent XCFramework/product handoff review

Date: 2026-09-20

Review target: `tailrocks/velnor`, exact tip `e5cc39059af24d23aceb9543fb335fdabd28fb9d` (`codex/github-first-xcframework-policy`), parent `dfb9ebb5c39ef912770c549c40f077ba41a17ea8`.

Read-only detached tree: `/private/tmp/g1-xcframework-e5-clean` (clean, detached at the exact tip). No source or remote mutation. Native macOS execution and Docker execution were not available; no G3 rollout claim.

## Verdict

Do not approve the handoff yet. The typed config and hosted producer/consumer DAG are present in both legacy and S2, and the exact source/product SHA manifest check rejects ordinary stale or cross-product artifacts. However, the generated consumer cannot materialize a normal artifact: shell-quoted path values are interpolated inside double-quoted path expressions, producing literal quote characters. The focused test only checks strings and misses this.

There is a second fail-closed gap: producer and consumer accept symlink entries (including an absolute symlink target), and the archive census checks names only. A hostile product can therefore pass the path/kind checks while materializing an escaping symlink. Archive hardlink types and duplicate member names are not rejected either. These need explicit regression coverage before approval.

## Contract evidence

- Legacy config validates the typed pair, safe lexical relative path, kind, and required producer task at `crates/velnor-workflow/src/config/mod.rs:1334-1372`; S2 mirrors it at `crates/velnor-workflow/src/s2/config/mod.rs:1078-1116`.
- Legacy and S2 path validators reject absolute paths, `..`, backslashes, controls, and empty segments (`platform.rs:420-433`; `s2/platform.rs:125-138`). This is lexical validation only; it cannot detect filesystem symlinks.
- `materialize_prerequisites` adds producer `depends_on`, exports product env to the consumer, and puts an artifact task only in `producer_prepared` (`platform.rs:654-724`; S2 equivalent `s2/platform.rs:367-437`). The exact generated project confirms `rust-ffi` owns `TARGETS='ios' mise run build-xcframework`, while `swift-package-app` does not contain the build task (`/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/platform-prereq-ffi-artifact-68140-0-out/.github/ci/project.toml:35-68`).
- Hosted caller ordering is present: `artifact_producer_needs` adds only the GitHub producer job (`primitives/ir.rs:3835-3859`; S2 `s2/primitives/ir.rs:3774-3799`). Exact rendered `ci-pr.yml` has `github-swift-package-app` with `needs: [plan, github-rust-ffi]` at generated lines 185-193.
- The prerequisite validator does not reject self-edges or cycles (`platform.rs:588-651`; S2 `s2/platform.rs:296-360`). `artifact_producer_needs` then turns each edge into a hosted caller `needs` entry. A self-edge or A↔B artifact pair therefore renders an invalid GitHub needs cycle; `closure_members` only avoids infinite traversal and is not cycle validation. Add an explicit DAG cycle rejection test before treating the handoff graph as authoritative.
- Producer staging binds `producer`, `product`, `source_sha`, `path`, `kind`, and archive digest (`primitives/ir.rs:1881-1918`; S2 `s2/primitives/ir.rs:2021-2058`). Consumer selection requires all those fields (`ir.rs:1921-1982`; S2 `s2/primitives/ir.rs:2061-2121`). Exact rendered manifest predicate is visible at generated `ci-unit-swift.yml:383-393`.
- Download uses `actions/download-artifact` with current-run pattern `velnor-products-*`; the manifest predicate prevents a different producer/product/path/kind/source SHA from being selected. The code does not explicitly bind `repository`, `run_id`, or job ID in the manifest; current-run action scoping is the implicit run binding. Keep this implicit reliance documented or add explicit run/repository fields if the contract requires independently auditable binding.

## Blocking rendered-shell defect

`crate::shell_quote("target/xcframework/App.xcframework")` is `'target/xcframework/App.xcframework'`. The renderer inserts it into a double-quoted expression at `ir.rs:1975` (and S2 `s2/primitives/ir.rs:2114`):

```sh
destination="$GITHUB_WORKSPACE/'target/xcframework/App.xcframework'"
```

The exact generated output contains this at `ci-unit-swift.yml:404-405`. Extraction writes `target/xcframework/App.xcframework`, but the subsequent `test -e` checks a different path containing literal `'`; a shell reproduction returned `expected-fail`. Producer staging has the same construction at `ir.rs:1911`/S2 `s2/primitives/ir.rs:2051` (`product_dir="$stage/'xcframework'"`). The producer upload still discovers its manifest, but the consumer always fails before checks for a normal artifact.

Narrow fix: keep validated values in shell variables and compose paths from variables (`product_name=...; product_dir="$stage/$product_name"; destination="$GITHUB_WORKSPACE/$path"`), or emit only a raw validated product name for path components. Add a test that executes the generated producer/consumer shell with a real file/directory, not only `contains(...)` assertions.

## Link/traversal/census gap

Producer checks use `test -e`/`test -d` (`ir.rs:1911`, S2 `s2/primitives/ir.rs:2051`), which follow symlinks. Consumer checks enumerate `tar -tf`, allow the expected path and descendants, reject lexical absolute/`..` names, then extract (`ir.rs:1975`, S2 `s2/primitives/2114`). They do not inspect archive member types or link targets.

Reproduction against the exact consumer logic: a producer path symlinked to an absolute outside directory yields a tar listing containing only the expected path; every current name/traversal case passes; extraction restores the symlink; `test -d "$destination"` passes (`symlink-accepted`, `root-is-symlink`). The same source-side acceptance applies before archiving. Hardlink entries and duplicate member names are not rejected by the current census. Add a bounded member census that rejects escaping links (or proves relative in-root links), rejects unsafe hardlink targets, and rejects duplicate member names before extraction. Preserve valid in-bundle framework symlinks only if their targets are proven in-root; blanket rejection may break legitimate framework layouts.

## Dual-lane scope

Artifact upload/download rendering and artifact producer needs are explicitly hosted-only (`ir.rs:4935-4941`, `4989-4994`; S2 `s2/primitives/ir.rs:4935-4941`, `4989-4994`). Legacy validation rejects Velnor-only but permits `RunnerMode::Both` (`platform.rs:557-562`); S2 similarly permits a local provider whenever hosted is also enabled (`s2/platform.rs:261-276`). Thus a portable `file`/`directory` artifact in a dual-lane project can still receive a Velnor consumer with no download/materialization step. XCFramework producer/consumer platform contracts are hosted Apple-only in practice, so this does not create a second XCFramework lane on the current fixture, but the generic typed contract must either reject local artifact consumers or implement a local handoff. Do not claim normal dual-lane fallback for generic artifact kinds until this is explicit.

## Verification

Commands run from the exact detached tree:

```text
rtk cargo test -p velnor-workflow --test platform_prerequisites artifact --locked
  2 passed, 9 filtered out
rtk cargo test -p velnor-workflow platform::tests --locked
  14 passed
rtk cargo test -p velnor-workflow s2::platform::tests --locked
  5 passed
rtk actionlint <generated ci-unit-rust.yml ci-unit-swift.yml ci-pr.yml>
  exit 0
rtk cargo fmt --all -- --check
  exit 0
rtk cargo check -p velnor-workflow --all-features --locked
  finished
rtk cargo clippy -p velnor-workflow --all-features --locked -- -D warnings
  no issues
```

The full `platform_prerequisites` integration file had 8 passed and 3 failed. The failures assert the old `macos-15` label while this exact tip renders `macos-26` (`swiftpm_verifies_on_the_default_executor_while_xcode_needs_macos`, `ffi_prerequisite_selects_the_consumer_and_prepares_the_product`, and `capability_override_moves_a_unit_to_macos`). This is the separately tracked native-host/OS label drift; no macOS host was available, and it is not evidence that the handoff works.

## Required regression matrix

For both legacy and S2 generated workflows, execute the actual shell path with:

1. normal file, directory, and XCFramework-shaped directory: producer task once, upload, download, extraction, consumer command sees the exact path;
2. wrong source SHA, producer, product, path, kind, archive digest, absent archive, and two exact manifests: fail before extraction;
3. absolute/`..` archive member, root symlink, nested escaping symlink, unsafe hardlink, duplicate archive member, pre-existing destination symlink, and self/cyclic prerequisite graph: fail closed;
4. portable artifact with `runners = "both"` / S2 hosted+local provider: either explicit hosted-only generation failure or a real local handoff; never a local consumer with no product;
5. producer failure/skip: consumer caller and aggregate must not report successful product use.
