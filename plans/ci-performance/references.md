# Primary-source checks

Checked 2026-09-20. Documentation describes capabilities; installed versions
and observed run behavior still require separate validation.

## GitHub cache service

[Dependency caching reference](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching)
documents immutable entries, branch/base visibility, and merge-ref isolation
for pull-request caches. Cache version also depends on paths and compression.
The current service exposes `cache-mode` and `ACTIONS_CACHE_MODE`; reusable
workflow requests cannot exceed an explicit caller limit. Low-trust events in
default-branch context default to read access. Pull-request merge-ref caching
has separate scope. Record actual effective access rather than inferring it
from `contents` permissions or a cache-hit flag.

Seven-day inactivity eviction and a default 10 GB repository limit remain
documented, but administrators can configure larger billed limits. Actual
repository limits and budgets are unknown here. Do not label 10 GB an absolute
platform limit or raise limits under this campaign without explicit authority.

## Cargo

[Cargo rebuilding diagnostics](https://doc.rust-lang.org/cargo/faq.html#why-is-cargo-rebuilding-my-code)
supports `CARGO_LOG=cargo::core::compiler::fingerprint=info` on the unexpected
rebuild itself. Listed causes include nonexistent `rerun-if-changed` inputs,
feature-set differences, filesystem timestamps and concurrent build mutation.
Investigate the generator's absent-file sentinel using this diagnostic;
compiler status lines alone do not establish cache misses.

## BuildKit

[Docker's GitHub Actions cache guidance](https://docs.docker.com/build/ci/github-actions/cache/)
distinguishes layer export from cache mounts, which are not preserved by the
GHA backend by default. A separate extraction/injection mechanism is necessary
for mutable mounts on ephemeral builders. Read-only workflows should import
without exporting. The documented local exporter cleanup flag requires
Buildx 0.35.0 or newer; verify installed versions before adopting it.

These checks are research inputs, not optimization iterations or measurements.
