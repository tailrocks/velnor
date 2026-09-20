# Exact d33 native-product handoff review

Review timestamp: 2026-09-20T03:37:58Z

## Verdict

Complete producer-to-consumer handoff: **REJECT**.

Basename checksum serializer: **PASS**.

The native build lane preserves and validates typed source rows, but the
published canonical/archive JSON drops `feature` and `identity`, which the
current Homebrew consumer requires. The fresh release verifier also omits a
mandatory CLI argument and cannot reach content verification.

## Immutable review snapshot

- Repository: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`
- Branch: `codex/github-first-native-product-v3`
- Candidate: `d33cf6210a1889cdfd548e78b326e11993a384fe`
- Parent: `6c2720f5bf03f7c746b788a1a4a745975a515420`
- Candidate subject: `chore(ci): pin generated workflows to native renderer`
- Candidate remote ref: `origin/codex/github-first-native-product-v3`
- Prior producer baseline reviewed: `2f7d5fba`
- Homebrew consumer snapshot: `6520aad7bd66d53349e040508146956e9c4f0c1e`

`d33` changes the generated workflow pin from the previous renderer revision;
the semantic release assembly examined below remains the committed output.

## Findings

### H0 — fresh release verification omits mandatory component contract

`ReleaseVerifyProductArgs.component_contract` is a required `PathBuf` CLI
argument at `crates/velnor-runner/src/service.rs:370-404`. The command reads
and applies it at `crates/velnor-runner/src/release.rs:1647-1717`.

The fresh product publisher builds `verify_args` without
`--component-contract` at `.github/workflows/release.yml:4508-4524`. The
published-release reconciliation path does include it at
`.github/workflows/release.yml:4669-4686`.

The renderer attempts to add the flag at
`crates/velnor-workflow/src/s2/primitives/release.rs:2130-2137`, but its
replacement expects a different indentation than the re-indented
`product_verification` block produced at `:2064-2080`; the committed fresh
output proves the replacement did not apply.

The release render test only asserts that the output contains
`release verify-product` (`crates/velnor-workflow/src/s2/primitives/release.rs:6594-6625`); it does not assert the mandatory flag.

### H1 — feature/identity loss breaks the current Homebrew handoff

The source-owned contract and native build rows retain typed fields:

- `.github/workflows/native-product.yml:86-90` materializes the contract;
- `.github/workflows/native-product.yml:147` emits `feature` and `identity`;
- `.github/workflows/release.yml:4426-4430` verifies all seven row fields.

The actual release serializer then discards them:

- `.github/workflows/release.yml:4460-4461` emits archive components without
  `feature` or `identity`;
- `.github/workflows/release.yml:4468-4472` groups canonical components into
  only `{name,crate,version,binary,targets}`.

The runner types codify the loss. `ApplicationComponent` has only five
consumer fields at `crates/velnor-runner/src/product.rs:65-77`, while
`ProductComponentContract` retains the source fields at `:79-110`.
`verify_typed_profile` explicitly checks only crate, binary, version, and
targets at `:529-579`. `ArchiveComponent` likewise omits the fields and uses
`deny_unknown_fields` at `:699-709`.

The local source contract is not a consumer handoff: release publication
uploads `product-assets/*` (`.github/workflows/release.yml:4549-4562`), not
`product-component-contract.json`.

The exact Homebrew consumer requires the dropped fields:

- Canonical component keys, including `feature` and `identity`,
  `config/homebrew-release-contract.json:28-36`;
- Archive component keys, including both fields,
  `config/homebrew-release-contract.json:136-145`;
- Exact canonical key/value enforcement,
  `scripts/package-update.sh:252-267`;
- Exact archive key/value enforcement,
  `scripts/package-update.sh:597-656`.

Therefore “moved to source contract” is not sufficient before consumer
changes. The published artifacts carry no trusted typed source contract from
which Homebrew can reconstruct the omitted values.

### PASS — basename checksum sidecar

The rendered publisher hashes from inside `product-assets` and writes the
basename-only sidecar at `.github/workflows/release.yml:4474-4475`.
This matches Homebrew's sidecar parser and fixes the prior path-bearing form.

The exact renderer test executes the extracted rendered shell, not a hand-
written checksum command:
`crates/velnor-workflow/src/s2/primitives/release.rs:6657-6704`.

## Verification performed

All tests ran in an isolated archive of the exact candidate under `/tmp`; no
repository source, release, install, dispatch, or host runtime was operated.

1. `rtk cargo test --locked -p velnor-workflow --lib rendered_native_product_serializer_emits_runner_compatible_sidecar`
   - **1 passed**, 1740 filtered.
2. `rtk cargo test --locked -p velnor-runner --lib product`
   - **23 passed**, 2336 filtered.

The runner tests pass while codifying the five-field canonical row shape;
there is no complete rendered assembly -> runner -> Homebrew consumer fixture.

## Required bounded follow-up

1. Fix the renderer replacement and regenerate the committed workflow; add a
   render assertion for `--component-contract` in the fresh verification path.
2. Preserve `feature`/`identity` in canonical and archive output, with runner
   verification, or land an atomic versioned consumer contract migration. Do
   not silently drop fields while Homebrew `6520` still requires them.
3. Add a fixture that executes the rendered assembly and feeds its actual
   manifest/archive bytes into runner and Homebrew checks, including negative
   feature/identity mismatch cases.

APT parent-manifest SHA-cycle and preview native-publication work remain open
and are not counted as fixed here.

No approval or gate success is claimed.
