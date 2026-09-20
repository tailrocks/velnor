# Native 578 exact Homebrew consumer cross-check

Status: exact producer contract and executable-mode checks pass; no valid
`MH_EXECUTE` Mach-O fixture exists, so strict Homebrew admission fails closed.
This is synthetic rendered evidence only: no GitHub publication, live provider
attestation, or macOS installation was performed.

## Source and rendered bytes

Resolved native source revision:

```text
branch: codex/github-first-native-product-v3
commit: 578a3470f4871ada32916429099627823e861126
parent: fba55325e9242986b4e21cef40e84010c87168cc
```

The exact rendered assembly test was run from a detached clean worktree at
that commit. The only diagnostic edit retained the test's temporary output
root; producer source was not changed.

```text
VELNOR_KEEP_NATIVE_ASSEMBLY=1 \
CARGO_TARGET_DIR=/private/tmp/dual-lane-native-product-v3/target \
cargo test -p velnor-workflow \
  rendered_native_product_assembly_produces_runner_and_homebrew_contract_bytes \
  -- --nocapture

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1742 filtered out
```

Captured root:

```text
/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/velnor-native-product-assembly-33017532567944036122331056372908556288
```

Product identity is stable `1.2.3`, source repository `tailrocks/velnor`,
source ref `refs/tags/v1.2.3`, source commit
`0123456789abcdef0123456789abcdef01234567`, release tag `v1.2.3`, release ID
`12345`. Exact SHA-256 values:

```text
product-manifest.json                                  518caaf09a57494baa47dfb3512cfd967a9899f50ade6eb88ce2021c1d7df47f
product-manifest.json.sha256 (sidecar file)             b11231291a8a3e0014d8564ddbd1a5f108b849ee1d9532c38ad434895f7a26bc
velnorctl-1.2.3-aarch64-apple-darwin.tar.gz             75d41aa6b80b1af3a8c134fe32525d22f0ba12f815c2ed678a52c19dc7195376
velnorctl-1.2.3-x86_64-apple-darwin.tar.gz              8b4ed0fdd38c322073a53e69675190aacf87fce37449652587cca4d5338f3e33
```

The sidecar content is:

```text
518caaf09a57494baa47dfb3512cfd967a9899f50ade6eb88ce2021c1d7df47f  product-manifest.json
```

Canonical macOS artifact rows:

```text
velnor-runner-aarch64-apple-darwin   36a2c66ae1a5f10831355ef4d9d571c9d86a3a3fb43ad38a363483e1364e5a0a
velnor-workflow-aarch64-apple-darwin d68de7fb822046a6de430469ad8d24cb8204eb855fd10c5621fc10099c818dcd
velnorctl-aarch64-apple-darwin      63a03ef0764dc534bfa6cff6d1e482151e8261fc06da6c36cc57700e5fbb10da
velnorctl-1.2.3-aarch64-apple-darwin.tar.gz 75d41aa6b80b1af3a8c134fe32525d22f0ba12f815c2ed678a52c19dc7195376
velnor-runner-x86_64-apple-darwin   413af8ba9718c468169b4a5311cce7318395b9500d2df71487e25bb64ac49c80
velnor-workflow-x86_64-apple-darwin d25abb06a66be9cebe53b92e1e49393619c28f03eb8d86a2711cf6fee0324641
velnorctl-x86_64-apple-darwin       9f21922ad3819312be7239906d5ea944836bdf81b079a5aa0239be9ba788027c
velnorctl-1.2.3-x86_64-apple-darwin.tar.gz 8b4ed0fdd38c322073a53e69675190aacf87fce37449652587cca4d5338f3e33
```

All six macOS product assets are `0755`. Both archives contain exactly the
required root members; all three binary members are `-rwxr-xr-x`:

```text
identity.json
manifest.json
velnor-runner
velnor-workflow
velnorctl
```

## Parent binding and source/release identity

Both arm64 and Intel subordinate `manifest.json` and `identity.json` records
bind:

```text
product_id=velnor
channel=stable
version=1.2.3
source_repository=tailrocks/velnor
source_ref=refs/tags/v1.2.3
source_commit=0123456789abcdef0123456789abcdef01234567
release_tag=v1.2.3
parent_manifest_id=12345
```

The archive component rows retain all required `feature`/`identity` fields and
the three binary digests equal the canonical artifact rows. No competing
archive authority was introduced.

## Strict Homebrew consumer result

The exact rendered `product-assets/*` bytes were copied unchanged. Only a
synthetic `release-attestation.json` and local Ed25519 signatures were added
for the existing `test-gh-provider.sh` admission double. The unchanged
`scripts/package-update.sh` first accepts archive executable modes, then
rejects the arm64 `velnorctl` payload:

```text
verify-macos-binary: .../aarch64-apple-darwin-velnorctl is not an MH_EXECUTE Mach-O (file type 7463726f)
package-update: archive member is not a native executable for aarch64-apple-darwin: velnorctl in velnorctl-1.2.3-aarch64-apple-darwin.tar.gz
package-update exit status: 1
```

Direct strict checks reject every rendered macOS sibling at the same file-type
gate. `file(1)` labels them Mach-O with expected arm64/x86_64 CPU, but the
fixture only writes the eight-byte magic/CPU prefix and appends the
`<binary>-<target>` label. It does not write `MH_EXECUTE` (`filetype == 2`) at
Mach-O offset 12. The observed bogus file types are:

```text
velnorctl:    7463726f
velnor-runner: 722d726f
velnor-workflow: 772d726f
```

Therefore 578 does not provide a valid native Mach-O fixture. Do not bypass
`verify-macos-binary.sh` or rewrite these bytes and call the consumer pass.
Native owner follow-up: provide a real thin `MH_EXECUTE` fixture or actual
build artifacts, then rerun this exact manifest/archive/provider-bound check.

No Homebrew source files changed. No publication, live provider proof, formula
generation claim, or package installation was made.
