# GitHub Runner V2 reference

Status: current pinned compatibility reference. The Velnor runner follows the
broker/run-service V2 path in the upstream `actions/runner` source. This file
is the input to `velnor-tools check-runner-reference`; the code and upstream
source remain the behavioral authorities.

## Pinned release

- latest release checked: `v2.337.0`
- tag commit: `397b032cbf865e9c3ddfab89d533ec19325e1273`
- previous release: `v2.335.1`
- release: <https://github.com/actions/runner/releases/tag/v2.337.0>
- source repository: <https://github.com/actions/runner>

The checker compares the pinned release with the upstream latest release and
checks that `crates/velnor-runner/src/protocol.rs` advertises the same runner
version and user-agent. A mismatch requires re-reading the upstream V2
anchors, updating this pin and protocol constants together, and rerunning the
compatibility tests.

## Velnor compatibility rules

- Hosted GitHub targets require V2 flow and `ServerUrlV2`.
- One broker session owns one active GitHub job; one Velnor daemon may own
  multiple internal slots and sessions.
- Session creation and broker polling use bounded retry/backoff for transient
  failures. Session cleanup is best effort on all loop exits.
- Broker status reports `Online` while idle and `Busy` while executing.
- Acquire, renew, complete, result upload, and cancellation preserve the
  upstream request/response semantics; Velnor maps failures into its typed
  runtime outcomes.

Detailed current runner and execution behavior is in
[system behavior](system-now.md), [execution](execution-now.md), and the
[interface reference](interface-reference.md). Open gaps are tracked in
[future direction](future-direction.md).
