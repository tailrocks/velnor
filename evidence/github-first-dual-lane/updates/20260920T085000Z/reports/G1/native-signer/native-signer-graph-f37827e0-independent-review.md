# Independent review: native signer graph design

## Exact design

- Path: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/native-signer/native-signer-graph-f37827e0.md`
- SHA-256: `ccbf61fcda05f0858f7a671358750b754f77850dc47f0d2770f235b0dda2f02c`
- 249 lines, 11,462 bytes.
- Design anchor: native producer `f37827e09415ba5733b37e97a16aef716e553322`.

## Verdict

**Design direction is correct, but not implementation-approval-ready.** It
closes the direct-attestation concept and names the needed graph, signer input,
identity fields, permissions, race checks, and hostile tests. Four contract
gaps still permit a producer/consumer mismatch or a skipped-path bypass.

## Required design repairs

1. **Use admitted identity outputs at every signer/consumer boundary.**

   The signer examples pass `refs/tags/${{ github.ref_name }}` and
   `${{ github.sha }}`. The admission job separately emits the re-read
   `source_ref` and `source_commit`. The design must require
   `needs.admit-product-release.outputs.source_ref/source_commit` (and the
   admitted `release_id`, tag, and target) to feed native build contracts,
   assembly, every signer caller, final `gh attestation verify`, APT, and
   Homebrew. Re-deriving from caller context leaves a tag/ref race or retry
   with two authorities. Preview must likewise use the resolved identity job
   output, not an independently re-read `github.sha`.

2. **Make published-path skipping executable, including the blocked Intel lane.**

   The design says `phase=published` skips assembly and signing, but its
   proposed `publish` condition still requires
   `needs.native-product-build.result == success`. The generated native
   reusable workflow has an explicit failing `x86_64-apple-darwin` blocked
   job, so an already-published release can be prevented from reaching the
   read-only verification path. Specify the exact `needs`/`if: always()` graph:
   either do not require native builds for `phase=published`, or define a
   separately proven blocked-target result that cannot satisfy a draft build.
   Unknown/empty phase and skipped/failed draft assembly or signer must remain
   fatal.

3. **Define the dynamic signer subject transport.**

   `sign-native-product[N]` is described as a matrix over the typed inventory,
   but the inventory is produced by `native-product-assemble` at runtime. The
   design must specify an exact job output schema (for example a canonical,
   sorted JSON array of safe basenames), how the matrix consumes it, how a
   skipped published path avoids evaluating an absent output, and the exact
   result condition for every matrix child. The list must be derived from the
   actual `native-product-assets` bytes/manifest, not a handwritten or
   generator-only list; duplicate, missing, unsafe, and extra subjects must
   fail before signing.

4. **Add the actual APT/Homebrew producer-consumer contract.**

   The design requires an exact consumer test but does not define the transport
   and field-level handoff. It must state how the sole `native-product-assets`
   artifact becomes release assets, how `product-manifest.json` rows map to
   those basenames/digests/sizes, how APT obtains and verifies the Debian rows
   covered by `sign-deb` plus the native metadata/source identity, and how
   Homebrew obtains/verifies each archive row and `parent_manifest_id`. Include
   exact provider release/tag/source-digest lookup and shared-signer selector
   commands for both consumers. A test requirement alone does not bind the
   current APT self-declared selection or the Homebrew archive consumer to the
   actual producer output.

## Additional contract clarifications

- Stable admission must prove the existing Git tag/ref resolves to the
  admitted commit before POSTing a draft; otherwise the release API may mint a
  tag from `target_commitish`. Preview tag creation needs a separately stated
  rule. Re-read after a create race and reject all identity/flag/name/target
  mismatches.
- Existing draft behavior needs an exact stale/partial/duplicate asset rule.
  The publisher must not silently clobber or leave old assets that defeat the
  complete census; define whether an empty-only draft is admissible or how a
  resumable exact draft is reconciled.
- `source-digest` must be required on every existing `sign-deb` and native
  caller, with source-ref exactness checked against the admitted pair. The
  shared signer should reject non-hex, wrong-length, caller/admitted mismatch,
  and any source ref not bound to its own commit before download/attestation.
- The caller permission union must be explicit in generated stable and preview
  workflows: assemble has no `id-token`/attestation/provider-write access;
  signer callers alone have `contents: read`, `id-token: write`,
  `attestations: write`; publish has provider write but no direct attestation.
  Every existing signer caller must pass the new required input.
- “No direct attestation” must be scoped to native product subjects while
  preserving any separately justified runtime/package attestation. Final
  publisher verification must use the shared signer workflow, exact source
  ref/digest, OIDC issuer, SLSA predicate, hosted-runner restriction, and
  complete subject census for the actual producer bytes.

No implementation, generation, publication, dispatch, install, or runtime was
performed for this design review.
