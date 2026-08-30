//! Workspace dependency-boundary tests.
//!
//! Asserted from `cargo metadata` so the law holds no matter how manifests
//! evolve: `velnor-client` meets the daemon only through versioned model DTOs,
//! `velnor-control` consumes the journal and shared model directly, the action
//! journal stays limited to foundational model crates, the cache service stays
//! bounded by the journal, action model, and CAS, the workspace graph is
//! acyclic, and no shared crate depends on Clap or Axum. The Axum transport
//! adapter is owned by the CLI composition crate; it never enters the model,
//! service, client, or renderer crates.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const WORKSPACE_PACKAGES: [&str; 12] = [
    "velnor-model",
    "velnor-action-model",
    "velnor-cas",
    "velnor-action-journal",
    "velnor-cache-service",
    "velnor-control",
    "velnor-client",
    "velnor-render",
    "velnorctl",
    "velnor-runner",
    "velnor-tools",
    "unit-collector",
];

const VELNOR_CONTROL_DIRECT_WORKSPACE_DEPS: [&str; 2] = ["velnor-action-journal", "velnor-model"];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root exists")
}

fn cargo_metadata() -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("metadata JSON parses")
}

/// Full resolved graph (`resolve` is only present without `--no-deps`).
fn cargo_metadata_resolved() -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("metadata JSON parses")
}

/// name -> direct dependency names, for workspace packages only.
fn dependency_graph(metadata: &Value) -> BTreeMap<String, Vec<String>> {
    let mut graph = BTreeMap::new();
    for package in metadata["packages"].as_array().expect("packages array") {
        let name = package["name"].as_str().expect("package name").to_owned();
        let deps = package["dependencies"]
            .as_array()
            .expect("dependencies array")
            .iter()
            .map(|dep| dep["name"].as_str().expect("dependency name").to_owned())
            .collect::<Vec<_>>();
        graph.insert(name, deps);
    }
    graph
}

fn workspace_package<'a>(metadata: &'a Value, name: &str) -> &'a Value {
    let expected_manifest = workspace_root()
        .join("crates")
        .join(name)
        .join("Cargo.toml")
        .canonicalize()
        .expect("workspace package manifest exists");
    let expected_manifest = expected_manifest
        .to_str()
        .expect("workspace package manifest path is UTF-8");

    metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|package| {
            package["name"].as_str() == Some(name)
                && package["source"].is_null()
                && package["manifest_path"].as_str() == Some(expected_manifest)
        })
        .unwrap_or_else(|| panic!("workspace package {name} has unexpected identity"))
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeclaredDependencyEdge {
    name: String,
    source: Option<String>,
    path: Option<String>,
    rename: Option<String>,
    kind: Option<String>,
    target: Option<String>,
    registry: Option<String>,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ResolvedDependencyEdge {
    name: String,
    package_id: String,
    package_source: Option<String>,
    package_manifest_path: String,
    kind: Option<String>,
    target: Option<String>,
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(str::to_owned)
}

fn package_directory(package: &Value) -> String {
    let manifest_path = package["manifest_path"]
        .as_str()
        .expect("workspace package manifest path");
    PathBuf::from(manifest_path)
        .parent()
        .expect("workspace package manifest has parent")
        .to_str()
        .expect("workspace package path is UTF-8")
        .to_owned()
}

fn package_library_name(package: &Value) -> String {
    package["targets"]
        .as_array()
        .expect("package targets array")
        .iter()
        .find(|target| {
            target["kind"]
                .as_array()
                .expect("target kinds array")
                .iter()
                .any(|kind| kind.as_str() == Some("lib"))
        })
        .and_then(|target| target["name"].as_str())
        .expect("workspace package library target name")
        .to_owned()
}

fn resolved_package<'a>(metadata: &'a Value, id: &str) -> &'a Value {
    metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|package| package["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("resolved package {id} exists"))
}

fn declared_dependency_edge(dependency: &Value) -> DeclaredDependencyEdge {
    DeclaredDependencyEdge {
        name: dependency["name"]
            .as_str()
            .expect("dependency name")
            .to_owned(),
        source: optional_string(dependency, "source"),
        path: optional_string(dependency, "path"),
        rename: optional_string(dependency, "rename"),
        kind: optional_string(dependency, "kind"),
        target: optional_string(dependency, "target"),
        registry: optional_string(dependency, "registry"),
    }
}

fn expected_declared_dependency_edge(metadata: &Value, name: &str) -> DeclaredDependencyEdge {
    let package = workspace_package(metadata, name);
    DeclaredDependencyEdge {
        name: name.to_owned(),
        source: None,
        path: Some(package_directory(package)),
        rename: None,
        kind: None,
        target: None,
        registry: None,
    }
}

fn resolved_dependency_edges(
    resolved: &Value,
    resolved_control: &Value,
    expected_library_names: &BTreeSet<String>,
) -> Vec<ResolvedDependencyEdge> {
    let local_package_ids = resolved["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .filter(|package| package["source"].is_null())
        .map(|package| package["id"].as_str().expect("package id").to_owned())
        .collect::<BTreeSet<_>>();

    let mut edges = Vec::new();
    for dependency in resolved_control["deps"]
        .as_array()
        .expect("resolved control dependencies array")
    {
        let package_id = dependency["pkg"].as_str().expect("resolved dependency pkg");
        let dependency_name = dependency["name"]
            .as_str()
            .expect("resolved dependency name");
        if !local_package_ids.contains(package_id)
            && !expected_library_names.contains(dependency_name)
        {
            continue;
        }
        let package = resolved_package(resolved, package_id);
        for kind in dependency["dep_kinds"]
            .as_array()
            .expect("resolved dependency kinds array")
        {
            edges.push(ResolvedDependencyEdge {
                name: dependency_name.to_owned(),
                package_id: package_id.to_owned(),
                package_source: optional_string(package, "source"),
                package_manifest_path: package["manifest_path"]
                    .as_str()
                    .expect("resolved package manifest path")
                    .to_owned(),
                kind: optional_string(kind, "kind"),
                target: optional_string(kind, "target"),
            });
        }
    }
    edges
}

fn assert_velnor_control_direct_dependencies(metadata: &Value, resolved: &Value) {
    let control = workspace_package(metadata, "velnor-control");
    let control_id = control["id"].as_str().expect("control package id");
    let control_dependencies = control["dependencies"]
        .as_array()
        .expect("control dependencies array");

    let mut expected_declared_edges = VELNOR_CONTROL_DIRECT_WORKSPACE_DEPS
        .into_iter()
        .map(|name| expected_declared_dependency_edge(metadata, name))
        .collect::<Vec<_>>();
    let mut actual_declared_edges = control_dependencies
        .iter()
        .filter(|dependency| {
            dependency["name"]
                .as_str()
                .is_some_and(|name| WORKSPACE_PACKAGES.contains(&name))
                || dependency["path"].is_string()
                || dependency["rename"].is_string()
        })
        .map(declared_dependency_edge)
        .collect::<Vec<_>>();
    expected_declared_edges.sort_unstable();
    actual_declared_edges.sort_unstable();
    assert_eq!(
        actual_declared_edges, expected_declared_edges,
        "velnor-control direct runtime dependency declarations must match exactly"
    );

    let mut expected_resolved_edges = VELNOR_CONTROL_DIRECT_WORKSPACE_DEPS
        .into_iter()
        .map(|name| {
            let package = workspace_package(resolved, name);
            let package_id = package["id"].as_str().expect("workspace package id");
            ResolvedDependencyEdge {
                name: package_library_name(package),
                package_id: package_id.to_owned(),
                package_source: optional_string(package, "source"),
                package_manifest_path: package["manifest_path"]
                    .as_str()
                    .expect("workspace package manifest path")
                    .to_owned(),
                kind: None,
                target: None,
            }
        })
        .collect::<Vec<_>>();

    let resolved_control = resolved["resolve"]["nodes"]
        .as_array()
        .expect("resolved nodes array")
        .iter()
        .find(|node| node["id"].as_str() == Some(control_id))
        .expect("resolved control package node");
    let expected_library_names = VELNOR_CONTROL_DIRECT_WORKSPACE_DEPS
        .into_iter()
        .map(|name| package_library_name(workspace_package(resolved, name)))
        .collect::<BTreeSet<_>>();
    let mut actual_resolved_edges =
        resolved_dependency_edges(resolved, resolved_control, &expected_library_names);
    expected_resolved_edges.sort_unstable();
    actual_resolved_edges.sort_unstable();
    assert_eq!(
        actual_resolved_edges, expected_resolved_edges,
        "velnor-control direct runtime resolution edges must match exactly"
    );
}

/// Everything transitively reachable from `root` through the resolved graph.
fn transitive_closure(metadata: &Value, root: &str) -> BTreeSet<String> {
    let mut id_to_name = BTreeMap::new();
    for package in metadata["packages"].as_array().expect("packages array") {
        id_to_name.insert(
            package["id"].as_str().expect("package id").to_owned(),
            package["name"].as_str().expect("package name").to_owned(),
        );
    }
    let mut edges = BTreeMap::new();
    for node in metadata["resolve"]["nodes"].as_array().expect("nodes") {
        let Some(name) = id_to_name.get(node["id"].as_str().expect("node id")) else {
            continue;
        };
        let deps = node["deps"]
            .as_array()
            .expect("node deps")
            .iter()
            .filter_map(|dep| id_to_name.get(dep["pkg"].as_str().expect("dep pkg")))
            .cloned()
            .collect::<Vec<_>>();
        edges.insert(name.clone(), deps);
    }
    let mut seen = BTreeSet::new();
    let mut queue = vec![root.to_owned()];
    while let Some(current) = queue.pop() {
        for dep in edges.get(&current).into_iter().flatten() {
            if seen.insert(dep.clone()) {
                queue.push(dep.clone());
            }
        }
    }
    seen.remove(root);
    seen
}

#[test]
fn workspace_has_exactly_the_twelve_expected_packages() {
    let metadata = cargo_metadata();
    let mut names: Vec<String> = metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .map(|p| p["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    let mut expected = WORKSPACE_PACKAGES.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(names, expected, "workspace package set is fixed");
}

#[test]
fn velnor_client_depends_only_on_velnor_model() {
    let metadata = cargo_metadata();
    let graph = dependency_graph(&metadata);
    let client = &graph["velnor-client"];
    assert!(
        client.iter().any(|d| d == "velnor-model"),
        "client depends on the shared model"
    );
    for forbidden in [
        "velnor-action-journal",
        "velnor-cache-service",
        "velnor-control",
        "velnor-runner",
        "axum",
        "clap",
    ] {
        assert!(
            !client.iter().any(|d| d == forbidden),
            "velnor-client must never depend on {forbidden}"
        );
    }
    let member_names = WORKSPACE_PACKAGES.to_vec();
    for dep in client {
        if member_names.contains(&dep.as_str()) {
            assert_eq!(
                dep, "velnor-model",
                "velnor-client meets no other workspace crate than the model"
            );
        }
    }
}

#[test]
fn velnor_client_transitively_never_reaches_daemon_internals() {
    let metadata = cargo_metadata_resolved();
    let closure = transitive_closure(&metadata, "velnor-client");
    for forbidden in [
        "velnor-action-model",
        "velnor-cas",
        "velnor-action-journal",
        "velnor-cache-service",
        "velnor-control",
        "velnor-runner",
        "velnorctl",
        "axum",
        "clap",
    ] {
        assert!(
            !closure.contains(forbidden),
            "velnor-client must never reach {forbidden}; closure was {closure:?}"
        );
    }
}

#[test]
fn crate_dependency_direction_matches_approved_graph() {
    let metadata = cargo_metadata();
    let graph = dependency_graph(&metadata);
    let members_only = |deps: &[String]| -> Vec<String> {
        deps.iter()
            .filter(|d| WORKSPACE_PACKAGES.contains(&d.as_str()))
            .cloned()
            .collect()
    };

    assert!(
        members_only(&graph["velnor-model"]).is_empty(),
        "model is the root"
    );
    let control_deps = members_only(&graph["velnor-control"])
        .into_iter()
        .collect::<BTreeSet<_>>();
    let control_allowed = VELNOR_CONTROL_DIRECT_WORKSPACE_DEPS
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        control_deps, control_allowed,
        "velnor-control direct workspace dependencies are explicitly allowlisted"
    );
    assert_eq!(
        members_only(&graph["velnor-action-journal"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["velnor-action-model".to_owned(), "velnor-model".to_owned(),]),
        "action journal is constrained to foundational model crates"
    );
    assert_eq!(
        members_only(&graph["velnor-cache-service"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "velnor-action-journal".to_owned(),
            "velnor-action-model".to_owned(),
            "velnor-cas".to_owned(),
        ]),
        "cache service is constrained to foundational storage crates"
    );
    assert_velnor_control_direct_dependencies(&metadata, &cargo_metadata_resolved());
    assert_eq!(members_only(&graph["velnor-render"]), vec!["velnor-model"]);
    let ctl = members_only(&graph["velnorctl"]);
    for required in [
        "velnor-model",
        "velnor-control",
        "velnor-client",
        "velnor-render",
    ] {
        assert!(
            ctl.contains(&required.to_owned()),
            "velnorctl -> {required}"
        );
    }
    for legacy in ["velnor-tools"] {
        for new_crate in [
            "velnor-model",
            "velnor-control",
            "velnor-client",
            "velnor-render",
        ] {
            assert!(
                !graph[legacy].iter().any(|d| d == new_crate),
                "legacy crate {legacy} must not depend on new crate {new_crate}"
            );
        }
    }
    // Transitional Plan 066 amendment: the legacy runner feeds the durable
    // operational store directly at its lifecycle boundaries until Plan 079
    // deletes the crate; after the cutover this allowance dies with it.
    assert!(
        graph["velnor-runner"].iter().any(|d| d == "velnor-model"),
        "runner reads shared model types"
    );
    assert!(
        graph["velnor-runner"].iter().any(|d| d == "velnor-control"),
        "runner persists sanitized lifecycle state through the store"
    );
}

#[test]
fn shared_crates_never_depend_on_clap_or_axum() {
    let graph = dependency_graph(&cargo_metadata());
    for shared in [
        "velnor-model",
        "velnor-action-model",
        "velnor-cas",
        "velnor-action-journal",
        "velnor-cache-service",
        "velnor-control",
        "velnor-client",
        "velnor-render",
    ] {
        for forbidden in ["clap", "axum"] {
            assert!(
                !graph[shared].iter().any(|d| d == forbidden),
                "{shared} must not depend on {forbidden}"
            );
        }
    }
}

#[test]
fn workspace_dependency_graph_is_acyclic() {
    let graph = dependency_graph(&cargo_metadata());
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    let mut marks = BTreeMap::new();

    fn visit(
        node: &str,
        graph: &BTreeMap<String, Vec<String>>,
        marks: &mut BTreeMap<String, Mark>,
    ) {
        match marks.get(node) {
            Some(Mark::Done) => return,
            Some(Mark::Visiting) => panic!("dependency cycle reached at {node}"),
            None => {}
        }
        marks.insert(node.to_owned(), Mark::Visiting);
        if let Some(deps) = graph.get(node) {
            for dep in deps {
                if graph.contains_key(dep) {
                    visit(dep, graph, marks);
                }
            }
        }
        marks.insert(node.to_owned(), Mark::Done);
    }

    for name in WORKSPACE_PACKAGES {
        visit(name, &graph, &mut marks);
    }
}
