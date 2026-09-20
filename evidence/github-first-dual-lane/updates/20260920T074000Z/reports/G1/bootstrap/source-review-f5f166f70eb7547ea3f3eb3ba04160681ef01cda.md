# Independent G1 bootstrap-isolation source review — `f5f166f70eb7547ea3f3eb3ba04160681ef01cda`

Observed 2026-09-20 Asia/Ho_Chi_Minh. Reviewed exact detached source tree `f5f166f70eb7547ea3f3eb3ba04160681ef01cda` on `codex/g1-bootstrap-isolation`; parent `6d231d43fc5a05d29ea375757779fcd792da2e61`. Source worktree was left unchanged. This report is the only external write.

## Verdict

**Source seams PASS; adoption remains CHANGES REQUIRED. No G1 approval.** The c013 producer-control repair is present in the generator and its four offline executable transport fixtures pass. The checked-in generated workflows are stale relative to the repaired generator under the declared pin, and the producer builder digest is intentionally unassigned. Neither is security proof or G1 evidence.

## Verification

In a fresh `git clone --no-local`, detached at the exact SHA, with real local `python3` selected ahead of the untrusted-checkout mise shim:

```text
PATH="/opt/homebrew/bin:$PATH" CARGO_NET_OFFLINE=true rtk cargo test --locked --manifest-path crates/velnor-workflow/Cargo.toml --test bootstrap_transport -- --test-threads=1
cargo test: 4 passed (1 suite, 97.12s)
```

The four tests are actual generated acquisition/producer/execute shell runs. They replace `gh`, `curl`, `git`, `docker`, `velnor-workflow`, and related tools with fixture-local commands; archives are harmless local ZIP/TAR files. No GitHub endpoint, upload endpoint, real Docker daemon, candidate binary, canary, host payload, or hosted runner was used.

The acquisition negative suite mutates and rejects: service digest, API tree, object tree, run head, artifact run, run attempt, stale/late artifact, duplicate run/job/artifact, missing artifact, failed run, wrong job, PR-head workflow substitution, extra uploader, and dynamic uploader. The positive path verifies measured raw ZIP hash and parsed handoff fields. Producer and execute paths assert artifact lifetime, candidate-source-only mount, fixed UID, exact source environment allowlist, and rejection of extra/symlink output.

## c013 repair review

- **Exact checkout roles:** `crates/velnor-workflow/src/s2/primitives/ir.rs:3001-3016` renders exactly two pinned checkout steps: base repository/base SHA into `candidate-control`, and PR head repository/head SHA into `candidate-source`; both use depth 1 and disable credentials.
- **Trusted control cwd/build/cleanup:** `ir.rs:3017-3019` binds the host build step to `candidate-control`; `ir.rs:3061-3070` verifies both checkout identities and rejects links/special files; the Docker bind at `ir.rs:3072` targets only `candidate-source`; container inspection requires exactly one read-only source bind (`ir.rs:3080-3094`). The EXIT cleanup trap is at `ir.rs:3073-3079`, and the upload-surface cleanup step is explicitly `working-directory: candidate-control` at `ir.rs:3138-3141`.
- **PR script/action isolation:** `crates/velnor-workflow/src/s2/mod.rs:4832-4892` rejects producer actions outside the checkout/upload allowlist, rejects repository-local actions, and requires every host `run` step to name `candidate-control`. The policy template requires exactly two checkout uses, exactly three total `uses` entries, zero local actions, and exact base/head checkout fields (`s2/mod.rs:5187-5207`). It compares the base and PR-head `candidate_producer` blocks byte-for-byte (`s2/mod.rs:5208-5211`), so PR workflow edits cannot alter the trusted producer contract.
- **Upload namespace:** the policy contract requires one fixed candidate uploader/name/path (`s2/mod.rs:5212-5217`); the reachable workflow graph scan checks one candidate uploader per archive, rejects producer secondary/non-static uploaders and candidate-like dynamic names (`s2/mod.rs:4931-4953` and the embedded namespace scan). The executable hostile fixtures cover substitution, extra uploader, and dynamic uploader and all reject.

The producer test reaches fake Docker and checks the resulting stage still contains `velnor-workflow`, manifest, container/after/exit records, and the upload surface. This is lifecycle evidence only; fake Docker is not Docker isolation evidence.

## Known generated fixed-point failure

`.github-gen/velnor-workflow.toml:9` still declares generator revision `fdeed261bd2247a38db6922a7726cd45d3d6f31e`. The repaired source template expects two producer checkouts, but checked-in `.github/workflows/ci-pr.yml:115-121` still has one PR-head checkout and no `candidate-control`; checked-in `.github/workflows/ci-policy.yml:111` likewise encodes the pre-repair one-checkout contract. This is generated drift under the old declared pin, not evidence that the repaired generator contract is adopted. Regeneration/pin promotion is required before treating the workflow surface as current.

The checked-in producer also has an empty builder digest (`.github/workflows/ci-pr.yml:128`); the shell correctly rejects it at line 134. The fixture supplies only a local `sha256:` value in `bootstrap_transport.rs:357-363` to reach fake Docker. This is a test-only digest override and provides no image identity or security proof.

## Boundary / non-claims

- No source, generated workflow, branch, GitHub, Docker, publication, or installation state was changed; no commit or push was made.
- This is an exact-source and offline fixture review only. It does not establish generated fixed-point adoption, hosted behavior, real action archive integrity, a Docker/canary result, candidate execution safety, or G1 approval.
