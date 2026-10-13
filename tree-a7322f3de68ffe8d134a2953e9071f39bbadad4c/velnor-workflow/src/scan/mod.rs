//! Scan pass: turn a repository directory into a typed `RepositoryShape`.
//!
//! The pass is a fixed pipeline of detectors. Every detector reads only the
//! file walk output, appends evidence to the shape, and never executes
//! project code or invents a command for something it could not prove. The
//! renderer consumes the shape; repository-specific estate profiles are
//! applied by the caller, never here.

mod docker;
mod docs;
pub(crate) mod file_walk;
mod gradle;
mod homebrew;
mod node;
mod opentofu;
pub(crate) mod rust;
mod signals;
mod swift;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::{
    default_workflow_files, identifier_suffix, AnalysisSummary, CacheSpec, GeneratorError,
    ProjectConfig, RepositoryProfile, RunnerMode, Unit, UnitKind,
};

/// Run the detector pipeline over `root` and return what it proved.
///
/// # Errors
/// Returns filesystem errors with the affected path.
pub(crate) fn scan_shape(
    root: &Path,
    runners: RunnerMode,
    default_branch: &str,
) -> Result<RepositoryShape, GeneratorError> {
    let files = file_walk::repository_files(root)?;
    let file_set: BTreeSet<String> = files.iter().cloned().collect();
    let context = ScanContext {
        root,
        files: &files,
        file_set: &file_set,
    };
    let mut shape = RepositoryShape {
        files: Vec::new(),
        units: Vec::new(),
        detected: Vec::new(),
        // Standing boundaries of static inspection. Every scan reports them,
        // whatever the detectors find.
        limitations: vec![
            "Project code, build scripts, task runners, and commands are never executed during analysis.".to_owned(),
            "Release, signing, registry, deployment, branch-protection, and runner-capability contracts remain explicit manual inputs.".to_owned(),
        ],
        default_branch: default_branch.to_owned(),
        runners,
    };
    // Detector order is part of the contract: ids are sorted stably below, so
    // the first detector to claim an id keeps the un-suffixed form.
    file_walk::detect(&context, &mut shape);
    rust::detect(&context, &mut shape)?;
    signals::detect(&context, &mut shape);
    gradle::detect(&context, &mut shape);
    node::detect(&context, &mut shape)?;
    swift::detect(&context, &mut shape);
    opentofu::detect(&context, &mut shape);
    docker::detect(&context, &mut shape);
    homebrew::detect(&context, &mut shape);
    docs::detect(&context, &mut shape);
    shape.finalize();
    shape.files = files;
    Ok(shape)
}

impl RepositoryShape {
    /// Canonical ordering applied once every detector has run.
    fn finalize(&mut self) {
        self.units.sort_by(|left, right| left.id.cmp(&right.id));
        disambiguate_unit_ids(&mut self.units);
        self.detected.sort();
        self.detected.dedup();
        self.limitations.sort();
        self.limitations.dedup();
    }
}

/// Typed output of the scan pass, before any repository-specific policy is
/// applied.
///
/// Every sequence is stored in canonical sorted order (units by id, capability
/// strings and limitations deduplicated and sorted, files sorted by the file
/// walk), so a derived serialization of the shape is stable across runs and
/// machines. The digest phase relies on that canonical ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RepositoryShape {
    /// Every repository file observed by the file walk, normalized and sorted.
    files: Vec<String>,
    /// Verification units derived from manifests and directory layout.
    units: Vec<Unit>,
    /// Detected capabilities, sorted and deduplicated.
    detected: Vec<String>,
    /// What static inspection could not prove, sorted and deduplicated.
    limitations: Vec<String>,
    default_branch: String,
    runners: RunnerMode,
}

/// Read-only view of the walked repository that every detector receives.
pub(crate) struct ScanContext<'a> {
    root: &'a Path,
    files: &'a [String],
    file_set: &'a BTreeSet<String>,
}

/// Build a verification unit with the shared id, label, and command contract.
pub(crate) fn unit(
    kind: UnitKind,
    root: &str,
    watch: Vec<String>,
    commands: Vec<String>,
    cache: Option<CacheSpec>,
) -> Unit {
    let suffix = if root == "." {
        String::new()
    } else {
        format!("-{}", identifier_suffix(root))
    };
    Unit {
        id: format!("{}{}", kind.id_prefix(), suffix),
        label: if root == "." {
            kind.label().to_owned()
        } else {
            format!("{} ({root})", kind.label())
        },
        kind,
        root: root.to_owned(),
        watch,
        pr_commands: commands.clone(),
        full_commands: commands,
        github_pr_commands: None,
        github_full_commands: None,
        velnor_pr_commands: None,
        velnor_full_commands: None,
        depends_on: Vec::new(),
        cache,
        tool_version: None,
    }
}

fn disambiguate_unit_ids(units: &mut [Unit]) {
    let mut counts = BTreeMap::new();
    for unit in units {
        let base_id = unit.id.clone();
        let count = counts.entry(base_id.clone()).or_insert(0_usize);
        *count += 1;
        if *count > 1 {
            unit.id = format!("{base_id}-{count}");
        }
    }
}

impl From<RepositoryShape> for ProjectConfig {
    fn from(shape: RepositoryShape) -> Self {
        Self {
            repository: String::new(),
            profile: RepositoryProfile::Generic,
            analysis: AnalysisSummary {
                method: "static-filesystem-and-manifest-inspection".to_owned(),
                detected: shape.detected,
                limitations: shape.limitations,
            },
            verified: true,
            workflow_files: default_workflow_files(),
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: shape.default_branch,
            runners: shape.runners,
            github_runner: "ubuntu-24.04".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned(), "velnor-target-mvp".to_owned()],
            release_enabled: false,
            release_reason: "Release is fail-closed. Enable only after declaring immutable artifact, registry, provenance, and tag-protection policy.".to_owned(),
            release: None,
            units: shape.units,
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
        }
    }
}
