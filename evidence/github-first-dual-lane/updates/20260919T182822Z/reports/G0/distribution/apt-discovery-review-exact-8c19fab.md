# Exact APT application-release discovery review

## Decision

**Reject source approval for G2 integration.** Exact commit
`8c19fab405b8c608c5db810d4f01d4f3c65b08cd` closes the prior inventory,
artifact-byte, sidecar, URL, source-tag, ambiguity, and release-ID findings in
the stable path, but still emits eligible candidates for malformed packaged
identity and a non-canonical release URL. Preview output also labels
`refs/heads/main` while proving only an immutable preview tag. No G2 approval,
workflow integration, publication, or remote write was performed.

## Exact scope

- Repository: `tailrocks/velnor-apt`
- Branch: `codex/github-first-apt-discovery`
- Reviewed commit: `8c19fab405b8c608c5db810d4f01d4f3c65b08cd`
- Base: `0081b5069134e4110e8bbbafb7ba2e61017e70b1`
- Worktree: clean before and after review.
- Cross-consumer comparison: Homebrew commit
  `c772971de3df714b33bffb55febfcfe478428175`, clean.

## Gates

```text
rtk bash scripts/test-release-discovery.sh       exit 0
rtk mise run check                               exit 0
rtk bash -n scripts/release-discovery.sh ...     exit 0
rtk shellcheck scripts/release-discovery.sh ...  exit 0
rtk git diff --check BASE..8c19fab...            exit 0
```

The existing external hostile harness was run against the exact commit:
`dual-lane-evidence/G0/distribution/apt-discovery-adversarial-v2/results.tsv`.

| Fixture | Expected | Actual |
|---|---:|---:|
| baseline | pass | exit 0 |
| binary-name-drift | fail | exit 1 |
| sha256-sums-tamper | fail | exit 1 |
| release-manifest-assets-tamper | fail | exit 1 |
| release-record-census | fail | exit 1 |
| homebrew-digest-tamper | fail | exit 1 |
| asset-url-missing | fail | exit 1 |
| source-ref-mismatch | fail | exit 1 |
| ambiguous-version | fail | exit 5 (jq ambiguity error) |
| release-id-grammar | fail | exit 1 |

## Blocking findings

### 1. Packaged `manifest.json` identity is still false-green

`validate_subordinate_records` only checks the packaged manifest's parent
digest and `.source_sha` (`scripts/release-discovery.sh:438-441`). It does not
check the packaged `.crate_version`, numeric `.version`, or that its version
equals `release-record.json`'s `.build.manifest_version`. The later
`verify-release.sh` checks these fields, but discovery itself emits an eligible
candidate before that gate.

Exact hostile proof used the baseline fixture, kept every sidecar and
release-record digest coherent, and changed `assets/14` (`manifest.json`) to
`.version=999` and `.crate_version="evil"`; the candidate still returned exit
0. Independent single-field runs also returned exit 0:

Persisted fixtures:

```text
dual-lane-evidence/G0/distribution/apt-discovery-review-exact-8c19fab/
  package-identity-tamper/
  package-version-tamper/
  package-crate-tamper/
```

Each was run as:

```text
FAKE_ROOT=<fixture> PATH=<fixture>/bin:$PATH \
  /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-apt/scripts/release-discovery.sh \
  --channel stable
# exit 0
```

The discovery gate must require the packaged identity fields and bind
`manifest.version` to `record.build.manifest_version`, in addition to the
existing source and parent-digest checks.

### 2. Trust-bearing `release_url` accepts an attacker URL

The candidate check only requires nonempty `.html_url`
(`scripts/release-discovery.sh:545-550`) and copies it into output
(`scripts/release-discovery.sh:633-635`). Asset URLs are correctly exact and
all bytes/sidecars remained valid, but changing the release's `.html_url` to
`https://evil.example/release` still returned exit 0 and emitted that URL as
`release_url`.

The exact hostile fixture is persisted under
`dual-lane-evidence/G0/distribution/apt-discovery-review-exact-8c19fab/html-url-tamper/`.
Require
`.html_url == https://github.com/$SOURCE_REPOSITORY/releases/tag/$tag`, or
derive this URL from the validated repository/tag rather than trusting API
metadata. It was run with the same command above and returned exit 0.

### 3. Preview source identity is semantically ambiguous

Stable ref resolution is now good: `resolve_immutable_release_ref` handles
lightweight and annotated tags and compares the resolved commit
(`scripts/release-discovery.sh:166-194`, `575-577`). Candidate tie rejection
is also present for stable and preview (`667-688`).

For preview, however, the candidate sets `source_ref=refs/heads/main`
(`562-568`), resolves only `refs/tags/preview-<commit>` (`575-577`), and emits
that tag as `source_ref_resolution.proof_ref` while still claiming
`source_ref=refs/heads/main` (`639-642`). The supplied preview fake API only
implements `/git/ref/tags/`, so the passing preview test does not prove a main
branch identity. This is either an unproved branch claim or a contract naming
error. Make the immutable preview-tag proof explicit as the authoritative
source ref, or include an issuance-time proof tying the build's
`refs/heads/main` to that commit. Do not present tag proof as branch proof.

## Prior-gap closure checks

Passed in this exact source:

- component names, crates, binaries, target sets, and artifact row schemas are
  exact;
- release-record architecture census is exactly `{amd64, arm64}` and its APT
  hashes bind to the parent manifest;
- subordinate release-manifest asset census is exactly the APT projection;
- every canonical artifact, including non-APT/Homebrew rows, is downloaded
  and checked for SHA-256 and size;
- APT `.deb` sidecars and exact two-line `SHA256SUMS` are checked;
- every listed asset URL is required to be the canonical GitHub download URL;
- stable immutable tag resolution and same-version ambiguity rejection work;
- APT `RELEASE_ID_PATTERN` is
  `^[A-Za-z0-9][A-Za-z0-9._:/-]*$`, matching Homebrew `c772971` and its
  documented shared grammar.

No product source files were edited. The only new artifact is this external
review report.
