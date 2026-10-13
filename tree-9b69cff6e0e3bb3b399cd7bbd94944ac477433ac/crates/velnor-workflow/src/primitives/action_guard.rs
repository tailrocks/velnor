//! Static action-reference checks for jobs whose first steps establish trust.
//!
//! External actions are admitted only by the exact reviewed pre-guard
//! allowlist. Local actions are admitted only when their manifest is supplied
//! by the caller and the complete nested composite-action graph passes the
//! same check. Retired Rust-generated Velnor actions fail closed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const MAX_COMPOSITE_DEPTH: usize = 9;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_METADATA_DEPTH: usize = 64;
const MAX_METADATA_NODES: usize = 50_000;
const MAX_COMPOSITE_STEPS: usize = 10_000;
const MAX_ESTIMATED_METADATA_BYTES: usize = 10 * 1024 * 1024;

/// A local manifest failed the guard's static safety contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionGuardError {
    reference: String,
    reason: String,
}

impl ActionGuardError {
    fn new(reference: &str, reason: impl Into<String>) -> Self {
        Self {
            reference: reference.to_owned(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ActionGuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reference, self.reason)
    }
}

impl std::error::Error for ActionGuardError {}

/// Validate action references collected from workflow step `uses` fields.
///
/// `local_manifests` maps each exact local `uses:` alias to the bytes of the
/// action manifest available at that checkout path. Nested local references
/// in composite `runs.steps` must also have an entry. Reusable-workflow job
/// references are not action steps and must not be passed here.
pub(crate) fn validate_action_references(
    references: &[String],
    generator_revision: Option<&str>,
    local_manifests: &BTreeMap<String, String>,
) -> Result<(), ActionGuardError> {
    let mut validator = Validator {
        generator_revision,
        local_manifests,
        active: Vec::new(),
        validated: BTreeSet::new(),
        total_manifest_bytes: 0,
    };
    for reference in references {
        validator.validate_reference(reference, 1)?;
    }
    Ok(())
}

struct Validator<'a> {
    generator_revision: Option<&'a str>,
    local_manifests: &'a BTreeMap<String, String>,
    active: Vec<String>,
    validated: BTreeSet<String>,
    total_manifest_bytes: usize,
}

impl Validator<'_> {
    fn validate_reference(
        &mut self,
        reference: &str,
        depth: usize,
    ) -> Result<(), ActionGuardError> {
        if is_repo_owned_remote_action_reference(reference) {
            return Err(ActionGuardError::new(
                reference,
                "retired Rust-generated Velnor action is unsupported; no current publisher or trusted manifest is available",
            ));
        }
        if is_local_reference(reference) {
            return self.validate_local(reference, depth);
        }
        if crate::is_runner_guard_safe_action_reference(reference, self.generator_revision) {
            return Ok(());
        }
        Err(ActionGuardError::new(
            reference,
            "action ref is not in the reviewed no-pre allowlist",
        ))
    }

    fn validate_local(&mut self, reference: &str, depth: usize) -> Result<(), ActionGuardError> {
        let Some(contents) = self.local_manifests.get(reference) else {
            return Err(ActionGuardError::new(
                reference,
                "local action manifest is missing from the audited checkout map",
            ));
        };
        self.validate_manifest_contents(reference, contents, depth)
    }

    fn validate_manifest_contents(
        &mut self,
        reference: &str,
        contents: &str,
        depth: usize,
    ) -> Result<(), ActionGuardError> {
        if self.active.iter().any(|active| active == reference) {
            let mut cycle = self.active.join(" -> ");
            if !cycle.is_empty() {
                cycle.push_str(" -> ");
            }
            cycle.push_str(reference);
            return Err(ActionGuardError::new(
                reference,
                format!("local composite action cycle: {cycle}"),
            ));
        }
        if self.validated.contains(reference) {
            return Ok(());
        }
        if depth > MAX_COMPOSITE_DEPTH {
            return Err(ActionGuardError::new(
                reference,
                format!("local composite nesting exceeds runner limit {MAX_COMPOSITE_DEPTH}"),
            ));
        }
        if contents.len() > MAX_MANIFEST_BYTES {
            return Err(ActionGuardError::new(
                reference,
                format!("local action manifest exceeds {MAX_MANIFEST_BYTES} bytes"),
            ));
        }
        self.total_manifest_bytes = self
            .total_manifest_bytes
            .checked_add(contents.len())
            .filter(|total| *total <= MAX_TOTAL_MANIFEST_BYTES)
            .ok_or_else(|| {
                ActionGuardError::new(
                    reference,
                    format!(
                        "local action manifests exceed the {} byte audit limit",
                        MAX_TOTAL_MANIFEST_BYTES
                    ),
                )
            })?;

        self.active.push(reference.to_owned());
        let result = self.validate_manifest(reference, contents, depth);
        self.active.pop();
        result?;
        self.validated.insert(reference.to_owned());
        Ok(())
    }

    fn validate_manifest(
        &mut self,
        reference: &str,
        contents: &str,
        depth: usize,
    ) -> Result<(), ActionGuardError> {
        let parser = serde_yaml::ParserConfig::default()
            .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
        let document: serde_yaml::Value = serde_yaml::from_str_with_config(contents, &parser)
            .map_err(|error| {
                ActionGuardError::new(reference, format!("invalid action manifest YAML: {error}"))
            })?;
        let mut nodes = 0usize;
        validate_metadata_tree(&document, 0, &mut nodes)
            .map_err(|reason| ActionGuardError::new(reference, reason))?;
        let _estimated_metadata_bytes = contents
            .len()
            .checked_add(nodes.saturating_mul(64))
            .filter(|size| *size <= MAX_ESTIMATED_METADATA_BYTES)
            .ok_or_else(|| {
                ActionGuardError::new(
                    reference,
                    format!(
                        "action manifest exceeds the {MAX_ESTIMATED_METADATA_BYTES} byte parser budget"
                    ),
                )
            })?;

        let Some(root) = document.as_mapping() else {
            return Err(ActionGuardError::new(
                reference,
                "action manifest root must be a mapping",
            ));
        };
        let runs = field(root, "runs")
            .and_then(serde_yaml::Value::as_mapping)
            .ok_or_else(|| {
                ActionGuardError::new(reference, "action manifest `runs` must be a mapping")
            })?;

        for key in runs.keys().map(String::as_str) {
            if is_preparation_field(key) {
                return Err(ActionGuardError::new(
                    reference,
                    format!("action manifest declares forbidden preparation hook `runs.{key}`"),
                ));
            }
        }

        let using = field(runs, "using")
            .and_then(serde_yaml::Value::as_str)
            .ok_or_else(|| {
                ActionGuardError::new(reference, "action manifest `runs.using` must be a string")
            })?;
        if !using.eq_ignore_ascii_case("composite") {
            return Err(ActionGuardError::new(
                reference,
                format!(
                    "local action runtime `{using}` is not admitted; only composite actions are allowed"
                ),
            ));
        }

        for key in runs.keys().map(String::as_str) {
            if !matches!(key, "using" | "steps") {
                return Err(ActionGuardError::new(
                    reference,
                    format!("unsupported composite action metadata field `runs.{key}`"),
                ));
            }
        }
        let steps = field(runs, "steps")
            .and_then(serde_yaml::Value::as_sequence)
            .ok_or_else(|| {
                ActionGuardError::new(
                    reference,
                    "composite action metadata `runs.steps` must be a sequence",
                )
            })?;
        if steps.len() > MAX_COMPOSITE_STEPS {
            return Err(ActionGuardError::new(
                reference,
                format!("composite action has more than {MAX_COMPOSITE_STEPS} steps"),
            ));
        }

        for (index, step) in steps.iter().enumerate() {
            let step = step.as_mapping().ok_or_else(|| {
                ActionGuardError::new(
                    reference,
                    format!("composite step {index} must be a mapping"),
                )
            })?;
            validate_composite_step(reference, index, step)?;
            if let Some(uses) = field(step, "uses").and_then(serde_yaml::Value::as_str) {
                self.validate_reference(uses, depth + 1)?;
            }
        }
        Ok(())
    }
}

fn validate_composite_step(
    reference: &str,
    index: usize,
    step: &serde_yaml::Mapping,
) -> Result<(), ActionGuardError> {
    for (key, value) in step {
        let key = key.as_str();
        match key {
            "id" | "name" | "if" | "run" | "shell" | "working-directory" | "uses" => {
                let text = value.as_str().ok_or_else(|| {
                    ActionGuardError::new(
                        reference,
                        format!("composite step {index} `{key}` must be a string"),
                    )
                })?;
                if key == "uses" && text.trim().is_empty() {
                    return Err(ActionGuardError::new(
                        reference,
                        format!("composite step {index} `uses` must not be empty"),
                    ));
                }
                if key == "id" && text.is_empty() {
                    return Err(ActionGuardError::new(
                        reference,
                        format!("composite step {index} `id` must not be empty"),
                    ));
                }
            }
            "with" | "env" => {
                let mapping = value.as_mapping().ok_or_else(|| {
                    ActionGuardError::new(
                        reference,
                        format!("composite step {index} `{key}` must be a mapping"),
                    )
                })?;
                for (name, value) in mapping {
                    if value.as_str().is_none() {
                        return Err(ActionGuardError::new(
                            reference,
                            format!("composite step {index} `{key}` entries must be strings"),
                        ));
                    }
                }
            }
            "continue-on-error" => {
                if !value.is_bool() && value.as_str().is_none() {
                    return Err(ActionGuardError::new(
                        reference,
                        format!(
                            "composite step {index} `continue-on-error` must be a boolean or string"
                        ),
                    ));
                }
            }
            _ => {
                return Err(ActionGuardError::new(
                    reference,
                    format!("composite step {index} has unsupported field `{key}`"),
                ));
            }
        }
    }

    let run = field(step, "run").is_some();
    let uses = field(step, "uses").is_some();
    if run == uses {
        return Err(ActionGuardError::new(
            reference,
            format!("composite step {index} must declare exactly one of `run` or `uses`"),
        ));
    }
    if run {
        if field(step, "shell")
            .and_then(serde_yaml::Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(ActionGuardError::new(
                reference,
                format!("composite run step {index} must declare a non-empty `shell`"),
            ));
        }
        if field(step, "with").is_some() {
            return Err(ActionGuardError::new(
                reference,
                format!("composite run step {index} must not declare `with`"),
            ));
        }
    } else if field(step, "shell").is_some() || field(step, "working-directory").is_some() {
        return Err(ActionGuardError::new(
            reference,
            format!("composite uses step {index} must not declare `shell` or `working-directory`"),
        ));
    }
    Ok(())
}

fn field<'a>(mapping: &'a serde_yaml::Mapping, name: &str) -> Option<&'a serde_yaml::Value> {
    mapping.get(name)
}

fn is_local_reference(reference: &str) -> bool {
    let normalized = reference.replace('\\', "/");
    normalized.starts_with("./")
        || normalized.starts_with("../")
        || normalized.starts_with('/')
        || has_windows_drive_prefix(&normalized)
}

/// The two Velnor-owned actions can be emitted as immutable remote refs.
/// Their exact pin still must be reviewed by the shared allowlist, and their
/// manifest must be supplied so that the audit examines the bytes at the
/// matching checked-out revision.
fn is_repo_owned_remote_action_reference(reference: &str) -> bool {
    let Some((path, revision)) = reference.rsplit_once('@') else {
        return false;
    };
    !revision.is_empty()
        && matches!(
            path,
            crate::VELNOR_WORKFLOW_SETUP_ACTION | crate::VELNOR_CI_REPORT_ACTION
        )
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_preparation_field(field: &str) -> bool {
    let normalized = field.to_ascii_lowercase().replace('_', "-");
    matches!(
        normalized.as_str(),
        "pre" | "pre-if" | "preif" | "pre-entrypoint" | "preentrypoint"
    )
}

fn validate_metadata_tree(
    value: &serde_yaml::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_METADATA_DEPTH {
        return Err(format!(
            "action manifest YAML nesting exceeds {MAX_METADATA_DEPTH} levels"
        ));
    }
    *nodes = (*nodes)
        .checked_add(1)
        .ok_or_else(|| "action manifest YAML node count overflow".to_owned())?;
    if *nodes > MAX_METADATA_NODES {
        return Err(format!(
            "action manifest YAML exceeds {MAX_METADATA_NODES} nodes"
        ));
    }
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let mut keys = BTreeSet::new();
            for (key, value) in mapping {
                let key = key.as_str();
                let folded = key.to_ascii_lowercase();
                if !keys.insert(folded) {
                    return Err(format!(
                        "action manifest has duplicate mapping key `{key}` after case folding"
                    ));
                }
                validate_metadata_tree(value, depth + 1, nodes)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for value in sequence {
                validate_metadata_tree(value, depth + 1, nodes)?;
            }
        }
        serde_yaml::Value::Tagged(_) => {
            return Err("action manifest YAML tags are unsupported".to_owned());
        }
        serde_yaml::Value::Null
        | serde_yaml::Value::Bool(_)
        | serde_yaml::Value::Number(_)
        | serde_yaml::Value::String(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_action_references;
    use std::collections::BTreeMap;

    const CHECKOUT: &str = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";

    fn local(manifest: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("./.github/actions/local".to_owned(), manifest.to_owned())])
    }

    fn validate(
        references: &[String],
        manifests: &BTreeMap<String, String>,
    ) -> Result<(), super::ActionGuardError> {
        validate_action_references(references, None, manifests)
    }

    #[test]
    fn accepts_reviewed_external_action_and_rejects_unknown_sha() {
        let safe = vec![CHECKOUT.to_owned()];
        assert!(validate(&safe, &BTreeMap::new()).is_ok());

        let unknown = vec![format!("actions/checkout@{}", "a".repeat(40))];
        let error = validate(&unknown, &BTreeMap::new()).expect_err("unknown SHA must fail");
        assert!(error.to_string().contains("reviewed no-pre allowlist"));

        let manifests = BTreeMap::from([(
            unknown[0].clone(),
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n"
                .to_owned(),
        )]);
        let error = validate(&unknown, &manifests)
            .expect_err("a manifest entry must not bless an unreviewed external ref");
        assert!(error.to_string().contains("reviewed no-pre allowlist"));
    }

    #[test]
    fn retired_repo_owned_remote_actions_fail_closed() {
        for path in [
            crate::VELNOR_WORKFLOW_SETUP_ACTION,
            crate::VELNOR_CI_REPORT_ACTION,
        ] {
            let reference = format!("{path}@{}", "a".repeat(40));
            let candidate_bytes = BTreeMap::from([(
                reference.clone(),
                "runs:\n  using: composite\n  steps: []\n".to_owned(),
            )]);
            let error = validate_action_references(
                std::slice::from_ref(&reference),
                Some(&"b".repeat(40)),
                &candidate_bytes,
            )
            .expect_err("candidate bytes cannot establish retired action provenance");
            assert!(error.to_string().contains("retired Rust-generated Velnor action"));
        }
    }

    #[test]
    fn accepts_composite_with_reviewed_nested_external_action() {
        let references = vec!["./.github/actions/local".to_owned()];
        let manifests = local(
            "name: local\nruns:\n  using: composite\n  steps:\n    - name: checkout\n      uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1\n",
        );
        assert!(validate(&references, &manifests).is_ok());
    }

    #[test]
    fn rejects_missing_local_manifest() {
        let references = vec!["./.github/actions/missing".to_owned()];
        let error = validate(&references, &BTreeMap::new()).expect_err("missing manifest");
        assert!(error.to_string().contains("manifest is missing"));
    }

    #[test]
    fn rejects_docker_and_preparation_hooks() {
        for manifest in [
            "runs:\n  using: docker\n  image: Dockerfile\n",
            "runs:\n  using: composite\n  pre: before.sh\n  steps:\n    - run: echo ok\n      shell: bash\n",
            "runs:\n  using: composite\n  pre-if: always()\n  steps:\n    - run: echo ok\n      shell: bash\n",
            "runs:\n  using: composite\n  pre-entrypoint: before.sh\n  steps:\n    - run: echo ok\n      shell: bash\n",
        ] {
            let references = vec!["./.github/actions/local".to_owned()];
            let error = validate(&references, &local(manifest)).expect_err("unsafe manifest");
            assert!(
                error.to_string().contains("only composite actions are allowed")
                    || error.to_string().contains("forbidden preparation hook"),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_malformed_and_duplicate_key_metadata() {
        for manifest in [
            "runs: [not-a-mapping]\n",
            "runs:\n  using: composite\n  using: docker\n  steps: []\n",
            "runs:\n  using: composite\n  steps:\n    - run: echo hi\n",
        ] {
            let references = vec!["./.github/actions/local".to_owned()];
            assert!(
                validate(&references, &local(manifest)).is_err(),
                "manifest must fail: {manifest}"
            );
        }
    }

    #[test]
    fn rejects_unreviewed_nested_ref_and_local_cycle() {
        let unknown_external = local(
            "runs:\n  using: composite\n  steps:\n    - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        );
        let references = vec!["./.github/actions/local".to_owned()];
        assert!(validate(&references, &unknown_external)
            .expect_err("nested unknown SHA")
            .to_string()
            .contains("reviewed no-pre allowlist"));

        let mut cycle =
            local("runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/child\n");
        cycle.insert(
            "./.github/actions/child".to_owned(),
            "runs:\n  using: composite\n  steps:\n    - uses: ./.github/actions/local\n".to_owned(),
        );
        assert!(validate(&references, &cycle)
            .expect_err("local cycle")
            .to_string()
            .contains("cycle"));
    }
}
