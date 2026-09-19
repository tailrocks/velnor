# Rules

- `actions/runner` (https://github.com/actions/runner) is the protocol source of truth: before writing runner protocol code (job messages, broker, expressions, credentials, run-service, or timeline), find and match its equivalent logic exactly. Never guess.
- GitHub-hosted macOS jobs must use an explicit label for the highest stable macOS major currently offered by GitHub for the required architecture. As of 2026-09-20, that is `macos-26` for arm64 and `macos-26-intel` for Intel; use the corresponding `macos-27` label when GitHub officially offers macOS 27. Never use `macos-latest`, macOS 15, or any older-major fallback: its alias can lag rollout, and a missing latest-major label for the required architecture is an explicit unsupported/blocker condition, not permission to downgrade or change architecture. Recheck the official [runner image matrix](https://github.com/actions/runner-images#available-images) when changing workflows.
- Prefer the latest stable versions when selecting or updating tools and dependencies. Keep action and image references immutable; advance SHA/digest pins only to verified newer revisions, never replace them with floating references.
- No legacy code. Finish every migration: remove old paths completely—no compatibility shims, aliases, or deprecation periods. Breaking changes are preferred.
- This is a research project. It is unsafe and expected to contain breaking changes; never treat it as production-ready. Break things when needed and deliver new implementations fast.
- Always apply these principles:
  - Judge work by correctness, consistency, and project fit. Never defer a known-wrong state because of ROI, cost, effort, or claims that it is low-value, marginal, or an edge case.
  - Stop only when the required change is proven impossible with the available tools or model. When uncertain, inspect, test, and measure first.
  - Before fixing a bug, identify why the architecture allowed the bug class and whether the same structure permits related bugs.
  - Prefer fixes that remove the enabling condition. Use a symptom-layer patch only when the root fix is infeasible or belongs in a separate change, and name the deferred root cause.
