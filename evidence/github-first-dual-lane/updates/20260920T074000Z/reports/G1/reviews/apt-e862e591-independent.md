# Independent APT source review: e862e591

Date: 2026-09-20

Scope: exact-commit, source-only, read-only review. No source edits, test
execution, release, install, publish, dispatch, Docker/OrbStack/Velnor
runtime, or hostile payload execution.

Reviewed remote ref:

```text
origin/dual-lane-apt-schema2
e862e5913b7ead8c5bbb2111657dd697a6a23782
```

Read-only remote evidence:

```text
git ls-remote origin refs/heads/dual-lane-apt-schema2
e862e5913b7ead8c5bbb2111657dd697a6a23782 refs/heads/dual-lane-apt-schema2
```

The reviewed tip has parent `cea653c9bf26bbe8cd086d1f153ace4eb8415dd4`
(`fix(apt): isolate supported gpgv verification`), whose parent is
`71a8a5391e5596c5c37a90376b0c505419dae196`. The requested parent SHA
`cea6539bf26bbe8cd086d1f153ace4eb8415dd4` was one character short; the full
object above is the exact parent in e862.

## Verdict

**BLOCKED. No G1/provider or APT publication approval.**

The tip materially adds provider reconciliation, paginated asset census,
immutable tag-to-commit checks, raw-byte manifest/artifact checks, a parsed
release-attestation contract, and an isolated `gpgv` command shape. Those are
useful fail-closed gates. They do not establish a native producer acceptance
path or live cryptographic/expiry proof: the native release workflow does not
publish the newly mandatory `release-attestation.json`, all provider/GPG
positive evidence is a stub or synthetic fixture, and the GPG status parser
does not independently reject the documented expiry/revocation status classes.

## Exact source hashes

```text
tip                                      e862e5913b7ead8c5bbb2111657dd697a6a23782
apt.rs                                   7967bba2c1d563b99370a1cc07c44d8360bfd385
GPG fix parent                            cea653c9bf26bbe8cd086d1f153ace4eb8415dd4
```

The e862 tree changes `crates/velnor-workflow/src/apt.rs`; it does not change
the native producer workflow to emit or attest the new release handoff.

## Blocking findings

1. **Native producer cannot satisfy the new required attestation asset.**

   `parse_discovery_selection` requires `release-attestation.json`; fetch then
   reads it, checks its exact source/release/ref/commit/manifest/artifact
   bindings, and sends its raw bytes through `gh attestation verify`
   (`crates/velnor-workflow/src/apt.rs:2983-3028,3194-3237`). The checked-in
   native workflow's release asset list contains release record, manifest,
   release-manifest, checksums, tarballs, `.deb`s, and sidecars, but no
   `release-attestation.json` (`.github/workflows/release.yml:4262-4379`). It
   explicitly says it does not push or dispatch the APT consumer
   (`release.yml:4381-4383`).

   The native assembly test inserts a one-byte synthetic placeholder for that
   name, sets synthetic provider IDs/assets, and only parses the selection
   (`apt.rs:8885-9031`). It never runs provider acquisition, downloads native
   release bytes, or verifies a genuine attestation. Therefore the actual
   producer-to-consumer path is missing; the correct current behavior is
   fail-closed, not a positive acceptance claim.

2. **Sigstore verification is delegated correctly in shape, but is unproven
   and not independently trust-root pinned.**

   Production invokes `gh attestation verify` with repository, exact source
   ref/digest, signer workflow, GitHub OIDC issuer, SLSA predicate, hosted-runner
   restriction, and JSON output (`apt.rs:3035-3077`). It then checks the
   subject digest, certificate issuer, hosted runner, source repository/ref/
   digest, predicate type, signer URI prefix, and nonempty verified timestamps
   (`apt.rs:3091-3157`). This is a sensible identity gate around a real `gh`
   verifier, but the source review cannot upgrade it to cryptographic proof.

   The test `discovery_gh_stub` emits forged JSON records; the 14-case
   adversarial test uses that stub and merely asserts a nonempty error
   (`apt.rs:8768-8883`). No genuine bundle is verified, no wrong/expired
   certificate is exercised, and no native release-attestation asset is
   consumed. `run_fixed` resolves ambient `gh` from `PATH` and inherits the
   process environment (`apt.rs:1595-1645`); the code does not pin a verifier
   binary/version, use `--custom-trusted-root`, or pin an explicit API host.
   These may be acceptable deployment policy choices, but they must be stated
   and proven in the Linux hosted canary before acceptance.

   GitHub's primary CLI documentation says successful verification validates
   the artifact, actor identity, and predicate; its JSON `verificationResult`
   contains certificate/timestamp values not manipulable by the workflow while
   statement predicate fields can be workflow-controlled. See
   [the GitHub CLI attestation verifier contract](https://cli.github.com/manual/gh_attestation_verify).
   This is why parsed certificate claims must not be treated as a substitute
   for executing the real verifier.

3. **GPG isolation shape is present; expiry/revocation acceptance is not
   demonstrated and status classes are not explicitly fail-closed.**

   `gpgv_argv` supplies `--status-fd 1`, `--no-options`, a fresh `--homedir`,
   and one materialized explicit keyring (`apt.rs:1648-1664`). Live publication
   reads all input bytes first, materializes them into fresh scratch paths, and
   invokes this shape for detached record and `InRelease` signatures
   (`apt.rs:5908-5974`). `gpgv_signer` requires exactly one UTF-8 `VALIDSIG`,
   validates both signing and primary fingerprints, and binds the primary to the
   configured publisher (`apt.rs:1666-1698`). The subkey/primary synthetic
   parser test covers direct primary, valid subkey, foreign primary, duplicate,
   malformed, and non-UTF-8 status (`apt.rs:9813-9859`).

   The official GnuPG `gpgv` documentation says `gpgv` treats keyring keys as
   trusted and does not itself check expired or revoked keys, while the status
   protocol defines distinct `EXPSIG`, `EXPKEYSIG`, `REVKEYSIG`, and `BADSIG`
   outcomes. See [gpgv's upstream documentation](https://github.com/gpg/gnupg/blob/master/doc/gpgv.texi)
   and [the upstream status protocol](https://github.com/gpg/gnupg/blob/master/doc/DETAILS).
   This implementation only extracts `VALIDSIG` and relies on process exit;
   it does not explicitly reject those status classes, pin/hash the keyring,
   or census the allowed primary/subkey material. No real Linux `gpgv` run,
   expired key, revoked key, wrong keyring, or algorithm case was executed.
   A hosted acceptance test must prove the intended policy, or the parser must
   make that policy explicit.

4. **Provider authority is improved but owner identity and schema completeness
   remain partial.**

   The code queries repository and numeric release endpoints, checks positive
   repository/release/asset IDs, exact repository/release URL forms, tag,
   target commit, publication state, channel, asset IDs/names/sizes/states,
   canonical browser/API URLs, and exact equality between embedded and
   paginated asset censuses (`apt.rs:2649-2788,2906-2979`). Tag refs are
   dereferenced through direct or annotated Git objects and must resolve to
   the selected commit; preview ancestry is checked (`apt.rs:2794-2904`).

   However, only a positive repository numeric ID is retained; there is no
   expected owner numeric ID/login binding or exact trusted owner object. The
   parser also accepts a single-page object shape for provider doubles, even
   though production requests `--paginate --slurp` (`apt.rs:2681-2698`).
   Release `published_at` is compared as an opaque nonempty string rather than
   validated as a timestamp. These gaps need either a deliberate contract
   decision or strict typed provider schema/negative tests. They are not
   compensated by canonical-looking URLs in untrusted selection JSON.

## What is covered, and what is not

Present in source:

- fresh provider repository/release/tag/API reads before asset download;
- numeric release/asset selection, canonical URL checks, duplicate/missing
  asset rejection, and embedded-versus-paginated census equality;
- direct and annotated tag resolution to the selected immutable commit;
- exact raw manifest/sidecar/attestation/artifact digest checks before the
  discovery selection is persisted (`apt.rs:3241-3387`);
- exact parsed release-attestation identity and artifact inventory checks;
- isolated supported `gpgv` command arguments and signing-subkey-to-primary
  binding.

Not proven by this source review:

- any successful native producer release-attestation handoff;
- real GitHub API/provider response authenticity beyond the genuine `gh`
  implementation being invoked in production;
- real Sigstore bundle signature, certificate validity, source binding, or
  expiry checks;
- real Linux GPG primary/subkey verification, expiry/revocation behavior, or
  keyring trust boundary;
- a positive owner numeric identity contract;
- exact expected-error assertions for the adversarial cases. The cases cover
  wrong release/source/tag, missing/duplicate/mixed assets, URL changes,
  draft state, forged attestation fields, payload mutation, and API failure,
  but each only checks that *some* error was returned.

## Required hosted acceptance gate

Run only in a disposable GitHub-hosted Linux canary after the native producer
emits the missing asset. Never run these hostile/provider commands on the Mac
workstation. The canary must capture redacted structured outcomes, not secret
bytes:

1. Resolve the exact remote ref and release/tag object. Query the repository,
   numeric release, embedded assets, paginated `/assets?per_page=100`, direct
   or annotated tag object, and (for preview) compare response. Require exact
   owner/repository numeric identity, release ID/tag/channel/status/time, raw
   asset census, and tag-to-commit equality. Missing, duplicate, unknown,
   non-uploaded, wrong-ID, wrong-URL, wrong-size, pagination, and owner inputs
   must fail before download.
2. Require a real native `release-attestation.json` asset. Download every
   selected asset by numeric ID into a disposable directory, hash exact raw
   bytes, parse only after hash, and require the attestation's source
   repository/ref/commit, release ID/tag/URL, target commit, manifest digest,
   and exact artifact inventory to equal the provider facts.
3. Run the real pinned `gh attestation verify` with the source/ref/digest,
   exact signer workflow, GitHub OIDC issuer, SLSA predicate, hosted-runner
   restriction, and (if policy requires) explicit trusted-root/host flags.
   Check a real positive bundle plus wrong subject, wrong source/ref/digest,
   wrong signer workflow, wrong issuer, self-hosted, expired, missing, and
   duplicate records; all negatives must fail closed.
4. Run actual Linux `gpgv` over detached record and `InRelease` signatures
   using a newly materialized keyring and fresh `--homedir`; require one
   `VALIDSIG` bound to the pinned primary and explicit rejection of
   `BADSIG`, `EXPSIG`, `EXPKEYSIG`, `REVKEYSIG`, missing/foreign/duplicate
   signatures, expired/revoked key material, and unexpected keyring bytes.
   Record tool versions and keyring SHA-256; do not treat synthetic status text
   as cryptographic evidence.
5. Only after all checks pass, persist the immutable selection and feed the
   existing verify/publish gate. Missing producer attestation, any provider
   disagreement, any real verifier failure, unreadable bytes, timeout, or
   malformed result must leave no positive handoff.

No runtime command above was run for this review. No G1 approval is granted.

