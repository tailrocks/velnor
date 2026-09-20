# Native 384 exact Homebrew consumer cross-check

Status: blocked at archive executable-mode admission. This is a synthetic
producer fixture run; it is not a GitHub publication, provider attestation, or
macOS install claim.

## Native source and rendered assembly

Source: `tailrocks/velnor`, branch
`codex/github-first-native-product-v3`, commit
`384e2e4e1ed039d12977547ec9ca0f538803ab54`.

The successor includes `50f40645` (canonical and archive component rows
preserve `feature`/`identity`) and `ea3da095` (the rendered assembly test
executes the generated publisher shell, invokes runner verification with
`--component-contract`, and checks the Homebrew archive field set). The exact
test passed:

```text
cargo test -p velnor-workflow rendered_native_product_assembly_produces_runner_and_homebrew_contract_bytes -- --nocapture
1 passed, 1864 filtered out
```

The test normally deletes its temporary output. A detached diagnostic copy of
the same source preserved the generated root only to run the consumer check;
the production source tree and commit were not changed. Captured root:

```text
/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/velnor-native-product-assembly-33017452920128350570806301982969561088
```

Native output identity:

```text
product version: 1.2.3
source commit: 0123456789abcdef0123456789abcdef01234567
release tag: v1.2.3
release id: 12345
product-manifest.json: a0fcb220c6d97722796f1ff62178437d5b21d54e9766afbc6b53c10b7eaf641c
product-manifest.json.sha256: c9c3ff0f73a20f8c05dec23031137b68486f22c441644ff8e3ed7fc8b043828a
arm64 archive: 65c427e0818fc32dd8db6fb93847f9f4fdbef77b1a3d2fd40439a87a5cdfc500
Intel archive: 223b8421cbceedbb98a91ad5d9eeae152501c46e20e289ae8eb952fdd6c9bbb1
```

The canonical manifest has seven component fields, including `feature` and
`identity`; the arm64 subordinate archive has all eight required component
fields. No five-field migration was accepted.

## Homebrew consumer run

The existing Homebrew `scripts/package-update.sh` consumed the exact native
`product-assets/*` bytes. A temporary Ed25519 key and the existing
`scripts/test-gh-provider.sh` supplied only a local provider/API test double;
the release-attestation and signatures were generated for admission plumbing,
not asserted as live provenance.

Result:

```text
package-update: archive member is not executable: velnorctl in velnorctl-1.2.3-aarch64-apple-darwin.tar.gz
```

The exact rendered archive lists every member as `-rw-r--r--`, including all
three binaries:

```text
-rw-r--r-- identity.json
-rw-r--r-- manifest.json
-rw-r--r-- velnor-runner
-rw-r--r-- velnor-workflow
-rw-r--r-- velnorctl
```

This is a real consumer rejection, not a source-string assertion. The native
assembly fixture writes sibling bytes with `fs::write` and does not set the
executable bit before the rendered tar step. The producer owner must make the
fixture (and verify the downloaded-build path) preserve executable modes, then
rerun this exact-byte check. Do not chmod or rewrite these captured bytes and
call that a producer pass.

No Homebrew source change or publication was made for this checkpoint.
