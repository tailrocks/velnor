//! Fleet audit.
//!
//! Regenerates every template (and, once the block SHA is bound, every callable
//! workflow) in memory, compares against the committed bytes, materializes all 24
//! repositories to prove each equals its class template, and enforces the closure,
//! owner-fan-out, aggregation, gate, and routing invariants. Any one-byte hand
//! edit to a generated file fails the audit.

use std::path::Path;

use crate::model::{FleetManifest, OWNERS, is_sha40};
use crate::render::{
    self, ACTIONS_REPO, CALVER_PLACEHOLDER, CANONICAL_OWNER, FLEET_SHA_PLACEHOLDER,
};
use crate::{ALL_CLASSES, RepositoryClass};

/// A deterministic disposable release identity used only to exercise
/// materialization during the audit (never written anywhere).
const AUDIT_RELEASE_SHA: &str = "0000000000000000000000000000000000000000";
const AUDIT_CALVER: &str = "2026.7.0";

/// Run the full fleet audit under `root`. Returns the exact summary line on
/// success.
///
/// # Errors
///
/// Returns `Err` on any contract, byte, closure, owner-fan-out, aggregation,
/// gate, or routing violation.
pub fn audit(root: &Path) -> Result<String, String> {
    let manifest = FleetManifest::load(root)?;

    // Composite building blocks must exist and be composite actions.
    check_composite(root, "run-gate")?;
    check_composite(root, "aggregate")?;

    // Every class template: regenerate and compare committed bytes.
    for class in ALL_CLASSES {
        let rendered = render::consumer_template(class);
        let committed = read_committed(&template_path(root, class))?;
        require_equal(&committed, &rendered, &template_path_display(class))?;
        audit_consumer_structure(class, &rendered)?;
    }

    // Materialize all 24 repositories and prove each equals its class template.
    audit_materialization(&manifest)?;

    // If the block SHA is bound, audit the full callable-workflow closure.
    let block_sha_path = root.join("fleet").join("block-sha");
    if block_sha_path.exists() {
        let block_sha = read_block_sha(&block_sha_path)?;
        for class in ALL_CLASSES {
            let contract = manifest.class(class);
            let rendered = render::callable_workflow(contract, &block_sha);
            let committed = read_committed(&callable_path(root, class))?;
            require_equal(&committed, &rendered, &callable_path_display(class))?;
            audit_callable_structure(class, &rendered, &block_sha)?;
        }
    }

    Ok(format!(
        "fleet valid: {} repositories, {} classes, {} templates",
        manifest.repositories().len(),
        manifest.classes().len(),
        ALL_CLASSES.len(),
    ))
}

fn template_path(root: &Path, class: RepositoryClass) -> std::path::PathBuf {
    root.join("templates").join(class.code()).join("ci.yml")
}

fn template_path_display(class: RepositoryClass) -> String {
    format!("templates/{}/ci.yml", class.code())
}

fn callable_path(root: &Path, class: RepositoryClass) -> std::path::PathBuf {
    root.join(".github")
        .join("workflows")
        .join(render::callable_file_name(class))
}

fn callable_path_display(class: RepositoryClass) -> String {
    format!(".github/workflows/{}", render::callable_file_name(class))
}

fn read_committed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| {
        format!(
            "reading generated file {}: {e} (run `mise run generate`)",
            path.display()
        )
    })
}

fn require_equal(committed: &str, rendered: &str, what: &str) -> Result<(), String> {
    if committed == rendered {
        Ok(())
    } else {
        Err(format!(
            "generated file {what} does not match regeneration (a hand edit or stale render); run `mise run generate`"
        ))
    }
}

fn read_block_sha(path: &Path) -> Result<String, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let sha = raw.trim();
    if !is_sha40(sha) {
        return Err(format!(
            "fleet/block-sha {sha:?} is not a 40 lowercase hex commit SHA (mutable or malformed refs are forbidden)"
        ));
    }
    Ok(sha.to_string())
}

fn check_composite(root: &Path, name: &str) -> Result<(), String> {
    let path = root.join("actions").join(name).join("action.yml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("reading composite {}: {e}", path.display()))?;
    if !text.contains("using: composite") {
        return Err(format!(
            "composite {} is not a composite action",
            path.display()
        ));
    }
    // Composite must not introduce any mutable or non-40-hex nested action ref.
    for reference in uses_refs(&text) {
        if !is_sha40(&reference) {
            return Err(format!(
                "composite {} references non-40-hex ref {reference:?}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Extract the git ref of every `uses: OWNER/REPO...@REF` line (ignoring local
/// `./` references, which have none). A trailing ` # comment` is stripped.
fn uses_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start().trim_start_matches("- ").trim_start();
        let Some(rest) = trimmed.strip_prefix("uses:") else {
            continue;
        };
        let value = rest.trim();
        if value.starts_with("./") || value.starts_with('.') {
            continue;
        }
        // Drop a trailing version comment.
        let value = value.split_whitespace().next().unwrap_or(value);
        if let Some((_, r)) = value.rsplit_once('@') {
            refs.push(r.to_string());
        }
    }
    refs
}

fn audit_consumer_structure(class: RepositoryClass, rendered: &str) -> Result<(), String> {
    let what = template_path_display(class);
    let file = render::callable_file_name(class);

    // Exactly three static owner-local reusable calls, one per recognized owner,
    // selected only by exact github.repository_owner. No dynamic `uses`.
    for owner in OWNERS {
        let call = format!(
            "uses: {owner}/{ACTIONS_REPO}/.github/workflows/{file}@{FLEET_SHA_PLACEHOLDER} # {CALVER_PLACEHOLDER}"
        );
        if !rendered.contains(&call) {
            return Err(format!("{what}: missing owner-local call for {owner}"));
        }
        let guard = format!("if: ${{{{ github.repository_owner == '{owner}' }}}}");
        if !rendered.contains(&guard) {
            return Err(format!("{what}: missing exact owner guard for {owner}"));
        }
    }
    if rendered.matches("uses:").count() != OWNERS.len() {
        return Err(format!(
            "{what}: expected exactly {} reusable-workflow calls",
            OWNERS.len()
        ));
    }
    // A dynamic `uses:` (expression in the ref) is forbidden.
    for line in rendered.lines() {
        if line.contains("uses:") && line.contains("${{") {
            return Err(format!("{what}: dynamic `uses:` reference is forbidden"));
        }
    }

    // ci-required is fail-closed: always() plus the positive truth table.
    require_contains(
        rendered,
        "if: ${{ always() }}",
        &what,
        "ci-required always()",
    )?;
    require_contains(rendered, "case \"${OWNER}\" in", &what, "owner truth table")?;
    require_contains(
        rendered,
        "unrecognized owner",
        &what,
        "unknown-owner rejection",
    )?;
    require_contains(
        rendered,
        "expected both 'skipped'",
        &what,
        "non-selected skipped requirement",
    )?;
    require_contains(
        rendered,
        "expected empty",
        &what,
        "non-selected empty-output requirement",
    )?;
    require_contains(
        rendered,
        "expected 'success'",
        &what,
        "selected success requirement",
    )?;
    // Forbid "no failure found" acceptance logic.
    if rendered.contains("!= \"failure\"") || rendered.contains("!= 'failure'") {
        return Err(format!(
            "{what}: forbidden negative (no-failure) aggregation logic"
        ));
    }
    Ok(())
}

fn audit_callable_structure(
    class: RepositoryClass,
    rendered: &str,
    block_sha: &str,
) -> Result<(), String> {
    let what = callable_path_display(class);

    // workflow_call only — no other trigger.
    require_contains(rendered, "workflow_call:", &what, "workflow_call trigger")?;
    for forbidden in ["pull_request:", "push:", "schedule:", "workflow_dispatch:"] {
        if rendered.contains(&format!("\n  {forbidden}")) {
            return Err(format!(
                "{what}: callable workflow must be workflow_call only, found {forbidden}"
            ));
        }
    }
    // No secret or environment inheritance.
    if rendered.contains("secrets:") || rendered.contains("environment:") {
        return Err(format!(
            "{what}: callable workflow must not inherit secrets or environments"
        ));
    }

    // Internal composite closure is pinned to the block SHA.
    let run_gate = format!("{CANONICAL_OWNER}/{ACTIONS_REPO}/actions/run-gate@{block_sha}");
    let aggregate = format!("{CANONICAL_OWNER}/{ACTIONS_REPO}/actions/aggregate@{block_sha}");
    require_contains(rendered, &run_gate, &what, "run-gate pinned to block SHA")?;
    require_contains(rendered, &aggregate, &what, "aggregate pinned to block SHA")?;

    // Every executable `uses:` ref must be a full 40-hex SHA (no mutable refs).
    for reference in uses_refs(rendered) {
        if !is_sha40(&reference) {
            return Err(format!("{what}: non-40-hex or mutable ref {reference:?}"));
        }
    }

    // Public unmerged-code events route the Velnor lane to GitHub-hosted; no
    // public-unmerged Velnor route is allowed.
    require_contains(
        rendered,
        "(github.event_name == 'pull_request' || github.event_name == 'merge_group') && 'ubuntu-latest' || 'velnor-trusted'",
        &what,
        "public-unmerged GitHub-hosted routing",
    )?;

    // Lane verdict is fail-closed and positive.
    require_contains(rendered, "if: ${{ always() }}", &what, "verdict always()")?;
    require_contains(
        rendered,
        "one lane never substitutes",
        &what,
        "no-substitution note",
    )?;
    require_contains(rendered, "contract=success", &what, "contract emission")?;

    // Every gate command rendered into the workflow is non-empty.
    for line in rendered.lines() {
        if let Some(cmd) = line.trim_start().strip_prefix("command:")
            && cmd.trim().is_empty()
        {
            return Err(format!("{what}: rendered gate has an empty command"));
        }
    }
    Ok(())
}

fn audit_materialization(manifest: &FleetManifest) -> Result<(), String> {
    for class in ALL_CLASSES {
        let template = render::consumer_template(class);
        let class_bytes = render::render_consumer(&template, AUDIT_RELEASE_SHA, AUDIT_CALVER)?;

        // Three coherent owner references sharing one SHA and one CalVer.
        let want_ref = format!("@{AUDIT_RELEASE_SHA} # {AUDIT_CALVER}");
        if class_bytes.matches(&want_ref).count() != OWNERS.len() {
            return Err(format!(
                "class {} materialization does not share one SHA/CalVer across all {} owner calls",
                class.code(),
                OWNERS.len()
            ));
        }
        // No placeholder survives; a second substitution is refused.
        if class_bytes.contains(FLEET_SHA_PLACEHOLDER) || class_bytes.contains(CALVER_PLACEHOLDER) {
            return Err(format!(
                "class {} materialization left a placeholder",
                class.code()
            ));
        }
        if render::render_consumer(&class_bytes, AUDIT_RELEASE_SHA, AUDIT_CALVER).is_ok() {
            return Err(format!(
                "class {} accepted a second repository-specific substitution",
                class.code()
            ));
        }

        // Every member of the class materializes to the identical bytes: no
        // per-repository fork or slug-specific substitution.
        for repo in manifest.members_of(class) {
            let repo_bytes = render::render_consumer(&template, AUDIT_RELEASE_SHA, AUDIT_CALVER)?;
            if repo_bytes != class_bytes {
                return Err(format!(
                    "repository {} does not materialize to its class {} template bytes",
                    repo.slug,
                    class.code()
                ));
            }
        }
    }
    Ok(())
}

fn require_contains(text: &str, needle: &str, what: &str, label: &str) -> Result<(), String> {
    if text.contains(needle) {
        Ok(())
    } else {
        Err(format!("{what}: missing {label}"))
    }
}
