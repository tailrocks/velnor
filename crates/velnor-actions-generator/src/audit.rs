//! Fleet audit.
//!
//! Regenerates every composite building-block action and every template (and, once
//! the block SHA is bound, every callable workflow) in memory, compares against the
//! committed bytes, materializes all 28 repositories to prove each equals its class
//! template, and enforces the closure, owner-fan-out, aggregation, gate, and routing
//! invariants. Any one-byte hand edit to a generated file — including a neutered
//! composite run-script body — fails the audit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use crate::cache::CacheContract;
use crate::composite;
use crate::model::{FleetManifest, OWNERS, is_sha40};
use crate::package::{self, PackagePolicy};
use crate::render::{
    self, ACTIONS_REPO, CALVER_PLACEHOLDER, CANONICAL_OWNER, FLEET_SHA_PLACEHOLDER,
    OWNER_SHA_PLACEHOLDERS,
};
use crate::{ALL_CLASSES, RepositoryClass};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REMOTE_CLOSURE_SCHEMA: u32 = 1;

#[derive(Debug, Deserialize)]
struct RemoteClosure {
    schema_version: u32,
    action: Vec<RemoteAction>,
}

#[derive(Debug, Deserialize)]
struct RemoteAction {
    root: String,
    sha: String,
    files: Vec<RemoteFile>,
}

#[derive(Debug, Deserialize)]
struct RemoteFile {
    path: String,
    kind: String,
    sha256: String,
}

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
    let caches = CacheContract::load(&root.join("fleet").join("caches.toml"))?;
    let packages = PackagePolicy::load(root)?;
    let remote_closure = load_remote_closure(root)?;

    // Composite building blocks must exist and match their canonical bytes exactly
    // (body included), so a neutered run-script fails the audit.
    for name in composite::COMPOSITE_NAMES {
        check_composite(root, name)?;
    }

    // Every class template: regenerate and compare committed bytes.
    for class in ALL_CLASSES {
        let rendered = render::consumer_template(class);
        let committed = read_committed(&template_path(root, class))?;
        require_equal(&committed, &rendered, &template_path_display(class))?;
        audit_consumer_structure(class, &rendered)?;
    }

    // Materialize all 28 repositories and prove each equals its class template.
    audit_materialization(&manifest)?;

    // If the block SHA is bound, audit the full callable-workflow closure.
    let block_sha_path = root.join("fleet").join("block-sha");
    if block_sha_path.exists() {
        let block_sha = read_block_sha(&block_sha_path)?;
        for class in ALL_CLASSES {
            let contract = manifest.class(class);
            let rendered = render::callable_workflow(contract, &caches, &block_sha);
            let committed = read_committed(&callable_path(root, class))?;
            require_equal(&committed, &rendered, &callable_path_display(class))?;
            audit_callable_structure(class, &rendered, &block_sha, &remote_closure)?;
        }
        let updater = packages.render_updater();
        for (path, expected) in [
            (
                root.join(".github/workflows/package-signer.yml"),
                package::SIGNER_WORKFLOW,
            ),
            (
                root.join(".github/workflows/package-updater.yml"),
                updater.as_str(),
            ),
            (
                root.join("templates/tap/package-update.yml"),
                package::TAP_TEMPLATE,
            ),
            (
                root.join("templates/apt/package-update.yml"),
                package::APT_TEMPLATE,
            ),
        ] {
            let committed = read_committed(&path)?;
            require_equal(&committed, expected, &path.display().to_string())?;
            if path.starts_with(root.join(".github/workflows")) {
                audit_immutable_uses(expected, &path)?;
                audit_admitted_closure(
                    expected,
                    &block_sha,
                    &path.display().to_string(),
                    &remote_closure,
                )?;
            }
        }
    }

    Ok(format!(
        "fleet valid: {} repositories, {} classes, {} templates",
        manifest.repositories().len(),
        manifest.classes().len(),
        ALL_CLASSES.len(),
    ))
}

fn audit_immutable_uses(workflow: &str, path: &Path) -> Result<(), String> {
    for reference in uses_refs(workflow) {
        if !is_sha40(&reference) {
            return Err(format!(
                "{}: non-40-hex or mutable ref {reference:?}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn load_remote_closure(root: &Path) -> Result<RemoteClosure, String> {
    let path = root.join("fleet").join("remote-actions.toml");
    let bytes = std::fs::read_to_string(&path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let closure: RemoteClosure =
        toml::from_str(&bytes).map_err(|error| format!("parsing {}: {error}", path.display()))?;
    if closure.schema_version != REMOTE_CLOSURE_SCHEMA || closure.action.is_empty() {
        return Err("remote action closure has unsupported schema or no actions".to_string());
    }
    let mut roots = BTreeSet::new();
    for action in &closure.action {
        if action.root.split('/').count() != 2 || !is_sha40(&action.sha) || action.files.is_empty()
        {
            return Err(format!(
                "invalid remote action identity {}@{}",
                action.root, action.sha
            ));
        }
        if !roots.insert((&action.root, &action.sha)) {
            return Err(format!(
                "duplicate remote action {}@{}",
                action.root, action.sha
            ));
        }
        let mut paths = BTreeSet::new();
        let mut manifests = 0;
        for file in &action.files {
            validate_relative_path(&file.path)?;
            if !paths.insert(&file.path) {
                return Err(format!(
                    "duplicate remote closure path {}/{}",
                    action.root, file.path
                ));
            }
            match file.kind.as_str() {
                "manifest" => manifests += 1,
                "behavior" => {}
                other => return Err(format!("unknown remote closure kind {other:?}")),
            }
            if file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(format!(
                    "malformed SHA-256 for {}/{}",
                    action.root, file.path
                ));
            }
        }
        if manifests == 0 {
            return Err(format!("{} has no manifest", action.root));
        }
    }
    Ok(closure)
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\n')
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe remote closure path {path:?}"));
    }
    Ok(())
}

/// Fetch every admitted file from its immutable commit, verify SHA-256, parse
/// each action manifest, and prove its exact Node `main`/`pre`/`post` closure.
pub fn verify_remote_closure(root: &Path) -> Result<String, String> {
    let closure = load_remote_closure(root)?;
    let mut handles = Vec::new();
    for action in &closure.action {
        for file in &action.files {
            let action_root = action.root.clone();
            let sha = action.sha.clone();
            let path = file.path.clone();
            let expected = file.sha256.clone();
            handles.push(std::thread::spawn(move || {
                let endpoint = format!("repos/{action_root}/contents/{path}?ref={sha}");
                let child = Command::new("gh")
                    .args([
                        "api",
                        "-H",
                        "Accept: application/vnd.github.raw+json",
                        &endpoint,
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|error| format!("executing gh for {endpoint}: {error}"))?;
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("waiting for gh for {endpoint}: {error}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "fetching {endpoint} failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                let observed = hex::encode(Sha256::digest(&output.stdout));
                if observed != expected {
                    return Err(format!(
                        "remote closure hash mismatch for {action_root}/{path}: expected {expected}, observed {observed}"
                    ));
                }
                Ok(((action_root, path), output.stdout))
            }));
        }
    }
    let mut fetched = BTreeMap::new();
    for handle in handles {
        let (identity, bytes) = handle
            .join()
            .map_err(|_| "remote closure fetch worker panicked".to_string())??;
        if fetched.insert(identity.clone(), bytes).is_some() {
            return Err(format!("duplicate fetched remote file {identity:?}"));
        }
    }
    for action in &closure.action {
        let declared_behaviors = action
            .files
            .iter()
            .filter(|file| file.kind == "behavior")
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        let mut derived_behaviors = BTreeSet::new();
        let mut derived_actions = BTreeSet::new();
        for manifest in action.files.iter().filter(|file| file.kind == "manifest") {
            let raw = fetched
                .get(&(action.root.clone(), manifest.path.clone()))
                .ok_or_else(|| {
                    format!("missing fetched manifest {}/{}", action.root, manifest.path)
                })?;
            derive_manifest_closure(
                &manifest.path,
                raw,
                &mut derived_behaviors,
                &mut derived_actions,
            )?;
        }
        if derived_behaviors != declared_behaviors {
            return Err(format!(
                "remote executable closure mismatch for {}: derived {derived_behaviors:?}, declared {declared_behaviors:?}",
                action.root
            ));
        }
        for (root, sha) in derived_actions {
            let mut segments = root.split('/');
            let repository = format!(
                "{}/{}",
                segments.next().unwrap_or_default(),
                segments.next().unwrap_or_default()
            );
            let subpath = segments.collect::<Vec<_>>().join("/");
            let manifest = if subpath.is_empty() {
                "action.yml".to_string()
            } else {
                format!("{subpath}/action.yml")
            };
            if !closure.action.iter().any(|candidate| {
                candidate.root == repository
                    && candidate.sha == sha
                    && candidate
                        .files
                        .iter()
                        .any(|file| file.kind == "manifest" && file.path == manifest)
            }) {
                return Err(format!(
                    "remote composite closure omits nested action {root}@{sha}"
                ));
            }
        }
    }
    Ok(format!(
        "remote closure valid: {} actions",
        closure.action.len()
    ))
}

fn derive_manifest_closure(
    manifest_path: &str,
    raw: &[u8],
    output: &mut BTreeSet<String>,
    actions: &mut BTreeSet<(String, String)>,
) -> Result<(), String> {
    let text = std::str::from_utf8(raw)
        .map_err(|error| format!("manifest {manifest_path} is not UTF-8: {error}"))?;
    let documents = yaml_rust2::YamlLoader::load_from_str(text)
        .map_err(|error| format!("parsing remote manifest {manifest_path}: {error}"))?;
    let document = documents
        .first()
        .ok_or_else(|| format!("empty remote manifest {manifest_path}"))?;
    let using = document["runs"]["using"]
        .as_str()
        .ok_or_else(|| format!("manifest {manifest_path} has no string runs.using"))?;
    if using == "composite" {
        let steps = document["runs"]["steps"]
            .as_vec()
            .ok_or_else(|| format!("composite manifest {manifest_path} has no runs.steps"))?;
        for step in steps {
            let uses = step["uses"].as_str().ok_or_else(|| {
                format!("composite manifest {manifest_path} has a step without uses")
            })?;
            let (root, sha) = uses
                .split_once('@')
                .ok_or_else(|| format!("nested action {uses:?} is not immutable"))?;
            if root.split('/').count() < 2 || !is_sha40(sha) {
                return Err(format!(
                    "nested action {uses:?} is not an immutable remote action"
                ));
            }
            actions.insert((root.to_string(), sha.to_string()));
        }
        return Ok(());
    }
    if !matches!(using, "node20" | "node24") {
        return Err(format!(
            "manifest {manifest_path} uses unsupported runtime {using:?}; closure must fail closed"
        ));
    }
    let parent = Path::new(manifest_path).parent().unwrap_or(Path::new(""));
    for (field, required) in [("main", true), ("pre", false), ("post", false)] {
        let value = document["runs"][field].as_str();
        if required && value.is_none() {
            return Err(format!("manifest {manifest_path} has no runs.{field}"));
        }
        if let Some(value) = value {
            let joined = normalize_join(parent, Path::new(value))?;
            output.insert(joined);
        }
    }
    Ok(())
}

fn normalize_join(parent: &Path, child: &Path) -> Result<String, String> {
    let mut segments = Vec::new();
    for component in parent.components().chain(child.components()) {
        match component {
            Component::Normal(segment) => segments.push(
                segment
                    .to_str()
                    .ok_or_else(|| "non-UTF-8 remote behavior path".to_string())?,
            ),
            Component::ParentDir => {
                segments
                    .pop()
                    .ok_or_else(|| "remote behavior path escapes action root".to_string())?;
            }
            Component::CurDir => {}
            _ => return Err("absolute remote behavior path".to_string()),
        }
    }
    if segments.is_empty() {
        return Err("empty remote behavior path".to_string());
    }
    Ok(segments.join("/"))
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
    let canonical = composite::canonical(name)
        .ok_or_else(|| format!("no canonical bytes for composite {name:?}"))?;

    // The embedded canonical bytes must themselves be a valid composite: this guards
    // the constant in `composite` against a bad future edit (a non-composite, or a
    // mutable/non-40-hex nested action ref).
    if !canonical.contains("using: composite") {
        return Err(format!(
            "canonical composite {name:?} is not a composite action"
        ));
    }
    for reference in uses_refs(canonical) {
        if !is_sha40(&reference) {
            return Err(format!(
                "canonical composite {name:?} references non-40-hex ref {reference:?}"
            ));
        }
    }

    // The committed action body must match the canonical bytes exactly. This is the
    // full body — env, run script, and exec line — not merely the refs, so tampering
    // (e.g. replacing the gate exec with `echo skipped`) fails the audit.
    let path = composite_path(root, name);
    let committed = read_committed(&path)?;
    require_equal(&committed, canonical, &composite_path_display(name))?;
    Ok(())
}

fn composite_path(root: &Path, name: &str) -> std::path::PathBuf {
    root.join("actions").join(name).join("action.yml")
}

fn composite_path_display(name: &str) -> String {
    format!("actions/{name}/action.yml")
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

fn uses_identities(text: &str) -> Vec<String> {
    let mut identities = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start().trim_start_matches("- ").trim_start();
        let Some(rest) = trimmed.strip_prefix("uses:") else {
            continue;
        };
        let value = rest.split_whitespace().next().unwrap_or_default();
        if !value.starts_with("./") && !value.starts_with('.') {
            identities.push(value.to_string());
        }
    }
    identities
}

fn audit_consumer_structure(class: RepositoryClass, rendered: &str) -> Result<(), String> {
    let what = template_path_display(class);
    let file = render::callable_file_name(class);

    // Exactly three static owner-local reusable calls, one per recognized owner,
    // selected only by exact github.repository_owner. No dynamic `uses`.
    for (owner, release_placeholder) in OWNERS.iter().zip(OWNER_SHA_PLACEHOLDERS) {
        let call = format!(
            "uses: {owner}/{ACTIONS_REPO}/.github/workflows/{file}{release_placeholder} # {CALVER_PLACEHOLDER}"
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
    remote_closure: &RemoteClosure,
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
    audit_admitted_closure(rendered, block_sha, &what, remote_closure)?;

    // A selected Velnor lane always means a real Velnor job. Event-dependent
    // substitution would make the lane selector and its evidence dishonest.
    require_contains(
        rendered,
        "velnor-lane:\n    name: velnor lane",
        &what,
        "Velnor lane job",
    )?;
    require_contains(
        rendered,
        "runs-on: ${{ 'velnor-trusted' }}",
        &what,
        "real Velnor route",
    )?;
    if rendered.contains(
        "(github.event_name == 'pull_request' || github.event_name == 'merge_group') && 'ubuntu-26.04' || 'velnor-trusted'",
    ) {
        return Err(format!(
            "{what}: selected Velnor lane must never substitute GitHub-hosted execution"
        ));
    }

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
    if class == RepositoryClass::Native {
        require_contains(
            rendered,
            "name: native-usage-menu-bar",
            &what,
            "native platform check name",
        )?;
        require_contains(
            rendered,
            "runs-on: macos-26",
            &what,
            "pinned native macOS runner",
        )?;
        require_contains(
            rendered,
            "command: mise run desktop-ci",
            &what,
            "repository-owned native gate",
        )?;
    } else if rendered.contains("mise run desktop-ci") {
        return Err(format!(
            "{what}: non-native class contains the native desktop gate"
        ));
    }
    Ok(())
}

fn audit_admitted_closure(
    rendered: &str,
    block_sha: &str,
    what: &str,
    closure: &RemoteClosure,
) -> Result<(), String> {
    for identity in uses_identities(rendered) {
        let (target, reference) = identity
            .rsplit_once('@')
            .ok_or_else(|| format!("{what}: action identity has no ref {identity:?}"))?;
        let mut segments = target.split('/');
        let owner = segments.next().unwrap_or_default();
        let repository = segments.next().unwrap_or_default();
        let root = format!("{owner}/{repository}");
        if root == format!("{CANONICAL_OWNER}/{ACTIONS_REPO}") {
            if reference != block_sha {
                return Err(format!(
                    "{what}: internal action is not block-bound: {identity}"
                ));
            }
            continue;
        }
        let Some(action) = closure
            .action
            .iter()
            .find(|action| action.root == root && action.sha == reference)
        else {
            return Err(format!("{what}: unadmitted remote action {identity}"));
        };
        let subpath = target.split('/').skip(2).collect::<Vec<_>>().join("/");
        let manifest = if subpath.is_empty() {
            "action.yml".to_string()
        } else {
            format!("{subpath}/action.yml")
        };
        if !action
            .files
            .iter()
            .any(|file| file.kind == "manifest" && file.path == manifest)
        {
            return Err(format!(
                "{what}: action identity {identity} has no bound manifest {manifest}"
            ));
        }
    }
    Ok(())
}

fn audit_materialization(manifest: &FleetManifest) -> Result<(), String> {
    for class in ALL_CLASSES {
        let template = render::consumer_template(class);
        let release_shas = [AUDIT_RELEASE_SHA; 3];
        let class_bytes = render::render_consumer(&template, release_shas, AUDIT_CALVER)?;

        // Three coherent owner references sharing one CalVer. Each SHA is
        // owner-local; the audit fixture intentionally uses the same bytes.
        let want_ref = format!("@{AUDIT_RELEASE_SHA} # {AUDIT_CALVER}");
        if class_bytes.matches(&want_ref).count() != OWNERS.len() {
            return Err(format!(
                "class {} materialization does not bind all {} owner calls to the release",
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
        if render::render_consumer(&class_bytes, release_shas, AUDIT_CALVER).is_ok() {
            return Err(format!(
                "class {} accepted a second repository-specific substitution",
                class.code()
            ));
        }

        // Every member of the class materializes to the identical bytes: no
        // per-repository fork or slug-specific substitution.
        for repo in manifest.members_of(class) {
            let repo_bytes = render::render_consumer(&template, release_shas, AUDIT_CALVER)?;
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

#[cfg(test)]
mod remote_closure_tests {
    use super::*;

    #[test]
    fn node_manifest_derives_exact_main_pre_post_set() {
        let raw = br#"runs:
  using: node24
  pre: ../dist/pre.js
  main: ../dist/main.js
  post: ../dist/post.js
"#;
        let mut output = BTreeSet::new();
        derive_manifest_closure("sub/action.yml", raw, &mut output, &mut BTreeSet::new()).unwrap();
        assert_eq!(
            output,
            ["dist/main.js", "dist/post.js", "dist/pre.js"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn unknown_runtime_missing_main_and_escape_fail_closed() {
        for raw in [
            "runs:\n  using: nodeevil\n  main: dist/main.js\n",
            "runs:\n  using: node24\n",
            "runs:\n  using: node24\n  main: ../../escape.js\n",
        ] {
            assert!(
                derive_manifest_closure(
                    "action.yml",
                    raw.as_bytes(),
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn obsolete_node_runtimes_fail_closed() {
        for runtime in ["node12", "node16"] {
            let raw = format!("runs:\n  using: {runtime}\n  main: dist/main.js\n");
            assert!(
                derive_manifest_closure(
                    "action.yml",
                    raw.as_bytes(),
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new(),
                )
                .is_err()
            );
        }
    }
}
