# G1 hostile producer fixture review: `b2546b46`

- Review timestamp: 2026-09-19T22:18:08Z.
- Reviewed commit: `b2546b4645eed1619896f06f79815d41eda62783`.
- Parent: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Tree: `77cdc6886030131c3c9e82afb741a460ec424894`.
- Branch: `codex/g1-bootstrap-hostile-fixture`, detached at `/private/tmp/g1-bootstrap-hostile-fixture`; no matching remote branch ref was advertised during review. Review is pinned to the commit.
- Scope: four added files only: `probe.rs`, `build.sh`, `fixture-contract.json`, `README.md`.
- Commit trailers: valid `Signed-off-by` and `Co-authored-by: Codex <codex@openai.com>`.
- Probe SHA-256: `c39cf177e94de2b5a1d2012beedfaaf295ffe972348bd0436897a40ce7087afe`.
- Build-script SHA-256: `97d480857138e1f46c6aefa492721f2268522a42d448b92229b40c125f25a01c`.
- Contract SHA-256: `f5c4217b73c928f9743b57027f5dbc3d4ede246a58ca852a645ca5476399a05`.

## Static checks

- `build.sh` compiled the probe into a disposable path with `rustc --deny warnings`; the result was a macOS arm64 Mach-O. The binary was not executed.
- `rustfmt --check --edition 2024 probe.rs`: pass.
- `jq empty fixture-contract.json`: pass.
- `git diff --check` for the fixture delta: pass.
- Secret-pattern scan over the four files: no match.

This is source/compile evidence only. No hostile probe, Docker command, network request, fork, file mutation, child pressure, or canary ran on the Mac.

## What the fixture does well

The probe is diagnostic-only and refuses normal execution unless it receives `--canary`, `HOSTILE_PROBE_ISOLATED=1`, PID 1, and `--output /output` (`probe.rs:478-528`). It emits fixed field names, bounded counts, and fixed status words, not environment values, URLs, response bodies, source bytes, or OS error text. Output writes are intended to remain under disposable `/output`; the contract explicitly says the trusted wrapper/verifier owns authority.

It covers useful bounded attempts: sensitive environment-name and `/proc/*/environ` scans (`probe.rs:53-143`), command-file environment probes (`probe.rs:201-244`), read-only `/input` and `/candidate` writes, symlink/hardlink creation, sparse file and bounded file-flood pressure, traversal, a forged handoff file, PID/UID/capability/no-new-privileges/seccomp inspection, and bounded child pressure (`probe.rs:293-454`).

## H1-H9 acceptance matrix

| Row | Static coverage | Review result |
| --- | --- | --- |
| H1 env/proc credentials | Names are counted without printing values; `/proc/1` and visible process environments are scanned. | Partial. `take(1025)` is not literally every process; no exact allow-list assertion or wrapper result validator is included. No canary proves parent/image env absence. |
| H2 command files | Checks five `GITHUB_*` path variables and writes only a disposable path when one points there. | Partial. It does not enumerate known command-file paths when variables are absent and does not prove later-step mutation is impossible. |
| H3 artifact impersonation | Parses runtime URL and sends bounded empty unauthenticated `POST`; reports token-name presence. | **Insufficient.** This is not an artifact upload or second-artifact attempt. It never uses a runtime token, so an unauthenticated POST cannot prove that a token-bearing process cannot impersonate the uploader. |
| H4 workspace/source/socket | Attempts writes to `/input` and `/candidate`; metadata-probes selected workspace paths. | Partial. No Docker/runner socket probe, no actual workspace/runner-temp write attempt, no deletion/rewrite of an existing source file, and no post-copy validator/harness. |
| H5 network | One fixed IPv4 connection plus possible runtime-endpoint connection. | **Incomplete.** No explicit DNS, loopback, IPv6, metadata, proxy, or Unix-socket cases required by the approved matrix. |
| H6 links | Creates a symlink and hardlink in disposable output. | Partial. It relies on the future wrapper to reject them; no archive/member/inode validator exists here. |
| H7 output abuse | Bounded sparse-file, 4096-file flood, traversal, and forged `handoff.json`. | Partial. No archive ZIP/TAR generation, duplicate normalized member/device checks, expected manifest-name forgery, or trusted post-copy quota validator. |
| H8 process/status hardening | Reads PID 1 status and starts at most 160 short-lived children. | Partial. No timeout/OOM/exit-propagation harness, cgroup quota assertion, or proof that seccomp is the approved profile rather than merely enabled. |
| H9 source/contract minting | Attempts `/input/.git` write and a forged handoff. | Partial. No exact source object/tree identity, cache/shallow/wrong-target case, image/config case, or independent contract verifier. |

No row is an acceptance result until the same measured binary runs in the fresh hosted final-image canary and the trusted wrapper independently validates every status, mount, process, output, digest, exit, quota, and timeout condition.

## Trust and image-contract gaps

1. `fixture-contract.json` is candidate-repository content and is explicitly diagnostic-only. It has no base-owned expected raw SHA-256 for `probe.rs`, `build.sh`, the compiled Linux binary, or any injecting action. A future harness must fetch/hash those bytes from the approved base pin before execution; it must not trust this JSON to select its own bytes.
2. `sandbox_base_image` records `ubuntu:26.04@sha256:2260313b...`, but this is the approved base/input digest, not the final platform manifest or config digest. The contract has no fields for final image repository, platform manifest digest, config digest, `RepoDigests`, or `Config.Env` equality. The README correctly warns that these must be supplied later, but the fixture does not enforce or record them.
3. The local compile output is Mach-O arm64 because `build.sh` uses host `rustc` without a Linux target. It cannot be injected into the Linux hosted canary. The future harness must build or inject a base-owned Linux binary, record its raw SHA-256 before execution, and separately record final-image platform/config digests.
4. There is no trusted wrapper, output copier, safe archive validator, image inspect, timeout/quota setup, or result parser in this commit. `VELNOR_HOSTILE_RESULT` is untrusted diagnostic data as documented; it cannot itself prove H1-H9 or mint a transport contract.

## Verdict

Bounded fixture source is suitable as a diagnostic starting point after the gaps above are repaired. It is **not** a hosted canary approval and does not approve any G1 source, image, transport, or policy gate. The minimum blockers before integration are: base-pinned raw-byte/action injection and independent hashes; final platform-manifest/config contract distinct from the base digest; real artifact impersonation attempt semantics without secret logging; complete network/socket/workspace/command-file probes; archive/quota/timeout/status harness; and actual hosted H1-H9 negative results.
