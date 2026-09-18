# ChainArgos pin RE-ANCHOR — ledger C-ca-3 (producer-side, read-only)

## Verdict: ANCHORED-WITH-DRIFT (no exact-build product exists)

Pin commit exists and is verified ancestor-of-record for the whole
immutable runtime-product line, but NO published product was built FROM
the pin itself: the pin predates the immutable-product scheme (~9h).
Nearest verified product is +54 commits ahead. See Consequences.

## 1. Pin commit (gh api, confirmed EXISTS)

- repo: `tailrocks/velnor`
- sha: `1279c4f92c97b75dc4cc627f122e119f8a5eae16`
- subject: `Clean-room velnor-workflow regeneration (fix) (#879)`
- date: `2026-09-16T09:38:50Z` (author Alexey Zhokhov; committer GitHub)
- tree: `0f5a96b4e5545340776e1f88da578e85ca9e5f07`
- parent: `e5b47d476b9814fdfba433b4e86b0fdd75d89d6c`
- note: message cites source `tailrocks/velnor@e05aee6de1d1614d752b4d1b1d26a49ac2c5ef91`;
  commit restores generator-only CI (adds `.github/ci/*`, 9 workflow files).

## 2. Closure + published runtime product

- Immutable scheme introduced AFTER pin: `crates/velnor-workflow/src/closure.rs`
  added by `e2f3f1314` (2026-09-16T15:53:08Z); absent from pin tree.
- All 14 `velnor-workflow-runtime-v1-*` releases descend from pin
  (`compare pin...target` → `status=ahead, behind=0, merge_base=pin` for all).
  Ahead counts: +54,+56,+58,+60,+62,+64,+67,+69,+72,+74,+81,+83,+86,+87.
- Nearest product (minimal audit drift + earliest published):
  - tag: `velnor-workflow-runtime-v1-203eb9b79141e5a2`
  - built from: `1bc72627df71ad4709bf20586485bac4a4a36745`
    (`chore(ci): bump D19 pin to d2523b16`, 2026-09-16T18:31:42Z)
  - published: `2026-09-16T18:33:48Z`
  - closure: `203eb9b79141e5a20683d4315ca7cd3c8c04680f3e4017f38f8fb4d3a79fcde8`

## 3. Manifest checks (nearest product) — ALL PASS

- tag-suffix == closure[0:16]: `203eb9b79141e5a2` ✓
- body `built from 1bc72627…` == release target_commitish ✓
- platform assets (3 + manifest): `velnor-workflow-Linux-X64` (5570056 B),
  `velnor-workflow-Linux-ARM64` (4658056 B), `velnor-workflow-macOS-ARM64`
  (4633600 B), `manifest.json` (613 B) ✓
- manifest `products.*.binary` == release asset digests ✓
- downloaded Linux-X64 sha256 `a97a9e895bb0…` == manifest field ✓
- downloaded macOS-ARM64 sha256 `0386e1538a15…` == manifest field ✓
- downloaded manifest.json sha256 `97ad7dc1f431…` == asset digest ✓
- binary self-report (macOS-ARM64, executed): `--revision` →
  `1bc72627df71ad4709bf20586485bac4a4a36745` ✓; `--closure` →
  `203eb9b79141e5a20683d4315ca7cd3c8c04680f3e4017f38f8fb4d3a79fcde8` ✓

## 4. Build provenance attestation — VERIFIED

- `gh attestation verify <Linux-X64> --repo tailrocks/velnor` → exit 0,
  1 attestation (negative control on fake file → 404, as expected).
- SLSA `https://slsa.dev/provenance/v1`; buildType
  `https://actions.github.io/buildtypes/workflow/v1`.
- builder: `tailrocks/velnor/.github/workflows/ci-runtime-products.yml`
  `@refs/heads/feat/ci-immutable-runtime-products`, event `workflow_dispatch`,
  run `35134982294`, runner `github-hosted`.
- resolvedDependency gitCommit `1bc72627…` == release target ✓;
  subject digest == downloaded binary sha256 ✓.

## 5. Anchored identity

`tailrocks/velnor@1279c4f92c97b75dc4cc627f122e119f8a5eae16`
(2026-09-16T09:38:50Z, "Clean-room velnor-workflow regeneration (fix) (#879)")
→ ancestor-of-record for runtime-product line; nearest verified product
`velnor-workflow-runtime-v1-203eb9b79141e5a2`
closure `203eb9b79141e5a20683d4315ca7cd3c8c04680f3e4017f38f8fb4d3a79fcde8`
built from `1bc72627df71ad4709bf20586485bac4a4a36745` (+54).

## 6. Consequences / gaps for C-ca-3

1. EXACT re-anchor impossible: no release targets the pin; ChainArgos audit
   at 1279c4f9 does NOT cover any published binary bit-for-bit.
2. Nearest-product drift: 54 commits (pin 09:38Z → product source 18:31Z same
   day, incl. closure-scheme introduction). Consumer must either re-audit at
   1bc72627 (or Latest `…-9f236b40635970a4`, +87) or accept ancestor-grade
   provenance only.
3. Consumer-side provenance still absent: bastion found no `1279c4f9` /
   ChainArgos references in the local workspace; the binding now lives ONLY
   in this producer-side record until a consumer pins the product tag +
   closure digest.
4. Closes C-ca-3 only as "producer lineage verified, exact product NOT-FOUND";
   recommend follow-up item: ChainArgos re-pin to a published
   `velnor-workflow-runtime-v1-*` tag with closure digest in consumer config.
