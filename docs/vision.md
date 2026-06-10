# Velnor Vision

Velnor is a GitHub Actions-compatible workflow runner with a Rust runtime.

The long-term product goal is a dependable runner and workflow engine for
CI/CD and general-purpose automation that feels familiar to teams already
using GitHub Actions: workflows, triggers, jobs, steps, matrices,
environments, secrets, artifacts, caches, reusable units, and runner labels
stay recognizable — and Velnor is simply the faster, nicer, cheaper place to
run them.

## Where we are

The original "Phase 0" goal — run real GitHub Actions jobs as a drop-in
self-hosted runner replacement, same YAML, correct logs/outputs/artifacts/
conclusions in the GitHub UI — is **achieved and in production**:

- GitHub still parses workflows, expands matrices and reusable workflows,
  schedules jobs, manages secrets, and renders the Actions UI. Velnor
  replaces only the runner side, over GitHub's current V2 JIT/broker/
  run-service/Results Service protocol, executing every assigned Linux job
  in an isolated Docker container.
- Three production repositories run **dual-lane with Velnor as the default**
  (`github`/`both` selectable per run): `ChainArgos/java-monorepo`,
  `ChainArgos/blockchain-nodes`, `ChainArgos/jackin-agent-brown`.
- The rest of the estate runs GitHub-hosted but is **switch-ready**: Velnor
  must support every feature those repositories use so any of them can move
  at any moment (see master-plan §3a).
- The runner ships as a Debian package from a fully automatic
  tag → CI → deb → apt chain, operates unattended under systemd
  (never-exit supervision, credential diagnosis, watchdog, doctor timers),
  and streams logs live per line to the GitHub UI.

## What drives the next phases

[master-plan.md](master-plan.md) is the execution sequence. The active
direction:

- **Performance ceiling-chasing** (P3): native HTTP client replacing the
  curl transport workaround, zero-copy log pipeline, dynamic slot
  autoscaling, org-level JIT fleets, multi-host scale-out.
- **Native-adapter completeness** (P3b): no JavaScript or remote action
  bundle ever executes as the product path — every `uses:` in the estate has
  a native Rust adapter matching the latest upstream.
- **UX superiority** (P4): never less informative than GitHub, then better
  (cache/speed evidence surfaced in logs).

## Non-Goals For Now

- no Velnor-native workflow language (no Pkl/PQL/KCL work)
- no local YAML scheduler — GitHub stays the scheduler/UI
- no broad GitHub Actions marketplace parity beyond the estate's surface
- no macOS job execution or macOS runner-label support

Typed workflow authoring and new language ideas may be revisited after the
performance and adapter goals are exhausted; until then they are not active
product direction.
