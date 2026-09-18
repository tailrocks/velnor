# B1 verification verdict: CERTIFIED

Verifier: independent (not the author). Branch `feat/b1-apt-primitives` at
`ed4428555d50993f9f785298cba7c72577be57f2` (fetched from origin; matches
`/tmp/b1-impl.md`). DCO sign-off present. Diff: 5 files, +7473/−41 — matches
the claim. All work done in a fresh clone (`/tmp/vb1-clone`, clean checkout
of `ed442855`); no merges, no pushes. Scratch worktrees removed after use.

## Spec §7 capability coverage (all 11 present and exercised)

1. Source + release identity: `ReleaseIdentity` via `parse_stable_tag` /
   `run_resolve_commit` (`^{}` first, preview refuses); probed bad-slug
   rejection and short-commit rejection.
2. Package: `valid_package_name` + contract completeness; probed `x;id`.
3. Arch set: exactly {amd64, arm64} (`parse_arch_set`, `record_arch_join`);
   probed wrong-arch record + `apt_arches=[amd64,i386]` config rejection.
4. Suites: stable `vX.Y.Z` + preview `X.Y.Z~preview.N+7hex` grammars, dotted
   assets, suite paths, bootstrap-vs-strict; positive + negative probes ran
   for both suites.
5. Signer + secrets: full-fingerprint validation, `secret_ref` uppercase
   identifier, passphrase stdin-only (`--passphrase-fd 0`, asserted in code
   and author argv-log tests); probed short-signer + full-vs-full mismatch.
6. Coherence verification: line-by-line oracle port (error strings match
   `verify-release.sh`); record/manifest/deb/`SHA256SUMS`/OCI/extracted
   checks all present. Every rejection lands before the sentinel write.
7. Assembly: staging-only, deterministic pool, `apt-ftparchive`, fixed `gpg`
   argv, collision-bytes check, pinned Release fields.
8. Records: stable + preview-variant publication records; parse + emit.
9. Retention: typed `Retention`, policy 1, versions-per-arch = 2/2/1,
   `derive_previous_pointer` is a line-exact port of `publication-previous.jq`
   (diffed against the live oracle; same branches and error strings).
10. Pages deploy: single-writer concurrency group, `package-feed` +
    `github-pages` envs, `pages:write` + `id-token:write`, `apt-deploy-guard`
    no-rollback check (probed forward accept / rollback refuse).
11. Channel tasks: `apt-channel-update` emits `velnor.apt-package-state.v1`
    (probed positive + missing-pool fail-closed).

No shell/YAML from config: all subprocesses go through `run_fixed`/`run_in`
with code-constant programs and fixed argv; rendered values pass through
`shell_quote` (correct single-quote escaping) or charset validators that
forbid metacharacters; `secret_ref` is `[A-Z][A-Z0-9_]*` raw only into an
`env:` key; fetch patterns are code constants. `apt` through the old
`verify-feed`/`update-feed` stubs is rejected (homebrew-only now).

## Independent disproof attempts (novel fixtures, built binary)

- Stable: 14/14 — positive control with never-used names
  (`globex/sprocket`, `sprocketd`, `sident`) verifies + arms `.reprepro-ok`;
  bad source/digest/key/arch/incoherent inputs (14 cases incl. tampered
  record, extra deb, record source/arch/manifest-hash mismatches, signer
  mismatch, rebuilt-deb binary incoherence) all reject with no sentinel.
- Preview: 5/5 — positive control + tampered deb, extra deb, missing commit,
  suffix≠commit rejections, all pre-mutation.
- Generator: 28/28 — two novel-name fixture repos render real five-job
  feeds (no omission, single writer, both envs, all seven commands); shape
  diff modulo names is empty; generate-twice byte-identical; `--check`
  clean; malformed source/arch/shell configs fail loudly.
- Authored-test sensitivity: a planted name-keyed mutant
  (`valid_package_name` rejects `"widget"`) kills both renamed-fixture
  tests (verifier + generator layers) — they are non-vacuous.
- Runtime spot probes: previous-pointer `"preview"`/`null`/stable-object
  consts, stable+bootstrap refused, publish-without-sentinel refused,
  `update-feed --kind apt` rejected, fetch/resolve arg validation pre-network.

## Rerun gates (fresh clone)

- `cargo test -p velnor-workflow`: 549 lib + 55 integration (2+6+5+9+33),
  0 failures — matches the claim exactly.
- `cargo clippy --workspace --all-targets --locked --features
  velnor-runner/test-support -- -D warnings`: clean (exit 0).
- `cargo fmt -p velnor-workflow -- --check`: clean.
- `actionlint` and `actionlint -shellcheck` on the rendered `release.yml`:
  exit 0, no findings.
- `--plain --dry-run` exit 0 in the fresh clone (repo itself) and in fresh
  fixture dirs; generated files match the committed tree byte-for-byte.

## Findings (non-blocking)

- F1 — Config-digest provenance churn: the schema extension moves the
  `config` input hash (new `null` keys in canonical JSON, per the file's
  uniform no-skip convention), so repo-level `--check` reports "generated
  files match but generation inputs changed" until a promotion regen.
  Parent `--check` passes, confirming B1 as the cause. Outputs are
  unaffected; resolves via the B2 promotion regen. Expected mechanics.
- F2 — Record `target` triple unauthenticated (minor, undocumented): B1
  never checks per-arch `target` triples; the runner's
  `ReleaseRecord::verify` does. The oracle doesn't check it either, so B1
  matches its stated contract, and no byte binding depends on it. Suggest
  adding one line to the deviations list; no capability gap.

## Verdict

CERTIFIED: `feat/b1-apt-primitives` @ `ed442855` implements work-plan B1
(actions 1–6) and spec §7 faithfully, with independent positive/negative
proof at both the verifier and generator layers. No merges or pushes made.
