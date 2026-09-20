# V-WATCH-001 independent review

Verdict: **HOLD for conservative-grammar acceptance**. The S2 draft fixes the
original scanner-loss problem and has useful fail-closed paths, but several
Docker grammar cases still can omit a repository input.

## What is sound

The draft preserves scanner watches for nested packages, adds declared named
context roots, keeps `.dockerignore`, follows Docker unit dependency closure,
and excludes the mutable `velnor-cache-seed` from source relevance. Dynamic
COPY/ADD sources, unknown bind mounts, missing source paths, unknown contexts,
and unsupported parser directives generally select `**`. Local stages and
ONBUILD COPY/RUN are traversed. Config validation rejects context traversal and
symlink escapes.

## Required fixes before acceptance

1. `parse_supported_mount` accepts `source`/`src` for every mount type, then
   classifies every `secret` mount as `NonSource`. BuildKit's Dockerfile secret
   grammar uses `id`, `target`, `env`, `required`, `mode`, `uid`, and `gid`;
   host-file `src` belongs to the *CLI* `docker build --secret` option, not
   `RUN --mount=type=secret`. The current code must reject the unsupported
   Dockerfile fields into the conservative fallback instead of silently
   accepting them. Separately, if generated commands can declare a CLI secret
   with `src=repository-file`, that source needs a typed input or a full-context
   fallback; the Dockerfile-only parser cannot see it. The current Velnor
   command uses an environment-backed token, so this is a boundary test rather
   than a claim about that command. See the Dockerfile reference and build
   secrets reference in the experiment record. `cache`, `tmpfs`, and `ssh` need
   their separate non-source proof.
2. Unknown instructions are silently ignored in `inspect_dockerfile` (`_ =>
   {}`). The document says future source-consuming forms select the complete
   context, but a custom/frontend instruction can consume repository files.
   Whitelist the Docker instructions whose source behavior is known and mark
   every other instruction conservative. Also restrict accepted `# syntax`
   frontend versions to the grammar actually tested; `starts_with` accepts
   arbitrary future `docker/dockerfile:*` and `*-upstream:*` values.
3. Quoted COPY/ADD paths are not parsed. `split_whitespace` can turn
   `COPY "app dir/file" /dst` into two apparently static sources. If the
   fragments happen to exist, the code can narrow the watch and miss changes
   to the real path. JSON-array COPY has the same boundary. Reject any quoted,
   escaped, or JSON-array COPY/ADD form into the complete-context fallback until
   a real Docker token parser exists. Add a fixture with fragments that exist.
4. A `COPY --from=stage`/`ADD --from=stage` with no source and destination is
   accepted as a stage reference without marking the grammar incomplete. Check
   operand count even on the `--from` branch; malformed instructions must take
   the conservative path.
5. A root Docker unit with no discovered root Dockerfile leaves
   `conservative_context` false and can lose `unit.watch` because root units are
   treated as derived. The same boundary occurs if a custom root command points
   at a non-root Dockerfile. Require a discovered/declared Dockerfile and
   command/context match, preserve explicit scanner/config watches, or select
   `**` when the root image closure cannot be proved. Add an explicit root-unit
   fixture with no Dockerfile and one with `--file docker/Otherfile`.

The generated repository currently uses a root `Dockerfile`, so these are
generic scanner correctness gates rather than claims about an observed Velnor
failure. A conservative fallback is acceptable; silent under-selection is not.

## Tests needed

Run the S2 suite with fixtures for secret `src`, cache/ssh/tmpfs, unknown
instruction/frontend, quoted and JSON COPY, malformed `--from`, missing root
Dockerfile, custom nested Dockerfile, named contexts, local stages, ONBUILD,
`.dockerignore`, dynamic variables, and dependency closure. For each dynamic or
unsupported form assert `**`; for each supported static form assert every
source path and assert an unrelated path does not select the image. Regenerate
the consumer after the source tests and compare selection for Dockerfile,
context, peer-unit, and unrelated edits.

Primary grammar references: [Dockerfile `RUN --mount` reference](https://docs.docker.com/reference/dockerfile/#run---mounttypesecret)
and [Build secrets CLI reference](https://docs.docker.com/build/building/secrets/).
