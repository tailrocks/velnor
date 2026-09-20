# Independent Homebrew successor review

## Verdict

**BLOCKED for strict audit isolation and no-host-global-write approval.** The successor closes the prior host-prefix tap mutation: the real online audit passed in an owned disposable runtime, the checked-out formula copy was exact, failure/TERM cleanup worked, and the generator state remained current. It does not yet bound inherited Homebrew credentials/configuration or the temporary-parent path. No install, publication, dispatch, or G2 approval is granted.

## Exact pin and scope

- Remote: `https://github.com/tailrocks/homebrew-velnor.git`
- Branch: `codex/github-first-homebrew`
- Reviewed tip: `86bc621bdeec8b408a854d902b002b807c93e975`
- Tree: `349531825cb59ab9f78cf3211ac4f75a31b126ac`
- Parent: `fb647c86c3393b60f488a3b969c01299941dd2a9`
- Prior comparison: `257e7fc8805e1d501b97aaef54bb14a880b39444`
- Isolated tree: `/private/tmp/velnor-homebrew-257-review`, detached and clean
- Delta from 257: only `scripts/homebrew-audit-checkout.sh` (59 insertions, 10 deletions)
- Prior report: `G1/reviews/homebrew-257e7fc-independent.md`, SHA256 `3407be839d8982bc7673add926592a42d9e99fa899062b2b5a6e57658779225e`
- Remote branch resolved to exact tip during review.

No source files were edited. Disposable fake fixtures and logs were outside the source tree.

## Passes

### Exact checkout binding and owned copy

At `scripts/homebrew-audit-checkout.sh:4-18,49-72`, the helper resolves its own repository root, requires the stable formula plus both templates, obtains a brew repository, creates a random tap below the new sandbox, and copies only `Formula/velnorctl.rb` into that tap. The fake audit harness verified the copied formula as a regular file with the exact source SHA:

```text
source/copy SHA256 = 29feb24e0746cd9f9f94edf65b7100ea0276c18e73f5e0f1a4baf7788fd1fbb6
```

The generated formula reference was `velnor-contract-audit/velnor-contract.<random>/velnorctl`; no ambient `tailrocks/homebrew-velnor` tap was selected.

### Current real runtime symlinks

The host Homebrew runtime used for the real audit was Homebrew `7.0.4-27-gb1d4cf0`. Its `Library/Homebrew` tree contains 165 symlinks; all 165 resolved inside that source tree. The exact checked-out Formula files are regular files, not symlinks. Thus the observed host copy has no outside-target symlink.

This is an observed-host pass only; the script has no post-copy symlink containment check (see blockers).

### Real online audit and host tap immutability

Command:

```text
bash scripts/homebrew-audit-checkout.sh
```

Result:

```text
homebrew audit: auditing checked-out .../Formula/velnorctl.rb via velnor-contract-audit/velnor-contract.<random>/velnorctl
==> Downloading Homebrew API data
✔︎ JSON API packages.arm64_golden_gate.jws.json
homebrew audit: audited checked-out .../Formula/velnorctl.rb
script-status=0
global-tap-tree-unchanged=PASS
global-tap-stat-unchanged=PASS
owned-sandbox-residue-count=0
```

The snapshot covered the `/opt/homebrew/Library/Taps` path census (through the tap-owner depth) and stats before/after the audit. This proves the normal local run did not mutate the observed host tap list. It does not prove the hostile inherited-environment cases below.

### Failure and signal cleanup

Disposable fake-brew cases used the exact helper and an external fake host repository. Every case copied the exact formula bytes into the owned sandbox and left zero `velnor-homebrew-audit.*` directories:

```text
success: exit 0, sandbox-residue=0
failure: exit 99, sandbox-residue=0
TERM:    exit 143, sandbox-residue=0
```

The TERM case showed the expected trap path (`trap 'exit 143' TERM` at lines 45-47) and cleanup. The prior fake host repository was not the real Homebrew prefix.

### Generator/source tracking

Pinned generator:

```text
source revision b9c3156cdb88e63c11b9e595a3e694b02238c09
```

`/private/tmp/g2-homebrew-generator-b9/target/debug/velnor-workflow --plain --check` passed at exact 86. All generated files, including `.github/ci/.github-actions-generator-state` and `ci-unit-homebrew.yml`, were reported `= unchanged`. The Homebrew unit still renders:

```text
PR: mise run homebrew-audit ; mise run homebrew-contract-test
```

### Focused checks

- `bash -n` on all repository shell scripts: **PASS**
- `ruby -c` on stable formula and both templates: **PASS**, all `Syntax OK`
- `bash scripts/test-package-update.sh`: **PASS**, `Homebrew contract fixture validation passed`
- `actionlint`: **PASS**, exit 0
- `git diff --check fb647c8..86bc621`: **PASS**
- Detached source status: **clean**

## Blocking findings

### P0 — inherited Homebrew credentials/configuration are not scrubbed

The audit subshell at `scripts/homebrew-audit-checkout.sh:75-86` sets sandbox paths and a few flags, but it does not clear inherited `HOMEBREW_*` variables. A fake-brew audit invoked with deliberately hostile inherited values observed:

```text
HOME=/private/tmp/velnor-homebrew-audit.<id>/home
XDG_CONFIG_HOME=/private/tmp/velnor-homebrew-audit.<id>/config
HOMEBREW_CACHE=/private/tmp/velnor-homebrew-audit.<id>/cache
HOMEBREW_LOGS=/private/tmp/velnor-homebrew-audit.<id>/logs
HOMEBREW_TEMP=/private/tmp/velnor-homebrew-audit.<id>/temp
HOMEBREW_NO_AUTO_UPDATE=1
HOMEBREW_GITHUB_API_TOKEN=SECRET_TOKEN
HOMEBREW_SSH_CONFIG_PATH=/host/ssh/config
HOMEBREW_REPOSITORY=/host/repository
```

The path flags and `NO_AUTO_UPDATE` override correctly. The credential/config values remain inherited. This is not only a fake-brew artifact:

- Current Homebrew `bin/brew:300-316` preserves every non-empty `HOMEBREW_*` variable when it re-execs the Ruby runtime.
- Current Homebrew `Library/Homebrew/extend/ENV/sensitive.rb:62-63` explicitly exempts `HOMEBREW_GITHUB_API_TOKEN` while evaluating formulae.
- Current Homebrew `Library/Homebrew/brew.sh:618-622` turns `HOMEBREW_SSH_CONFIG_PATH` into `GIT_SSH_COMMAND`.
- Homebrew documents `HOMEBREW_CURLRC` as a curl config path (`env_config.rb:280-284`) and the GitHub token/SSH config as credential-bearing settings (`env_config.rb:463-469,724-728`).

An audit evaluates the checked-out formula. A formula under review can therefore observe the inherited GitHub token, use inherited SSH/curl configuration, or influence network behavior. `HOME`/XDG isolation does not remove environment credentials. Other unbounded variables include `HOMEBREW_FORCE_API_AUTO_UPDATE`, API/artifact/bottle domains, curl/git/ruby overrides, and registry tokens. The helper needs a deliberate clean environment/allowlist for audit, not only path assignments.

### P1 — absolute `TMPDIR` is not a confined parent

At `scripts/homebrew-audit-checkout.sh:27-30`, the helper accepts any absolute `TMPDIR` and uses it directly as the `mktemp` parent. It does not reject `/`, resolve symlinks, require a caller-owned directory, or verify the parent after creation.

The disposable hostile test set `TMPDIR` to an absolute symlink:

```text
TMPDIR=/private/tmp/velnor-homebrew-86-fake-fixture/tmp-link
tmp-link -> .../host-tmp-target
```

The helper successfully created the audit sandbox under the symlink path, and the fake audit observed that path. Cleanup removed it afterward. This proves the path boundary is controlled by the symlink target. If `TMPDIR` points at `/opt/homebrew/Library/Taps` or another host-global directory, the claimed sandbox writes there during the run even though the normal local snapshot passes. The path must be anchored to a validated private parent or created with a trusted parent independent of inherited `TMPDIR`.

### P1 — formula/template symlink escape is accepted

`[[ -f "$formula_path" ]]` at line 16 follows symlinks, and `cp -- "$formula_path"` at line 72 follows the target. There is no `[[ ! -L ... ]]`, `realpath` containment check, or `O_NOFOLLOW`-equivalent handoff. The current exact checkout has regular Formula files, so this was not an active current-tree failure; it is a missing structural guard for a checked-out symlink that resolves outside the repository. The same unchecked `-f` behavior applies to the two template census checks at lines 17-18.

The copied Homebrew runtime's current 165 symlinks are all in-tree, but the helper does not enforce that property for a future/different runtime. `cp -R` therefore remains an observed-host property, not a proof of generic confinement.

## Inherited boundary and residuals

- The old host-prefix tap-owner mutation is closed for a normal trusted `TMPDIR`: tap, cache, logs, temp, HOME, and XDG config are under the owned sandbox; normal/failure/TERM cleanup passed.
- `HOMEBREW_NO_AUTO_UPDATE=1`, `NO_ANALYTICS=1`, `NO_ENV_HINTS=1`, `NO_INSTALL_CLEANUP=1`, and developer mode are explicitly set in the audit subshell. `HOMEBREW_FORCE_API_AUTO_UPDATE` is not cleared, so the no-update claim is not absolute under inherited configuration.
- The copied real Homebrew binary recomputes repository/prefix/library from its sandbox location, so ordinary inherited `HOMEBREW_REPOSITORY`/`PREFIX` values do not redirect the observed real runtime. The fake harness still saw those values because a fake binary cannot provide Homebrew's internal reset; this remains an environment-contract dependency, not a source proof.
- Local real-audit evidence proves one macOS Homebrew installation only. It does not pin a Homebrew version or prove Linuxbrew runner behavior.
- Prior blockers remain: central empty aggregate/pagination, non-native/clean-client coverage, native macOS publication, and install/upgrade proof. This successor does not address them.

## Final decision

- Exact checked-out formula handoff: **PASS for current regular-file tree**.
- Owned sandbox/copy/cleanup: **PASS for normal trusted parent; hostile parent boundary BLOCKED**.
- Real online audit: **PASS locally**, exit 0; no host tap mutation observed.
- Generator state/source tracking: **PASS**, exact pinned `--check` unchanged.
- Credential/config isolation: **FAIL/BLOCKED** due inherited Homebrew credentials/configuration.
- Symlink confinement: **FAIL/BLOCKED structurally**; current files happen to be regular/in-tree.
- Install, publication, dispatch, or G2 approval: **not granted**.
