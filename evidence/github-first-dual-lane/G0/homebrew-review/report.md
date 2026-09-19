# Homebrew consumer review

Review target: `tailrocks/homebrew-velnor` commit
`b1c94246216d0006e9b7639b1a4a294212ce0bf7`.

Review tree: detached `/tmp/velnor-homebrew-review-b1c9424`; source tree and
remotes were not changed. The review read the G2 section of
`velnor-github-first-dual-lane-goal.md`,
`G0/homebrew-contract/proposal.md`, and
`G0/distribution-review/report.md`.

## Result

Not approved as G2 evidence. This commit is a consumer contract/updater
proposal only: no verified producer archive or canonical manifest exists, no
generated stable/preview formula is checked in, no publication workflow/CI is
present, and no clean macOS install/upgrade/switch/uninstall run exists.

## Findings

### H1 — invalid SemVer accepted (must fix)

`scripts/package-update.sh` accepts `[0-9]+` components, so it accepts
SemVer-invalid leading zeroes. An executed fixture with a real tar archive and
canonical manifest was accepted and rendered:

```text
updated .../Formula/velnorctl.rb for stable 01.2.3 from v01.2.3
leading-zero version accepted by updater
```

Stable/preview validation and ordering need strict SemVer numeric components
(`0|[1-9][0-9]*`) and a comparator that remains correct for the allowed range.

### H2 — same-version canonical manifest mutation is accepted (must fix)

The same disposable fixture first rendered stable `01.2.3`, then changed only
the non-Homebrew Linux artifact name in `product-manifest.json` while keeping
the source commit, release tag, release ID, Homebrew archive bytes, and archive
checksum unchanged. A second updater run succeeded and changed the formula's
`PRODUCT_MANIFEST_SHA256`. Same-version checks compare the archive checksum but
not the prior canonical-manifest digest. Stable reruns must reject any changed
canonical manifest for an existing version/release, or prove an exact immutable
release record before rewriting the formula.

### M1 — formula tests are not functional package evidence

The fixture executes archive extraction, JSON checks, checksums, sizes, and
monotonic rejection, but formula assertions are `rg` string checks. The
templates only assert that command output contains binary names. They do not
run a generated formula, prove `--version` equals the product/source identity,
prove sibling discovery through `velnorctl host start`, or exercise Homebrew
fresh install, same-channel upgrade, cross-channel switch, rollback, or
uninstall. The commit adds no generated PR/main CI workflow. These are required
before G2.

### M2 — producer provenance is an explicit external dependency

The updater cross-checks the supplied canonical manifest and archive, but does
not independently verify the provider release ID/tag/source commit through the
release API or verify attestation/signature. This is acceptable only if the
producer handoff directory is itself a verified stage with those checks and
durable evidence; that handoff is absent at this commit and must remain a
named G2 dependency.

### M3 — Intel migration needs an explicit publication guard

This exact source commit leaves the historical source-built formula unchanged
and explicitly documents Intel as unsupported, so no deletion is observed here.
Once a producer handoff exists, however, `package-update.sh` will overwrite
`Formula/velnorctl.rb` with an arm64-only formula without inspecting the prior
formula's architecture/support. The publication workflow must make that
unsupported-target migration explicit and preserve/document the existing
advertised target; do not treat the updater alone as proof that Intel removal
is safe.

## Verified passes

- Canonical manifest top-level schema/keys, product, channel, source repo/ref,
  commit, release tag, release ID, artifact rows, size, checksum, and component
  inventory are checked fail-closed.
- Stable and preview identities are cross-checked; Homebrew selects exactly
  one `homebrew-archive`/`aarch64-apple-darwin` row and rejects an Intel
  Homebrew artifact. README/config explicitly record Intel unsupported; no
  silent removal was found in this source-only commit (the old source formula
  remains unchanged).
- The archive requires exactly five root members and executable regular-file
  sibling binaries. `manifest.json` and `identity.json` are subordinate,
  cross-checked against canonical identity with
  `parent_manifest_id == release_id`; the formula carries an external
  `PRODUCT_MANIFEST_SHA256`, so there is no self-hash cycle or alternate
  authority in the intended chain.
- Executed disposable hostile fixtures rejected metadata symlink, traversal/
  extra member, missing sibling, checksum/size tampering, and foreign source
  cases. The supplied `scripts/test-package-update.sh` also passed.
- An executed same-version fixture confirmed the H2 manifest-digest mutation;
  an executed real-archive fixture confirmed H1's leading-zero acceptance.
- `bash -n`, `shellcheck`, Ruby syntax checks, and `git diff --check` passed.

## Commands

```text
bash -n scripts/package-update.sh
bash -n scripts/test-package-update.sh
./scripts/test-package-update.sh
shellcheck scripts/package-update.sh scripts/test-package-update.sh
ruby -c Formula/velnorctl.rb.template
ruby -c Formula/velnorctl-preview.rb.template
git diff --check HEAD^ HEAD
```

All passed on the detached exact-commit tree. No formula/publication or clean
client claim is made. Native scanner review remains pending its exact commit.
