# Parallax UI candidate review

Verdict: PASS for the bounded eight-file candidate at `e28a88eceb03351da6951447a71be18780dc8bef` (detached, based on current main).

## Evidence

- Diff: 8 files, 356 insertions, 78 deletions; `git diff --check` passed.
- Focused execution: `rtk cargo test --locked -p parallax-xtask --lib release::tests -- --nocapture` -> 10 passed, 88 filtered.
- Embedded build guard cases pass: feature disabled, missing product, empty/stub, valid product, shell/product symlinks, nested external asset symlink, socket, and FIFO/special-file rejection. The guard never writes a placeholder and rejects legacy marker bytes.
- The selection comparison [selection observations](../observations/parallax-ui-048-selection.json) has six representative UI inputs. 048 whitelist treatment: 21 required, 21 full, fallback=true. Candidate root treatment: 16 required, 3 full (`bun-ui`, `rust-parallax-cli`, `rust-parallax-server`), fallback=false. All six candidate cases have the same result.
- Source and generated config retain generator revision `048a7bdaed8240cf652127c94434e60528633dec`, `runners = "github"`, `automatic_lanes = "github"`, `default_dispatch_runner = "github"`, the three explicit scan exclusions, the `embedded-ui` product and CLI/server prerequisites, and release fail-closed/rehearsal assertions.
- `/tmp/velnor-runtime-048a7bda/manifest.json` reports revision 048, closure `32565cc6272a84d449edfb985b3088c6136dbb710fb5ce983a6422245c39b232`, release/no features. The macOS ARM binary hash matches both manifest and binary attestation (`fceed1773dd9b3805645e432afb06e63a72d5ae1f2735a11a01ec44f3c2997f7`); attestations bind source/workflow SHA 048 and producer run 35272213107 on GitHub-hosted.

## Caveat

The product declaration drives selection/prerequisite closure, while generated isolated consumer jobs still execute `mise run build-ui-for-rust` themselves. `bun-ui` also runs its own build job; no cross-job product artifact is transported. Treat this as correctness/coverage PASS, not evidence of one shared UI build or a measured speedup. The broad `ui/**` watch intentionally covers config/assets beyond old extensions and selects the same 3 full/16 required units for all six probes.

Published tree: `72f3275e43ba34d89790d4675b44a3073045665f`, commit
`43d1841550318f684fe1df757255e78b26e38c53`, [PR #118](https://github.com/tailrocks/parallax/pull/118).
Local candidate PASS does not certify full CI: the CLI cache import failed before
checks in run `35521049612`; see the retained checkpoint and compressed log.
