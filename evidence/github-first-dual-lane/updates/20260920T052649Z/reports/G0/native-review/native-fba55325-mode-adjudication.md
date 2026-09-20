# Addendum — exact fba executable-mode adjudication

Review timestamp: 2026-09-20T05:31:00Z

This addendum supersedes only the mode residual in
`native-fba55325-exact-review.md`; that historical report remains unchanged.

## Corrected verdict

**Executable-mode handling: PASS.** The earlier concern that `chmod 0755`
silently repairs a hostile download treated expected GitHub artifact transport
loss as hostile input. The exact pinned actions document that zipped artifact
upload does not preserve file modes: files are restored as `0644`, and tar is
the documented preservation mechanism. fba's explicit restoration at the
trusted source-bound artifact boundary is therefore required, not a bypass.

The final archive is still independently mode-admitted. The unchanged
Homebrew consumer rejects a final archive member without execute permission,
and the exact fba consumer cross-check confirms both valid and lost-mode
behavior. No additional mode field is required: the final tar bytes are
covered by the product artifact digest/attestation, while the consumer checks
the executable mode before native-binary admission.

Overall fba remains **not approved**: the positive consumer run reaches and
passes mode admission but then rejects the intentionally synthetic, non-
`MH_EXECUTE` Mach-O fixture. This is a fixture/full-handoff limitation, not a
mode failure. APT parent binding, immutable preview publication, and the
unpublished f1bf runtime product remain independent blockers.

## Pinned transport evidence

The exact checked-in producer uses `actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` with no `archive` input (`.github/workflows/native-product.yml:233-239`). Its pinned `action.yml` defaults `archive` to true; the pinned source sets `skipArchive` only when that input is false:

- [upload-artifact pinned action.yml](https://github.com/actions/upload-artifact/blob/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml#L48-L53)
- [upload-artifact pinned source](https://github.com/actions/upload-artifact/blob/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/src/upload/upload-artifact.ts#L60-L89)

The pinned upload bundle's archive implementation carries default regular-file
mode `0644` and directory mode `0755` (`dist/upload/index.js`, constants around
the exact bundle's `DEFAULT_FILE_MODE`/`EXT_FILE_ATTR_FILE`). The official
pinned README states the same contract and recommends tar when permissions
must survive:

- [upload-artifact pinned permission-loss contract](https://github.com/actions/upload-artifact/blob/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/README.md#permission-loss)

The exact checked-in downloader is
`actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c`
(`release.yml:4271-4277`). Its pinned source delegates to
`artifactClient.downloadArtifact` with content digest validation, but no mode
preservation option. The pinned README states that post-download files are
not executable after zipped upload and recommends tar:

- [download-artifact pinned source](https://github.com/actions/download-artifact/blob/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/src/download-artifact.ts#L185-L197)
- [download-artifact pinned permission-loss contract](https://github.com/actions/download-artifact/blob/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/README.md#permission-loss)

Therefore, `0644` immediately after artifact download is expected transport
state. It is not evidence of producer corruption.

## Exact fba mode path

The fba producer first requires the compiler output to be executable, then
sets both uploaded sibling copies to `0755` and checks them before upload
(`.github/workflows/native-product.yml:121-145`; preview equivalent
`:141-145`, `:181-185`, `:221-225`). The stable assembly then:

1. checks source-bound files and rejects symlinks;
2. restores `0755` after the documented artifact transport loss;
3. copies to `product-assets`, restores/checks `0755`;
4. restores/checks each archive input and archive member before tar; and
5. hashes the completed tar into the canonical product manifest.

Exact generated lines: `.github/workflows/release.yml:4422-4453` and
`:4454-4473`; generator source:
`crates/velnor-workflow/src/s2/primitives/release.rs:2237-2252`.

The desired mode is thus bound at the final asset boundary, where the whole
archive digest and release attestation are made. A hostile post-assembly
`0644` archive cannot be accepted by a consumer without changing the
attested/digested asset; even a locally refreshed diagnostic metadata set is
rejected by the consumer's explicit mode guard.

## Fresh exact consumer evidence

Evidence: [`native-fba-exact-crosscheck.md`](../homebrew-contract/native-fba-exact-crosscheck.md), SHA-256
`d8a1732b46487fbf034c1140696acac8d935fc723e92c02a4470541599b87aaf`.

The exact candidate and exact rendered bytes produced six executable product
siblings; both Homebrew archives listed all three members as `-rwxr-xr-x`.
The unchanged consumer passed its executable-mode guard
(`dual-lane-homebrew/scripts/package-update.sh:545-552`) and proceeded to the
native Mach-O check. It then correctly rejected the fixture because its
eight-byte synthetic payload was not a valid `MH_EXECUTE` file. This means the
mode path passed, while full consumer admission remains unproven until a real
Mach-O artifact is used.

The same cross-check changed only the three archive member modes to `0644`,
refreshed local diagnostic metadata/signatures, and ran the unchanged consumer.
It failed at the intended guard:

```text
package-update: archive member is not executable: velnorctl in velnorctl-1.2.3-aarch64-apple-darwin.tar.gz
lost-mode negative rejected with status 1
```

This is actual consumer rejection, not a string or local-helper assertion.

## Adjudication matrix

| Question | Result |
|---|---|
| Does GitHub artifact transport preserve raw executable bits here? | **No; documented/pinned zipped transport normalizes files to `0644`.** |
| Is fba restoration before final assembly legitimate? | **Yes; required to produce the final archive contract.** |
| Are valid fba final archive members executable? | **Yes; exact cross-check lists all required members `0755`.** |
| Does the real consumer reject final `0644` members? | **Yes; exact lost-mode negative exits at the mode guard.** |
| Did the exact synthetic fixture complete all consumer checks? | **No; it reaches mode admission, then fails invalid Mach-O.** |
| Is a separate signed JSON mode field required by current evidence? | **No; final archive digest/attestation plus consumer mode admission bind the delivered mode.** |

## Adjudication follow-up

1. Replace the synthetic Mach-O bytes with actual producer/build output and
   rerun the exact-byte Homebrew consumer check; do not bypass the native
   `MH_EXECUTE` check.
2. Retain the producer/download/staging/archive `0755` restoration and the
   consumer lost-mode negative regression.
3. Resolve the independent APT parent digest, immutable preview product, and
   f1bf runtime publication blockers before any overall approval.

No publication, install, host runtime operation, or source edit was performed.
