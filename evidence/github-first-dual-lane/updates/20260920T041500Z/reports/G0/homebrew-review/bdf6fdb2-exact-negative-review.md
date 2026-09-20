# Exact Homebrew negative-fixture review: bdf6fdb2

Review timestamp: 2026-09-20T03:52:31Z

## Verdict

The exact Homebrew consumer commit **rejects both five-field migration
projections**. This is the required fail-closed behavior while the native
producer has not published an atomic provider-bound contract migration.

Publication approval: **REJECTED / not claimed**. The prior native producer
handoff review remains blocked; this consumer result does not make d33
publishable.

## Immutable snapshot

- Repository: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-homebrew`
- Branch: `codex/github-first-homebrew`
- Candidate: `bdf6fdb2e5f6038bc2a56a35afae2401320329c6`
- Parent: `6520aad7bd66d53349e040508146956e9c4f0c1e`
- Candidate tree: `68b92c3925d32dfc7a2ad8d7e8697db24009588f`
- Remote ref: `origin/codex/github-first-homebrew` resolves exactly to the
  candidate SHA.
- Commit trailers: `Co-authored-by: Codex <codex@openai.com>` and
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`.
- Git cryptographic signature status: `%G? = N` (no cryptographic signature;
  trailers are present).

The candidate changes only `scripts/test-package-update.sh` (+37 lines).

## Exact fixture changes

The canonical hostile case removes both component fields from the real
producer-shaped `product-manifest.json` and refreshes its attestation:
`scripts/test-package-update.sh:399-403`.

The archive hostile case extracts the actual archive, removes both fields from
`manifest.json`, rebuilds the archive, updates its canonical digest/size,
refreshes the attestation, and signs the changed archive:
`scripts/test-package-update.sh:322-347,405-407`.

The fixture invokes the production parser, not a mock validator:
`scripts/test-package-update.sh:243-250` runs
`$root/scripts/package-update.sh`; the provider shim performs real Ed25519
signature verification at `scripts/test-gh-provider.sh:27-66`.

## Failure proof

An instrumented copy of the exact candidate printed the production failure
for the new cases:

```text
EXPECTED_FAILURE_8
package-update: canonical component inventory is invalid
EXPECTED_FAILURE_9
package-update: archive manifest does not cross-check the canonical identity: velnorctl-9.8.7-aarch64-apple-darwin.tar.gz
```

The canonical failure is the exact required-key/value gate at
`scripts/package-update.sh:252-271`; the archive failure is the exact
required archive component field/value gate at `scripts/package-update.sh:575-609`.
The archive case reaches this gate only after artifact size/checksum, archive
member, binary-executable, subordinate identity, and provider attestation
checks, so it is not an unrelated fixture failure.

## Full checks

Executed against the exact candidate:

```text
rtk bash scripts/test-package-update.sh                         PASS
  Homebrew contract fixture validation passed
rtk bash -n scripts/package-update.sh                            PASS
rtk bash -n scripts/test-package-update.sh                       PASS
rtk shellcheck scripts/package-update.sh scripts/test-package-update.sh PASS
rtk ruby -c Formula/velnorctl.rb.template                       PASS
rtk ruby -c Formula/velnorctl-preview.rb.template               PASS
rtk jq empty config/homebrew-release-contract.json              PASS
rtk git diff --check 6520aad7... bdf6fdb2...                     PASS
```

The full suite exercises valid stable/preview updates, same-version and
rollback rejection, missing/tampered signatures, archive metadata, native
architecture negatives, and the two new missing-field negatives. No source
tree edits, install, release publication, remote mutation, or clean-client
claim was made.

## Consumer contract relevance

The required canonical component fields remain explicit in
`config/homebrew-release-contract.json:28-36`; required archive fields remain
explicit at `:136-145`. Production checks enforce canonical fields at
`scripts/package-update.sh:252-271` and archive fields at `:575-609` plus
component cross-checks at `:633-656`.

This confirms the native d33 review finding: dropping `feature` and
`identity` before a coordinated consumer migration is rejected by the real
consumer parser.

No G2 publication or approval is claimed.
