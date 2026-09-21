# velnor-workflow rules

- velnor-workflow is a GENERIC workflow generator. It must never contain
  knowledge specific to any particular repository — no `velnor`, `velnor-apt`,
  `velnor-actions-fixture`, holla, jackin, or any other estate name, path,
  pin, grant, or special case baked into the crate. A repository-specific
  template, estate profile entry, or hardcoded consumer list in this crate is
  a design violation; remove it.
- Generation is driven by SCANNING the target repository. Discover what the
  repository provides (Rust crates, scripts, publish surfaces, existing
  workflow structure) and generate workflows from those findings: e.g. every
  discovered Rust crate gets its own build/test/clippy jobs, grouped under
  that crate in the CI/CD structure. The generator reasons about shapes
  (workspace, crate, manifest, lockfile), never about names.
- Repository-specific definitions (release lanes, publish policies, consumer
  catalogs, channel grants) live in the target repository's generation config
  outside `.github`, consumed by the generic engine. The crate supplies the
  engine and generic building blocks only.
- Before changing generation behavior, scan-first: run the generator's scan
  on the target repository, confirm the discovery output matches the real
  repository structure, and only then generate. Never hand-edit generated
  files under `.github`; change the generator or the repo's generation config
  and regenerate.
- Plan change-aware MINIMAL work from the full event change set plus
  discovered/declared deps: classify every changed path as OWNED (a unit
  watches or reads it), UNKNOWN (impact undecidable), or IRRELEVANT
  (no watcher, no consumer). Select-before-setup; schedule no workload
  for proven-irrelevant changes; unknown keeps the smallest sound scope
  with an explicit reason. Explain selection/fallback/cache/reuse with
  the plan; never silent skipping, silent full, blanket exclusions, or
  repo-specific exceptions. Cover every planning change with tests plus
  updated product docs.
- Keep output byte-stable for identical (repository structure + generation
  config): regeneration must be reproducible, checkable, and fail closed when
  generated files drift.
