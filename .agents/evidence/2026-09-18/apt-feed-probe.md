# APT feed probe — velnor-apt.tailrocks.com

- Probed: 2026-09-17 ~01:12 UTC (read-only HTTPS GET/HEAD + `gpg --list-packets`; no feed writes)
- Feed: `https://velnor-apt.tailrocks.com/` (served by GitHub Pages: `server: GitHub.com`)
- Method: curl status/body checks; SHA256 recomputation vs Release; gzip roundtrip; signature-packet keyid extraction (fingerprint only, NO trust decision, no verification performed)

## 1. Root / indices

- `/` → **404** (GitHub Pages default page; no `index.html`). Cosmetic only — APT clients don't need it.
- No directory listings anywhere (`dists/`, `dists/stable/`, `pool/`, `pool/main/v/velnor-runner/` → all 404). Expected on Pages; layout reconstructed from Release + direct object fetches.

## 2. dists/ layout

- Single suite: **`stable`** only. All other suites 404: testing, unstable, nightly, experimental, main, rolling, dev, prod, release, candidate.
- `dists/stable/` objects live (HTTP 200):
  - `InRelease` (3319 B, clearsigned), `Release` (2437 B), `Release.gpg` (833 B, detached)
  - `main/binary-amd64/Packages` (2466 B) + `Packages.gz` (998 B)
  - `main/binary-arm64/Packages` (2466 B) + `Packages.gz` (1001 B)
- Absent (404, not listed in Release either): `Packages.xz`, `Translation-*`, per-arch `Release`, `by-hash`, `InRelease.gpg`.

## 3. Release metadata

Date: `Mon, 14 Sep 2026 14:52:13 +0000`. Origin `Velnor`, Label `Velnor`, Suite/Codename `stable`, Components `main`, Architectures `amd64 arm64`. Checksum blocks MD5/SHA1/SHA256/SHA512 over the 4 Packages objects (+ self `Release` entry, 219 B... note: actual Release is 2437 B; the 219-B self-entry is `apt-ftparchive`-style and not verifiable — standard quirk, not a defect).

## 4. Signatures (fingerprint only, no trust decision)

- InRelease payload is **byte-identical to Release** (extracted clearsign body `diff`: MATCH).
- Both signatures made by the **same key**, creation `2026-09-14 14:52:13 UTC` (= Release Date):
  - keyid: `857FCD279679A34B`
  - issuer fingerprint (v4, sig subpacket 33): `CD4693750A4BA4F12BC9ABFD857FCD279679A34B`
  - algo RSA(1); sigclass 0x01 (InRelease) / 0x00 (Release.gpg)
- **No public key published on the feed**: 20+ candidate paths 404 (`key.gpg`, `velnor.asc`, `KEYS`, `dists/stable/*.key|asc|gpg`, `.well-known/apt-key.asc`, …). Signatures therefore not cryptographically verifiable from feed alone — key must come from the separately trusted project reference per C1 procedure (absence on feed is consistent with that rule, but verification is blocked until the trusted key is available).

## 5. Packages indices — versions / archs

Both archs list exactly two `velnor-runner` stanzas: previous **`0.1.273`** + candidate **`0.1.274`**.

amd64 (`SHA256 3db8f09f…9ca3`, matches Release; `.gz 86bf4d92…779e`, matches; gzip roundtrip MATCH):

| Version | Filename | Size | SHA256 |
|---|---|---|---|
| 0.1.273 | `pool/main/v/velnor-runner/velnor-runner_0.1.273_amd64.deb` | 134832884 | `5d7fefec861a8e4304aea12bb5e5ada84d7b27fd97bf93c3583dc5fa2c0f70bf` |
| 0.1.274 | `pool/main/v/velnor-runner/velnor-runner_0.1.274_amd64.deb` | 134831888 | `3a35d3aba1b93f0031960827fb55a7ab0488d72e032ba9bd8e2c1cc24569a84a` |

arm64 (`SHA256 df9bb944…3992`, matches Release; `.gz db61bf4f…5662`, matches; gzip roundtrip MATCH):

| Version | Filename | Size | SHA256 |
|---|---|---|---|
| 0.1.273 | `pool/main/v/velnor-runner/velnor-runner_0.1.273_arm64.deb` | 109196484 | `2f45042194ae0ddeeb0bf246a4313cc5cdc573dfdcd30b8ad67d8dd2360c79ce` |
| 0.1.274 | `pool/main/v/velnor-runner/velnor-runner_0.1.274_arm64.deb` | 109198972 | `915e98fa56a63ce12a58b6a7fe668e40cacc82b849460515756a4f57695651f6` |

Common stanza fields: Section `devel`, Maintainer `Alexey Zhokhov <alexey@zhokhov.com>`, Depends `ca-certificates, curl, git, libc6 (>= 2.34), libgcc-s1, systemd-tmpfiles | systemd, util-linux (>= 2.37.2)`, Recommends `docker.io | docker-ce`.

## 6. Pool

- All 4 `.deb` files HTTP 200; `Content-Length` **exactly equals** the `Size:` field for all 4; `Last-Modified: Mon, 14 Sep 2026 14:53:1x GMT`.
- Range-fetch magic bytes on both 0.1.274 debs: `!<arch>` ✓ (real Debian archives, not error pages).
- Retention boundary: `0.1.272` (both archs) → 404; `0.1.275` (both archs) → 404. **Exactly the two indexed versions are present** — nothing older, nothing newer.
- Full `.deb` SHA256 verification NOT performed (total ~488 MB; left to the release verifier holding the source record). Sizes + magic + index-hash chain verified instead.

## 7. Publication record

`/publication-record.json` → 200 (747 B, `Last-Modified: Mon, 14 Sep 2026 14:53:13 GMT`):

```json
{
  "schema": "velnor.publication-record/v1",
  "source_record_sha256": "1ea9dc7095ef8f873ea2266b34acaf80442fc2b1b17bd13d2c8ebd65f2574bd9",
  "tag": "v0.1.274",
  "crate_version": "0.1.274",
  "inrelease_sha256": "f2e8923b939b691e195ed655728e8fb24bcf34f1658caf76641b17823c75dfd1",
  "packages": [
    {"arch": "amd64", "sha256": "3db8f09f570610220b333a9bb6dbf5d24a4835573522dfb2f71ef4ce995a9ca3"},
    {"arch": "arm64", "sha256": "df9bb944865fc10f055cfa86445f441e00a475b00000a5dd4e152eba767d3992"}
  ],
  "signer_fingerprint": "7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801",
  "previous": {"tag": "v0.1.273", "source_record_sha256": "e6497cdda5717c8087a719f7a1115e6c60c505d3f1ac064151e1c9150cee4f12"}
}
```

- `inrelease_sha256` **MATCHES** live InRelease recomputed locally; both `packages[].sha256` **MATCH** Release entries. Record corresponds to exactly this live metadata.
- ⚠ **MISMATCH: `signer_fingerprint` (`7E66…B801`) ≠ live signature issuer fingerprint (`CD46…A34B`)** on both InRelease and Release.gpg. The metadata chain is self-consistent (both sigs, same key, same timestamp); the record is the outlier. Possible benign cause: record names the primary key while `CD46…` is its signing subkey — unresolvable without the published/trusted public key. **Must be resolved against the separately trusted key reference before any C1 install treats the feed as verified.** No trust decision made here.

## 8. B4 target-state comparison

B4 done-when: staged index + retained previous version + signed InRelease/Release + publication record; single-writer Pages; no-rollback; live candidate verified (signed chain, exact version, both archs, hashes, source/record, origin); signer fingerprint recorded.

| B4 element | Feed state | Verdict |
|---|---|---|
| Staged index (live) | Packages list 0.1.273 + 0.1.274, both archs; all index hashes verify against Release | ✓ live index consistent (staging mechanics not observable from static feed) |
| Previous version retained | 0.1.273 in index + pool, both archs; `previous: v0.1.273` + source_record_sha256 in record | ✓ |
| Signed InRelease/Release | Both present; payloads identical; same-key sigs; timestamp = Release Date | ✓ present & self-consistent (crypto validity unverified — no trusted key) |
| Publication record | Present at feed root; hashes match live metadata | ⚠ hash-consistent BUT signer fingerprint mismatches live sigs |
| Single-writer Pages | Hosted on GitHub Pages | ✓ consistent (serialization/no-rollback not observable statically) |
| Live candidate: exact version, both archs | 0.1.274 amd64 + arm64, exact sizes/hashes recorded above | ✓ |
| Origin | `Origin: Velnor`, `Label: Velnor` | ✓ |
| Signer fingerprint recorded | Live sigs: `CD46…A34B`; record claims `7E66…B801` | ⚠ recorded, DISCREPANT — see §7 |

## 9. Flags for the verifier / infra owner

1. **Signer-fingerprint discrepancy** (§7) — resolve against the trusted key reference; do not C1-install until the signing key vs record is reconciled (confirm or rule out primary/subkey relation).
2. No public key on feed — expected per procedure, but live-chain crypto verification needs the out-of-band trusted key.
3. Full `.deb` payload hashes unverified in this probe (sizes + magic only); verifier must compare against the source record.
4. No-rollback / same-feed serialization / pre-mutation verification are process properties — not provable from this static read-only probe.
