# Stable external evidence checkpoint 20260920T085000Z

- Capture cutoff: **2026-09-20T08:50:00Z** UTC. Capture metadata: **2026-09-20T08:53:31Z** UTC.
- Parent checkpoint: `c391d96a3d1e3cc434f83c5481949f25bdbe49d6`, preserved unchanged.
- Included: **48** regular non-symlink files, **3,034,649** source bytes; exact source/destination/hash/mtime rows are in `INVENTORY.tsv`.
- Groups: **G0 39 files / 2,964,223 bytes**, **G1 9 files / 70,426 bytes**.

## Included reviewed records

- G0 run-metadata capture: independent review SHA-256 `5b5545de1b398e3f263da87937ccbd823b8fb28b1ccff026aeb439dd82f43609`; root bounded-observation report, corrected capture metadata, integrity-v2 index, and raw inventory manifest are included. Review disposition is bounded raw-observation/child-graph integrity only, not a gate.
- Small real GitHub API corpus: independent review SHA-256 `3a1e02a9b8ac0efad921fc596712071c3e65c9e9809f38ddebf9cdcddcc76106`; all 28 raw files plus README, manifest, and relationship report are included. The source manifest rehashed 28/28 raw paths with zero hash/size mismatches before copy.
- Checker provider-raw review `40d54bee...` SHA-256 `861db0fec15fb52036d6f762e1f0e2d4f3ba726f2bd2bc1c30a1945f8ba0b1d2`: changes required; mapper corpus review `f7818fb8...` SHA-256 `76a6258cf4c2fd8f3d7d7f205898797de8b6bd767cf5d4ba4e89b769d99289d5`: reject acceptance-ready.
- Native/signer records: native `f37827e0` review SHA-256 `07158373e35acced2328038565ae061123137241a2588a1ca89af7344b295a30` rejects publication; signer design SHA-256 `ccbf61fcda05f0858f7a671358750b754f77850dc47f0d2770f235b0dda2f02c` and independent review SHA-256 `8059448c53b9441bad42ec7166ddbf6e5d4b91df6bb8e4dfdf8c73601083f163` remain design-only/not implementation approval.
- Records/docs: predecessor review `2719c8cd...` SHA-256 `362cbe4232b48b9426040a79173c6ba344ce9e40bfbf0e546eda4c902941683c` remains changes-required history; successor `907261d9...` SHA-256 `f40c0e9e59a7e1b911c4c15f8a55d5b63d4bcda178d68a4e10b06a94dae52fbf` approves only the narrow documentation correction.
- Bootstrap/containment: source-review `358412b3...` SHA-256 `11d05444c9c8956fda6fa4c24b875b38c9a648f8a9b4e02481439d9b10961560` is partial/changes-required; its f5 addendum SHA-256 is `136942daef4645f5a8f4e703a3ffe739ad3fa840d6104768a2b1d452e1367ae2`. Containment research SHA-256 `72dd413c1fa5efafa9075277444e628d8e77bdd795a89f10638a852f5805a798` and runtime design review SHA-256 `cfef00e802e390784c06b4eff4b1867a7bf45ecf42f0c92b25b47cf5371c9abe` are bounded architecture research/review only.

## Raw-byte scope

The small real-API corpus is byte-sensitive. Root `.gitattributes` adds `-text -filter` rules for `reports/G0/real-api-fixture-corpus-*/raw/**` and the captured `raw-manifest.json`; fresh committed-object verification must confirm these attributes.

The full G0 capture raw tree was deliberately **excluded**: capture root is approximately **225,468 KiB / 7,037 files**, with **115,952 KiB / 7,005 raw files**. It exceeds this bounded checkpoint's size budget. Its safe inventory-only `raw-manifest.json` (1,931,936 bytes, SHA-256 `6cc98c2886ad23a72bd17cf72f0fb5cea9038da8f01090579360affe62d53637`) is included; raw bodies, HTTP envelopes, stderr logs, and the remaining capture summaries are not copied.

## Stability and exclusions

Every selected source path was a regular non-symlink file at two source hash reads bracketing the copy. The complete 48-row source list matched between reads, and all destination hashes/sizes reconciled. The source tree was not edited.

Excluded: live writers and moving capture trees; checker/hosted WIP; full G0 raw bodies/HTTP/stderr and large derived capture summaries; credentials/auth tokens; builds, targets, archives, repository clones, `.git` metadata, and `__pycache__`; and all paths newer than the cutoff. No workflow dispatch, source admission, authority transition, release/publication, gate, merge, or installation occurred.

Validation target: JSON/NDJSON parse, high-confidence secret scan, source/destination hash reconciliation, real-corpus manifest rehash, and fresh committed-object checks (including predecessor raw-byte manifests and new `-text -filter` attributes) pass before normal push. Attestation: **none**. Gate status: **not-evaluated**.
