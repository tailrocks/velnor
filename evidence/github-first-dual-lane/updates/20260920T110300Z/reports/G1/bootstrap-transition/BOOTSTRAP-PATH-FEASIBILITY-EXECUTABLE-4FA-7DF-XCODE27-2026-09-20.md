# Executable 4fa admission probe: 7df xcode-27 fixture

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

## Verdict

Published 4fa executes and admits the baseline generated tree's runner
semantics, but rejects an isolated xcode-27 output:

    FAIL trusted-runners
      .github/workflows/ci-runtime-products.yml: job build: runs-on does not match any declared provider selector

This is old-validator source behavior, not candidate execution. The 4fa S2
policy source classifies only labels beginning with ubuntu-, macos-, or
windows- as GitHub-hosted (4fa s2/policy.rs lines 2112-2117 and 2752-2767).
Other labels must equal a declared provider selector exactly (lines 2197-2207
and 2772-2785). xcode-27 matches neither rule. The 7df source separately
requires exact xcode-27 before rendering (native_contract.rs lines 486-529;
platform.rs lines 280-287). Both contracts cannot pass in an ordinary PR while
4fa remains the validator.

No candidate binary, pin-build, candidate manifest, GitHub write, dispatch,
merge, release, or adoption was used.

## Immutable identities

- Audited snapshot: 7df0481e3f38f9f662d25cd963c99d24bc32357b, tree
  c6dbb8dace01b9a84ec169ad9142d9fe6995a40e.
  Parents: f6cb27c4606103d0c879bbc60a6a060911ce7b91 and base
  9e5c0eb215d4169578d6f064806e89fe4c793e85.
- Trusted validator: 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d, tree
  36462a36373085356cd24272afab063bdbd390fa.
- Published release: 392384468,
  velnor-workflow-runtime-v1-af140ad4d8d84326,
  closure af140ad4d8d84326d5676f626ce4261fd7914ac41d521d851484dcb8253270e7.
- Manifest SHA-256:
  1c3d1b0776d21fb6ba988064a18cbe2752639dcbde3ab58e12e615df72082f88.
- Linux-X64 asset SHA-256:
  672de6b171eba0f9e8053e96974cf361a6788af22ca2d498eabba71b1d73c236.
  Attestation verification succeeded; source digest and workflow signer identify
  4fa and ci-runtime-products.yml.
- Executable control: same release's attested macOS-ARM64 asset,
  SHA-256 7bee5aabdea114e3c7a869101b7bd951619455605addfcb896e69e229e5af49c.
  It reports revision 4fa and closure af140ad4d8d84326d5676f626ce4261fd7914ac41d521d851484dcb8253270e7.
  Its attestation source digest is 4fa and workflow is
  .github/workflows/ci-runtime-products.yml.

Asset proof commands:

    gh attestation verify /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-Linux-X64 --repo tailrocks/velnor --format json
    gh attestation verify /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 --repo tailrocks/velnor --format json
    shasum -a 256 /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-Linux-X64 /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64
    /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 --revision
    /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 --closure

The two attestation commands returned verified SLSA statements whose source
digest is 4fa; the hash command returned the digests listed above; revision
and closure returned 4fa and af140ad4d8d84326d5676f626ce4261fd7914ac41d521d851484dcb8253270e7.

The Linux-X64 asset is ELF x86-64; the execution host is Darwin arm64:

    /tmp/velnor-4fa-runtime.qgE9ts/verified-velnor-workflow --revision
    zsh: exec format error: .../verified-velnor-workflow
    exit 126

No Docker or QEMU was used. A Linux-ARM64 control attempt in the existing
isolated Linux VM was also not used for the result: that asset requires
GLIBC_2.39, while the VM lacks that version and lacks git.

## Disposable fixtures

Clean clone:

    /tmp/velnor-4fa-7df-probe.VM6IqM

The xcode fixture starts from the same commit and changes only generated-output
bytes in:

    .github/actionlint.yaml
    .github/workflows/ci-runtime-products.yml

Exact replacement: macos-26 to xcode-27. No .github-gen/velnor-workflow.toml
bytes changed; its SHA-256 remains
d42a10c819ea665455c809bae87a2e57a60f596ac9dbf61ec55500b957e41ca5.
Synthetic output diff SHA-256:
f9de5996f9ce2c77dbee6cedfe67f1428545991b41cdaf90d6ef97a9594e727a.

Changed-file hashes:

    .github/actionlint.yaml
      xcode 10d7eee7db15ceff26921b8a688e9d81219df0c0bad01c098776186fe9308ae6
      7df   44ad924472fd9d2dd4bb08755e872dea4ad8140cfbd169242c079fb4af507789
    .github/workflows/ci-runtime-products.yml
      xcode 93eb04cb68acdf0d778cdd5214484b4e2b6fb9cb9b819c5bc46545c0a1d4e7c7
      7df   1b28c3159a6d76a72fb1ee8163f4e53b8ce3e60e5b9e3b72b08151885383a596

This is a label-only output fixture, not a trusted candidate render. The 7df
candidate generator was not executed.

## Exact commands and results

All policy calls used the attested 4fa macOS-ARM64 binary, explicitly set
VELNOR_WORKFLOW_PINNED_BINARY, unset VELNOR_WORKFLOW_CANDIDATE_MANIFEST, and
omitted both --candidate-manifest and --pin-build. Base SHA was 9e, head SHA
was 7df, and contexts were DCO,Policy,ci-required.

### Published generator check

Clean fixture:

    cd /tmp/velnor-4fa-7df-probe.VM6IqM
    /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 --plain --check /tmp/velnor-4fa-7df-probe.VM6IqM

Exit 1. Raw failure:

    error: generated files differ: .github/ci/project.toml, .github/ci/.github-actions-generator-state; rerun generate

This is published-render drift, independent of semantic runner admission.

Xcode fixture:

    cd /tmp/velnor-4fa-7df-xcode27.nbiD01
    /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 --plain --check /tmp/velnor-4fa-7df-xcode27.nbiD01

Exit 1. Raw failure:

    error: refusing to overwrite manually modified generated file: .github/actionlint.yaml; restore its recorded generated content or reconcile it manually

The old generator does not silently accept the xcode output as its own render.

### Semantic policy: clean 7df output

    cd /tmp/velnor-4fa-7df-probe.VM6IqM
    env -u VELNOR_WORKFLOW_CANDIDATE_MANIFEST VELNOR_WORKFLOW_PINNED_BINARY=/tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 policy --workflow-root /tmp/velnor-4fa-7df-probe.VM6IqM --head-sha 7df0481e3f38f9f662d25cd963c99d24bc32357b --base-sha 9e5c0eb215d4169578d6f064806e89fe4c793e85 --base-revision 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d --ruleset-contexts DCO,Policy,ci-required

Exit 1; only generated-tree failed. Runner semantics passed.

    PASS pin-declared
    PASS pin-reachable
    PASS pin-monotonic
    PASS entrypoint-pin
    FAIL generated-tree
    PASS pull-request-target
    PASS entrypoint-privileges
    PASS trusted-runners
    PASS action-pins
    PASS workflow-structure
    PASS required-checks
    policy: 11 rules, 1 failed
    error: workflow policy failed: generated-tree

### Semantic policy: xcode-27 output

    cd /tmp/velnor-4fa-7df-xcode27.nbiD01
    env -u VELNOR_WORKFLOW_CANDIDATE_MANIFEST VELNOR_WORKFLOW_PINNED_BINARY=/tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 /tmp/velnor-4fa-runtime.qgE9ts/velnor-workflow-macOS-ARM64 policy --workflow-root /tmp/velnor-4fa-7df-xcode27.nbiD01 --head-sha 7df0481e3f38f9f662d25cd963c99d24bc32357b --base-sha 9e5c0eb215d4169578d6f064806e89fe4c793e85 --base-revision 4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d --ruleset-contexts DCO,Policy,ci-required

Exit 1; generated-tree and trusted-runners failed.

    PASS pin-declared
    PASS pin-reachable
    PASS pin-monotonic
    PASS entrypoint-pin
    FAIL generated-tree
      - .github/actionlint.yaml: differs from the pinned render
      - .github/ci/.github-actions-generator-state: differs from the pinned render
      - .github/ci/project.toml: differs from the pinned render
      - .github/workflows/ci-runtime-products.yml: differs from the pinned render
    PASS pull-request-target
    PASS entrypoint-privileges
    FAIL trusted-runners
      - .github/workflows/ci-runtime-products.yml: job build: runs-on does not match any declared provider selector
    PASS action-pins
    PASS workflow-structure
    PASS required-checks
    policy: 11 rules, 2 failed
    error: workflow policy failed: generated-tree, trusted-runners

## Admission conclusion

Measured old-4fa contract:

1. macos-26 is admitted as a GitHub-owned hosted label.
2. xcode-27 is rejected as foreign by the same checker, even when output and
   actionlint contain xcode-27.
3. Adding xcode-27 to the existing github-hosted selector list does not repair
   the matrix: the old checker compares each scalar matrix value against a
   selector's complete label set, and xcode-27 is still not a GitHub-owned
   prefix. That isolated experiment was not counted as approval.
4. 4fa's generator hardcodes MACOS_HOSTED_RUNS_ON = macos-26 (s2/mod.rs lines
   5063-5066), so its check cannot produce the 7df xcode-27 transition.
5. 7df's native source has the opposite exact contract: xcode-27 is the only
   production Apple offer, and macos-26 is rejected before rendering.

No ordinary PR retaining trusted 4fa can satisfy both current xcode-27 native
source admission and old semantic admission. A protected validator/contract
transition is required; this probe does not authorize or perform it.
