# Security

Report suspected vulnerabilities through GitHub's private security reporting
for `tailrocks/velnor`. Do not disclose credentials, signing material, or
unpublished release artifacts in an issue or pull request.

## Release trust

Velnor's `.github/workflows/release.yml` is the source-side coordinator for
the Velnor↔velnor-apt release train. A tagged release binds the source commit,
crate version, binary and package digests, OCI image digest, and capability
manifest into one immutable release record.

- The release workflow holds no APT credential and does not publish to or
  dispatch `tailrocks/velnor-apt`.
- Package subjects are attested by the pinned reusable signer in
  `tailrocks/velnor-actions`; the current workflow pin is a full commit SHA
  and carries its release label.
- `tailrocks/velnor-apt` independently fetches and verifies the release
  record, package hashes, and signed APT metadata before publication.
- APT repository signing keys remain owned by `tailrocks/velnor-apt`; they
  are never copied into this repository or its workflow inputs.

No release may bypass the signed record, signer attestation, or signed APT
publication path. A GitHub Release asset or local package is not an
installation authority by itself.

## Rotation runbook

1. Freeze the affected release train and record the reason, current release
   tag, source commit, and signer or APT key fingerprint.
2. For reusable-workflow rotation, review the new `velnor-actions` release,
   verify the full commit SHA and release label, then update the pinned
   reference in a signed change. Keep the previous signer reference only for
   the explicitly documented validity window while consumers are re-rendered.
3. For APT-key rotation, generate and protect the replacement key in the
   `velnor-apt` owner boundary, publish the replacement public key through its
   documented channel, and overlap old/new repository metadata only for the
   approved transition window.
4. Verify the exact source/tag/version identity, signer attestation, package
   SHA-256 values, `InRelease` and `Release.gpg` signatures, trusted key
   fingerprint, and candidate/rollback pair before resuming publication.
5. Revoke or retire the old key or signer reference after the transition
   window, then record the final fingerprints, release SHA, and verification
   evidence in the owning repository's release log.

Suspected compromise is fail-closed: stop publication, preserve evidence,
rotate the affected credential or key in its owning repository, and do not
install an unsigned or locally rebuilt package.
