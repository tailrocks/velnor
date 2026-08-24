//! Dependency-boundary assertions over the resolved workspace graph
//! (`cargo metadata` JSON), per Plan 064 step 1 verification.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

const WORKSPACE_MEMBERS: [&str; 7] = [
    "velnor-model",
    "velnor-control",
    "velnor-client",
    "velnor-render",
    "velnorctl",
    "velnor-runner",
    "velnor-tools",
];

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn metadata_json() -> serde_json::Value {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            &workspace_root().join("Cargo.toml").to_string_lossy(),
        ])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata must be runnable");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata JSON must parse")
}

/// name -> set of dependency package names across every dependency kind,
/// resolved through the full graph so external transitives are covered.
fn dependency_graph(metadata: &serde_json::Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut id_to_name = BTreeMap::new();
    for package in metadata["packages"].as_array().expect("packages") {
        id_to_name.insert(
            package["id"].as_str().expect("package id").to_string(),
            package["name"].as_str().expect("package name").to_string(),
        );
    }

    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes");
    let mut graph = BTreeMap::new();
    for node in nodes {
        let id = node["id"].as_str().expect("node id");
        let name = id_to_name.get(id).cloned().unwrap_or_default();
        let mut dep_names = BTreeSet::new();
        for dep in node["deps"].as_array().expect("node deps") {
            let dep_id = dep["pkg"].as_str().expect("dep pkg id");
            if let Some(dep_name) = id_to_name.get(dep_id) {
                dep_names.insert(dep_name.clone());
            }
        }
        graph.insert(name, dep_names);
    }
    graph
}

fn transitive_closure(graph: &BTreeMap<String, BTreeSet<String>>, root: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue = vec![root.to_string()];
    while let Some(current) = queue.pop() {
        for dep in graph.get(&current).into_iter().flatten() {
            if seen.insert(dep.clone()) {
                queue.push(dep.clone());
            }
        }
    }
    seen.remove(root);
    seen
}

#[test]
fn all_seven_workspace_packages_exist() {
    let metadata = metadata_json();
    let names: BTreeSet<String> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    for member in WORKSPACE_MEMBERS {
        assert!(names.contains(member), "workspace package {member} missing");
    }
}

#[test]
fn workspace_internal_dependency_graph_is_acyclic() {
    let metadata = metadata_json();
    let graph = dependency_graph(&metadata);

    // DFS cycle detection over workspace-internal edges only.
    fn visit(
        node: &str,
        graph: &BTreeMap<String, BTreeSet<String>>,
        stack: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if stack.iter().any(|n| n == node) {
            let mut cycle = stack.clone();
            cycle.push(node.to_string());
            return Some(cycle);
        }
        if !done.insert(node.to_string()) {
            return None;
        }
        stack.push(node.to_string());
        for dep in graph.get(node).into_iter().flatten() {
            if WORKSPACE_MEMBERS.contains(&dep.as_str()) {
                if let Some(cycle) = visit(dep, graph, stack, done) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        None
    }

    for member in WORKSPACE_MEMBERS {
        let mut stack = Vec::new();
        let mut done = BTreeSet::new();
        assert!(
            visit(member, &graph, &mut stack, &mut done).is_none(),
            "cycle reachable from {member}"
        );
    }
}

#[test]
fn crate_dependency_edges_match_the_plan() {
    let metadata = metadata_json();
    let graph = dependency_graph(&metadata);

    fn internal_deps(graph: &BTreeMap<String, BTreeSet<String>>, name: &str) -> Vec<String> {
        graph[name]
            .iter()
            .filter(|d| WORKSPACE_MEMBERS.contains(&d.as_str()))
            .cloned()
            .collect()
    }

    assert_eq!(
        internal_deps(&graph, "velnor-model"),
        Vec::<String>::new(),
        "model depends on nothing"
    );
    assert_eq!(
        internal_deps(&graph, "velnor-control"),
        ["velnor-model"],
        "control depends only on model"
    );
    assert_eq!(
        internal_deps(&graph, "velnor-client"),
        ["velnor-model"],
        "client depends only on model"
    );
    assert_eq!(
        internal_deps(&graph, "velnor-render"),
        ["velnor-model"],
        "render depends only on model"
    );

    let ctl = internal_deps(&graph, "velnorctl");
    for expected in [
        "velnor-model",
        "velnor-control",
        "velnor-client",
        "velnor-render",
        "velnor-runner",
    ] {
        assert!(
            ctl.contains(&expected.to_string()),
            "velnorctl must depend on {expected}; got {ctl:?}"
        );
    }
}

#[test]
fn client_never_reaches_control_axum_or_runner_internals() {
    let metadata = metadata_json();
    let graph = dependency_graph(&metadata);
    let closure = transitive_closure(&graph, "velnor-client");

    for forbidden in ["velnor-control", "velnor-runner", "velnorctl", "axum"] {
        assert!(
            !closure.contains(forbidden),
            "velnor-client must never reach {forbidden}; closure was {closure:?}"
        );
    }
}

#[test]
fn shared_crates_stay_clap_free() {
    let metadata = metadata_json();
    let graph = dependency_graph(&metadata);
    for shared in [
        "velnor-model",
        "velnor-control",
        "velnor-client",
        "velnor-render",
    ] {
        assert!(
            !graph[shared].contains("clap"),
            "{shared} must not depend on Clap"
        );
    }
}

#[test]
fn axum_appears_nowhere_in_the_workspace_yet() {
    let metadata = metadata_json();
    let graph = dependency_graph(&metadata);
    for member in WORKSPACE_MEMBERS {
        assert!(
            !graph[member].contains("axum"),
            "Axum is isolated to a transport-adapter module by Plan 067; {member} must not depend on it yet"
        );
    }
}
