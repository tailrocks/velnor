# A2 — Publish-before-pin gap analysis (read-only, no edits)

Spec: `plans/bastion-three-provider-ci/spec.md` §3.2 + work-plan STEP A2.
Mapped against: `crates/velnor-workflow/src/primitives/runtime_products.rs` (RP),
`crates/velnor-workflow/src/closure.rs` (CL), `crates/velnor-workflow/build.rs` (BR),
`.github/workflows/ci-runtime-products.yml` (PW), `.github-gen/sources/actions/setup-velnor-workflow/action.yml` (SA),
plus consumer-adjacent code the spec names: `policy.rs` (PO), `lib.rs` (LB), `primitives/ir.rs` (IR).

Legend: EXISTS = implemented in code (file:line). GAP = missing → G-numbered list at end.
"Static-only" = covered by string assertions on rendered YAML, never executed.

## §3.2 steps 1–6

| Req | Verdict | Evidence |
|---|---|---|
| S1: planning/policy/committed tree stay bound to R during C | EXISTS | PO:448-475 generated-tree verdict vs pin render; candidate exception passes on PR, fails on mainline (PO:466,473); bootstrap pinned to `workflow_revision` (LB:2601-2610,3994-3998; IR:4957-4984) |
| S2: C built/tested once per closure/platform; disposable trees; never a trusted pin | EXISTS | Head-anchored candidate: skip iff head==base closure (IR:1553-1557); merge-tree binary reused only if it reports head closure (IR:1561-1570), else clean head-worktree build (IR:1574-1582); fast-path condition pinned by test (CL:312-373); artifact name closure+platform-bound (IR:1594); render compared in scratch/disposable dirs (PO:1538-1544; IR:1583-1593); candidate only proves render equality, mainline fails until pin advances (PO:466) |
| S3: closure covers every build-affecting input; no one-crate-hash assumption | EXISTS + GAP G8,G9 | Paths+footer canonical form (CL:84-91,106-121), mirrored+test-pinned in build script (BR:28-39,73-104; CL:488-508); build.rs reads only git+env (BR:41-69); exact toolchain channel `1.98.1` (rust-toolchain.toml) + file-driven install (PW:125-128); hermetic guards RUSTFLAGS/RUSTC_WRAPPER/isolated CARGO_HOME (PW:136-146); whole-subtree pathspec + root manifest/lock (CL:82-91); no path-deps today (velnor-workflow/Cargo.toml — only crates.io/workspace/git-rev + dev-only path dep) |
| S4: merged reviewed source; published/attested via generated trusted hosted producer; exact-reuse; merge change → one new build | EXISTS | Generated owner-only producer (RP:156-159,541-546; PW rendered); main-push+dispatch only, no PR/schedule/tag triggers (RP:787-811); least-privilege perms + SHA-pinned actions (RP:1161-1193); HEAD-anchored closure (PW:54-60); asset+manifest attested where built/assembled (PW:174-177,250-254); tag-exists skip in build+publish + pre-create re-check, never overwrite (PW:66-78,83,191,242-249; RP:1135-1159); content-addressed tags (CL:98-104) |
| S5: all platform assets + attestations exist and verify before consumable | GAP G4 (ordering) | Pre-create checks EXIST: transport digests all 3 platforms (PW:217-228), native self-report spot check (PW:229-230), manifest accept-filter (PW:238-241). But `gh release create` (PW:247) precedes manifest attestation (PW:250-254) and the consumer-flow smoke test (PW:255-286) |
| S6: atomic promotion commit (pin+metadata+tree); pin/tree consistency checked | EXISTS (check) + GAP G5 (mechanism) | Consistency check EXISTS: mainline stale-pin fails with bump instruction (PO:466); no promotion command exists — bump+regen is manual message text (PO:466,473,580) |

## Consumer verification list (spec §3.2 ¶3)

| Req | Verdict | Evidence |
|---|---|---|
| C1 producer repository | EXISTS | Hardcoded product repo both consumers (SA:65,108,118-119,122-127; LB:4522 `--repo tailrocks/velnor`); producer owner-only (RP:157-159,774-785) |
| C2 full closure | EXISTS | Full 64-hex compared everywhere, tag is locator-only (SA:89-91,128-133,155-159,177-178; LB:4522; PW:162-163; BR:60-64 stamp; CL:45-48) |
| C3 source identity | GAP G1 | Release manifest has NO revision field (RP:218-220 shape is closure/profile/features/products); release `--target HEAD_SHA` lives in untrusted notes text (PW:247-248, unverified); binary `--revision` stamp (BR:57-60) is never checked by any consumer |
| C4 OS/architecture | EXISTS | RUNNER_OS-RUNNER_ARCH platform key in asset name + manifest lookup (SA:115,130,157,164-165; RP:205-229) |
| C5 features/profile | EXISTS | Shared accept filter (RP:69-74; SA:128-133,155-159; LB:4522); pinned equal by test (RP:1002-1050) |
| C6 manifest + binary digests | EXISTS | Manifest filter + recomputed sha256 vs manifest (SA:134-140,160-166; LB:4522; PW:217-228) |
| C7 trusted signer workflow/ref | EXISTS (workflow) + GAP G2 (ref) | `--owner + --signer-workflow …/ci-runtime-products.yml` both consumers + smoke (SA:122-127; LB:4522; RP:480-481; RP:1052-1102). No `--signer-ref`/`--cert-identity` anywhere (repo-wide grep-clean); producer `workflow_dispatch` can run from any branch |
| C8 binary self-report | EXISTS | `--closure` tripwire post-install all consumers + producer + smoke (SA:177-178; LB:4522; PW:162-163,229-230,285-286) |
| C9a cache hit waives nothing | EXISTS | Unconditional Verify step after restore-or-download (SA:148-166); Velnor slot reuse requires manifest-digest match + self-report (LB:4522; RP:1046-1049) |
| C9b PR checksum ≠ trust | EXISTS (construction) + test GAP G6 | No checksum input exists (SA:29-48); candidate acquire recomputes digest (LB:3891-3897) and equates manifest closure to locally computed pin candidate (LB:3899-3900); policy binds env candidate by locally-computed closure + digest-before-exec (PO:1483-1485,1492-1500,1524-1537) |
| C9c producer ≠ consumer identity | EXISTS + GAP G3 | Owner/consumer routing derived from action coordinate (LB:4416-4441; RP:774-785); setup-action API fallback uses product repo (SA:84). BUT Velnor provisioner fetches the generator pin from the consuming repo (LB:4522) |

## Work-plan A2 negatives / proofs

| Req | Verdict | Evidence |
|---|---|---|
| N1 missing product → precise producer defect, no retry-forever, no consumer compile | EXISTS (script text) | Names producer + closure (SA:118-121; LB:4522); single attempt, no retry loop; no cargo in SA (grep-clean) or Velnor provisioner |
| N2 wrong digest rejects | EXISTS (script text) + test GAP G6 | SA:140,166; LB:4522; IR-side LB:4538 |
| N3 wrong manifest rejects | EXISTS (script text) + test GAP G6 | Accept-filter gates (SA:128-133,155-159); malformed-digest gates (PW:221-224) |
| N4 untrusted signer/ref rejects | EXISTS workflow / GAP G2 ref + test GAP G6 | Attestation verify gates (SA:122-127; LB:4522) |
| N5 consumer-repo lookup confusion rejects | GAP G3 (+G6) | See C9c |
| N6 PR-checksum-as-trust rejects | EXISTS construction + test GAP G6 | See C9b |
| N7 zero cargo in Planning/Policy/unit bootstrap | EXISTS | Policy/consumer renders assert cargo-free (LB:12195,12422-12423,13528-13530,13549-13550,13810-13811); unit bootstrap = setup action or run-scoped artifact download (IR:4969-4984; LB:4535-4550). NB: IR:1579 `cargo build` is producer-side same-PR candidate packaging (IR:1596 same-repo gate), not a consumer fallback |
| D dedup per closure/platform; exact-reuse; merge change → exactly one new build | EXISTS | Tag-exists skip + pre-create re-check (PW:66-78,191,242-246; RP:1135-1159); `exists` probe fails safe (error → rebuild attempt → re-check skips); candidate binding executable-tested (PO/tests:1130 lying digest, 1164 foreign tree, 1192 unbound, 1221 malformed, 1323 false closure claim) |
| CC cold-consumer (empty cache resolves+verifies, zero waivers) | Code EXISTS + proof GAP G7 | Cache optional/miss-tolerant (SA:37-40,93-103,118-147); full verification on miss + re-verify on hit (SA:148-166). No test runs it cold |
| AP atomic promotion proof | GAP G5 | No mechanism, no test (check half exists per S6) |
| DA daemon never via `releases/latest`; separate release identities | Code EXISTS + test GAP G10 | Zero `latest` in generator/setup/producer/workflows/scripts/config (grep-clean); runtime tags `velnor-workflow-runtime-v1-*` (CL:79) vs app/daemon releases `v*` (release.rs:1368); daemon delivers via APT feed (spec §7), not releases. No test pins the disjointness |

## GAP list (10)

- **G1 source identity unverified.** Manifest carries no source revision; release target commit unverified; `--revision` stamp never consumed. Spec lists source identity separately from closure. Fix: carry `revision` (or target SHA) in manifest.json + verify (release-product path; the in-run artifact path already does `.revision == $revision`, LB:4538).
- **G2 signer ref unpinned.** Only `--signer-workflow`+`--owner`; add `--signer-ref`/cert-identity pinning (and/or constrain producer `workflow_dispatch` to default branch) in SA + LB:4522 + RP smoke; pin by test.
- **G3 Velnor provisioner resolves generator closure from the consuming repo.** `git fetch … "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY" "$PINNED_REVISION"` (LB:4522) vs setup action's product-repo API fallback (SA:84-86). Spec-negative "consumer-repo lookup confusion" + functional break for Velnor policy lanes in consumer repos (foreign pin unresolvable → fail). Fix: fetch/resolve from product repo like SA.
- **G4 release exposed before full verification.** `gh release create` (PW:247 / RP:455-457) runs before manifest attestation + consumer-flow smoke test (PW:250-286). A smoke failure leaves an immutable, tag-blocking release that future runs skip. Fix: verify-then-create (attest subjects pre-upload or verify-then-publish ordering) so step 5 holds literally.
- **G5 no atomic-promotion mechanism.** Only manual "bump [generator] revision … and regenerate" text (PO:466,473,580). Fix: one command producing the pin+metadata+full-tree commit; test that promotion output keeps pin/tree consistent (policy already fails stale mainline, PO:466).
- **G6 no executable consumer negative suite.** Missing/wrong-digest/wrong-manifest/untrusted-signer/lookup-confusion/PR-checksum covered only by static string assertions (LB:7591,7757-7758,7770-7837; RP:1002-1102). Fix: fixture-executed tests running SA script + Velnor provisioner against stubbed `gh`/`jq` proving each rejection. (Candidate-binding negatives already executable: PO/tests:1130,1164,1192,1221,1323.)
- **G7 no cold-consumer test.** Fix: run SA with cache disabled/empty against fixtures; assert full verification path + zero waivers.
- **G8 target/runner image outside closure.** `github_runner`/`macos_runner` labels feed the build matrix (RP:111-129) but not the digest (CL:84-91): a label change (incl. arch) reuses the stale tag product. Fix: fold platform→runner/target mapping into closure or manifest, or rebuild-on-label-change.
- **G9 no guard against future path-deps escaping closure.** Correct today (no `[dependencies]` path deps in velnor-workflow/Cargo.toml) but nothing fails if one is added. Fix: test asserting crate closure-completeness (no uncovered local path inputs).
- **G10 daemon/runtime identity separation unpinned.** True by absence (no `latest`; disjoint tag schemes) but no test. Fix: test asserting no `latest` selector in rendered daemon surfaces + tag-scheme disjointness (`velnor-workflow-runtime-v1-*` vs `v*`).
