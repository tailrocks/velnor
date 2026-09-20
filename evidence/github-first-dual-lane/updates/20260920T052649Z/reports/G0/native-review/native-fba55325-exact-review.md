# Independent exact native mode-successor review — `fba55325`

Review timestamp: 2026-09-20T05:18:00Z

## Verdict

**REJECT / changes required.** The exact successor carries the executable-mode
fix through the generated producer and stable assembly, and its source fixture
now creates executable siblings and a valid executable archive. The mode gate
is not fail-closed for hostile downloaded inputs: the release path runs
`chmod 0755` before `test -x`, so a `0644` artifact is repaired and admitted.
Its negative fixture checks only a local archive-listing helper, not the actual
Homebrew consumer or a rendered producer rejection path. Existing independent
APT, preview, and runtime-publication blockers remain.

No source, host runtime, install, dispatch, release, publication, or consumer
repository operation was performed. The review used a detached exact worktree,
read-only GitHub API inspection, and focused source tests. It is not a G2/G3
gate approval.

## Immutable candidate

- Repository: `tailrocks/velnor`
- Branch: `codex/github-first-native-product-v3`
- Candidate: `fba55325e9242986b4e21cef40e84010c87168cc`
- Parent/source fix: `f1bf2397ba1a93444ead42358a29a8682fe0e0c7`
- `git ls-remote origin refs/heads/codex/github-first-native-product-v3` equals
  the candidate.
- `git diff --check 384e2e4e..fba55325e` passed.
- The candidate commit is the generated D19 pin bump to `f1bf2397`; the source
  mode change is in `f1bf2397`, whose parent is the prior exact `384e2e4e`.

## Capability matrix

| Capability | Exact evidence | Verdict |
|---|---|---|
| Producer upload boundary | Each native build copies both siblings, sets `0755`, then checks `test -x` before artifact upload (`.github/workflows/native-product.yml:121-145`, `:181-185`, `:221-225`; preview equivalent `native-product-preview.yml:141-145`, `:181-185`, `:221-225`). | **PASS for valid producer output** |
| Download and staging boundary | Stable assembly uses source-bound artifact paths, rejects symlinks, applies `chmod 0755`, checks executable state, copies into `product-assets`, reapplies `0755`, and checks again (`.github/workflows/release.yml:4422-4453`; generator source `release.rs:2237-2244`). | **PARTIAL: repairs wrong mode before checking** |
| Archive staging | Each archive binary is checked after the same `chmod 0755` normalization, copied to the archive directory, normalized again, and checked before `tar` (`.github/workflows/release.yml:4454-4471`; generator source `release.rs:2245-2252`). Valid `0755` members therefore reach tar. | **PASS for valid input; not fail-closed** |
| Hostile wrong-mode rejection | The exact current Homebrew consumer rejects a member without owner execute permission (`dual-lane-homebrew/scripts/package-update.sh:545-552`). The candidate's fixture sets source files to `0755`, asserts tar listing, then changes an extracted archive to `0644` and calls only local `archive_has_executable_members` (`release.rs:7054-7113`). It does not run `package-update.sh`, and the production download/archive steps would `chmod` the wrong-mode input first. | **FAIL / residual** |
| Candidate render fixture | `fs::set_permissions(..., 0o755)` now models executable producer siblings; the rendered shell is executed, runner manifest/contract verification runs, product binaries are mode-checked, and archive member modes are inspected (`release.rs:6903-6909`, `:6957-7014`, `:7015-7073`). | **PASS as source fixture only** |
| Typed manifest/provenance | No candidate diff touches `velnor-runner/src/product.rs`, `velnor-runner/src/release.rs`, or either native product contract. Existing typed fields and runner verification remain (`product.rs:29`, `:70-112`, `:540-638`; release assembly `release.yml:4466-4508`). | **PASS / unchanged** |
| Homebrew exact consumer | Prior exact `384e2e4e` bytes were rejected by the real consumer because all archive members were `0644` (evidence `native-384-exact-crosscheck.md`, SHA `ff6f37e380efc7eeb5721b654436d533ab5e854f179a824c01456a4a93f146a0`). A separate exact fba consumer rerun is assigned; this review does not claim that external rerun. | **UNAVAILABLE here; no approval** |
| APT parent binding | Current APT ref `f679a1ce94627281a99e4b887fe26fc1cad33409` requires `.parent_manifest_sha256` in the record and release/compiled manifests (`dual-lane-apt/scripts/release-discovery.sh:379-430`, `:464-482`). Candidate `ReleaseRecord` remains only `schema`, `build`, and `architectures` (`crates/velnor-runner/src/release.rs:343-346`); stable output still emits no APT parent field. | **FAIL / blocking** |
| Immutable preview product | Preview publication still requires native build, rejects nonempty blocked targets, and publishes only `SHA256SUMS` plus `release-manifest.json` and two Debian assets (`.github/workflows/preview.yml:927-1135`). The native preview workflow still unconditionally fails blocked Intel (`native-product-preview.yml:17-27`). | **FAIL / blocking** |
| D19 runtime publication | f1bf closure reproduced with the checked-in algorithm: `3fbc0878167cf79afcc7f80094b9136eb9a5afc11abf4a572231e7456353abeb`; expected tag `velnor-workflow-runtime-v1-3fbc0878167cf79a`. Read-only GitHub API `GET /repos/tailrocks/velnor/releases/tags/velnor-workflow-runtime-v1-3fbc0878167cf79a` returned HTTP 404 at 2026-09-20T05:12:53Z. Setup action fails closed when this product is absent (`.github/actions/setup-velnor-workflow/action.yml:81-126`). | **FAIL / integration blocker** |

## Mode-specific residual

The producer checks the compiler output before copying, then explicitly sets
the two uploaded siblings executable. This addresses the original fixture's
`fs::write`/`0644` problem. However, GitHub artifact transport can lose mode
metadata; the stable assembly deliberately normalizes the downloaded file with
`chmod 0755` before checking it. That makes a transport repair possible, but
also makes a hostile or malformed `0644` download indistinguishable from a
valid one. The archive check has the same ordering. If wrong-mode rejection is
required, validate the received mode before normalization or carry a trusted
mode/provenance field and reject mismatch; do not rely on `test -x` after chmod.

The local negative fixture proves that its helper recognizes a `0644` tar
member. It does not prove that the rendered workflow rejects malformed
artifact input, nor that the real Homebrew script consumes the exact fba bytes.
The assigned independent Homebrew rerun must supply that proof.

## Focused verification

All commands ran in detached exact worktree `/tmp/velnor-review-fba55325`:

- `cargo test -p velnor-workflow rendered_native_product_assembly_produces_runner_and_homebrew_contract_bytes -- --nocapture` — 1 passed, 1864 filtered.
- `cargo test -p velnor-workflow native_product -- --nocapture` — 6 passed, 1859 filtered.
- `cargo test -p velnor-workflow --lib release -- --nocapture` — 260 passed, 1482 filtered.
- `cargo test -p velnor-runner --lib product -- --nocapture` — 23 passed, 2336 filtered.
- `cargo fmt --all -- --check` — passed.

These are local source/render checks. No external publication, macOS install,
Homebrew rerun, or runtime product availability is inferred from them.

## Bounded follow-up

1. Make the mode contract explicit: distinguish expected artifact-transport
   normalization from hostile wrong-mode input, and add a rendered failure test
   that executes the real consumer or an exact equivalent over the fba bytes.
2. Preserve the current producer `0755` checks and archive member requirement;
   do not weaken Homebrew's executable admission.
3. Close the APT parent digest contract, preview immutable product publication,
   and f1bf runtime product publication at the exact closure/tag above.

No approval, publication, runtime-operability, or gate success is claimed.
