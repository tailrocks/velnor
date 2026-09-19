# G0 distribution ownership

This evidence directory is owned by `g0_distribution` for the bounded APT
discovery and schema-2 runtime handoff.

- APT source worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-apt`
- APT source branch: `codex/github-first-apt-discovery`
- APT source checkpoint: `f679a1ce94627281a99e4b887fe26fc1cad33409`
- Schema-2 worktree: `/private/tmp/dual-lane-apt-schema2`
- Schema-2 branch: `dual-lane-apt-schema2`
- Schema-2 checkpoint: `4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4`
- Schema-2 follow-up: [4c4b7a7](https://github.com/tailrocks/velnor/commit/4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4)
- Schema-2 WIP parent: `c63c1c5d89888b9c93f53d9d11f1f8003804db9b`

The schema-2 checkpoint is mixed WIP: `c63c1c5` includes a concurrent
bootstrap/policy delta in `s2/mod.rs` alongside the APT runtime work. Preserve
that history; isolate ownership before integration. The schema-2 tree is
currently frozen pending the bounded compile repair. No generated consumer
workflow, package publication, dispatch, or Mac runtime operation is covered
by this checkpoint.
