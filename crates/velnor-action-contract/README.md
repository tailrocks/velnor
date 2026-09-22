# velnor-action-contract

Standalone `action.yml` metadata contract parser.

This foundation follows the GitHub Actions runner sources of truth:

- [`action_yaml.json`](https://github.com/actions/runner/blob/main/src/Runner.Worker/action_yaml.json)
- [`ActionManifestManager.cs`](https://github.com/actions/runner/blob/main/src/Runner.Worker/ActionManifestManager.cs)
- [`YamlObjectReader.cs`](https://github.com/actions/runner/blob/main/src/Sdk/WorkflowParser/Conversion/YamlObjectReader.cs)
- [`TemplateReader.cs`](https://github.com/actions/runner/blob/main/src/Sdk/DTObjectTemplating/ObjectTemplating/TemplateReader.cs)

It provides typed runtime fields, strict runtime/output/step mappings,
runner-style YAML 1.2 scalar coercion, and case-insensitive duplicate-key
rejection. Root and input metadata retain the upstream schema's loose-field
behavior; fields the runner ignores are intentionally not copied into the
typed model.

Caller migration is intentionally not part of this PR. `velnor-runner` still
owns its execution-oriented `ActionMetadata` model and parser, while
`velnor-workflow` owns source scanning and Velnor capability policy. A later
migration must make both callers consume this crate's parser, then add their
caller-specific validation at explicit boundaries. Until that migration lands,
this crate is a foundation only and is not used by production callers.
