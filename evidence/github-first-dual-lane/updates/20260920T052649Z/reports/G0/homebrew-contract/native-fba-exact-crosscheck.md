# Native fba exact Homebrew handoff cross-check

Status: producer mode admission passes; the unchanged Homebrew consumer reaches
native-architecture admission and rejects the synthetic fixture payload. This
is not a publication, provider-provenance, or macOS installation claim.

## Exact source and rendered assembly

Native source: `tailrocks/velnor`, branch
`codex/github-first-native-product-v3`, commit
`fba55325e9242986b4e21cef40e84010c87168cc`.

The mode-preservation change is `f1bf2397`: downloaded siblings, staged
product assets, archive inputs, and archive members are explicitly restored to
`0755` and checked executable. The exact rendered assembly test was run from
a detached diagnostic worktree at fba (the only diagnostic edit made there
was retaining its normally deleted temporary root):

```text
VELNOR_KEEP_NATIVE_ASSEMBLY=1 \
CARGO_TARGET_DIR=/private/tmp/dual-lane-native-product-v3/target \
cargo test -p velnor-workflow \
  rendered_native_product_assembly_produces_runner_and_homebrew_contract_bytes \
  -- --nocapture

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1741 filtered out
```

Captured rendered root:

```text
/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/velnor-native-product-assembly-33017477877755338135042013644699205632
```

The output identity is stable `1.2.3`, source commit
`0123456789abcdef0123456789abcdef01234567`, tag `v1.2.3`, release ID
`12345`. Exact SHA-256 values:

```text
product-manifest.json                                  62b54c94048acb6d0153a4e7ee083a41a8d18deca9c7ae4dbbb91bca2b38f990
product-manifest.json.sha256 (sidecar file)             cb9a40c3fadbbc18323e226e8097aa474b35ffb14c8e9f3d259c661d6729ec5e
velnorctl-1.2.3-aarch64-apple-darwin.tar.gz             2720e62ccd58c3f3997b3519c762b6c80491c59a8670b047920646c7368e800f
velnorctl-1.2.3-x86_64-apple-darwin.tar.gz              655f659d899317e4189ae5d10cdb91a388a5970427304107cdcf2930d968ac55
```

The sidecar content is the manifest digest followed by the basename
`product-manifest.json`. macOS sibling binary digests from the canonical
manifest are:

```text
velnor-runner-aarch64-apple-darwin  36a2c66ae1a5f10831355ef4d9d571c9d86a3a3fb43ad38a363483e1364e5a0a
velnor-workflow-aarch64-apple-darwin d68de7fb822046a6de430469ad8d24cb8204eb855fd10c5621fc10099c818dcd
velnorctl-aarch64-apple-darwin     63a03ef0764dc534bfa6cff6d1e482151e8261fc06da6c36cc57700e5fbb10da
velnor-runner-x86_64-apple-darwin  413af8ba9718c468169b4a5311cce7318395b9500d2df71487e25bb64ac49c80
velnor-workflow-x86_64-apple-darwin d25abb06a66be9cebe53b92e1e49393619c28f03eb8d86a2711cf6fee0324641
velnorctl-x86_64-apple-darwin     9f21922ad3819312be7239906d5ea944836bdf81b079a5aa0239be9ba788027c
```

All six product-assets siblings are `-rwxr-xr-x`. Both Homebrew archives
contain the required three siblings as executable members:

```text
-rwxr-xr-x velnor-runner
-rwxr-xr-x velnor-workflow
-rwxr-xr-x velnorctl
```

The same native test creates a lost-mode archive by setting those three
members to `0644`, then asserts that the archive has no executable members.
That positive mode assertion and lost-mode negative assertion both passed in
the fba test above.

## Unchanged Homebrew consumer

The exact rendered `product-assets/*` bytes were copied without rewriting.
Only package-root release metadata (`release-attestation.json`) and local
Ed25519 signatures were generated so the existing
`dual-lane-homebrew/scripts/test-gh-provider.sh` could stand in for provider
API/attestation calls. No real GitHub release or provider provenance was
claimed.

The unchanged `scripts/package-update.sh` accepted the executable archive
members, then rejected the synthetic binary payload during its required
Mach-O check:

```text
verify-macos-binary: .../aarch64-apple-darwin-velnorctl is not an MH_EXECUTE Mach-O (file type 7463726f)
package-update: archive member is not a native executable for aarch64-apple-darwin: velnorctl in velnorctl-1.2.3-aarch64-apple-darwin.tar.gz
```

The rendered assembly fixture intentionally writes an eight-byte Mach-O
prefix and appends the component/target label; it is sufficient for the
native contract/mode test but not a full `MH_EXECUTE` file. Bypassing or
rewriting this consumer check would be invalid. Native owner follow-up is a
real Mach-O fixture or actual build artifact, followed by this same exact-byte
rerun.

For the lost-mode consumer negative, a temporary diagnostic copy changed only
the three arm archive member modes to `0644`, then refreshed its manifest,
sidecar, synthetic attestation, and local signatures. The unchanged updater
failed at the intended earlier guard:

```text
package-update: archive member is not executable: velnorctl in velnorctl-1.2.3-aarch64-apple-darwin.tar.gz
lost-mode negative rejected with status 1
```

No Homebrew source files changed. Homebrew branch remains
`codex/github-first-homebrew` at `bdf6fdb2e5f6038bc2a56a35afae2401320329c6`,
clean and matching its remote. No package publication, live install, or
formula advertisement occurred.
