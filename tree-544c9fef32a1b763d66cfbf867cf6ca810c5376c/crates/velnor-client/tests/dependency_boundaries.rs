//! Plan 064 dependency-boundary tests.
//!
//! Asserted from `cargo metadata` so the law holds no matter how manifests
//! evolve: `velnor-client` meets the daemon only through versioned model DTOs,
//! the five new crates form an acyclic graph, and no shared crate depends on
//! Clap or Axum. The Axum transport adapter is owned by the CLI composition
//! crate; it never enters the model, service, client, or renderer crates.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

const WORKSPACE_PACKAGES: [&str; 7] = [
    "velnor-model",
    "velnor-control",
    "velnor-client",
    "velnor-render",
    "velnorctl",
    "velnor-runner",
    "velnor-tools",
];

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
fn workspace_has_exactly_the_seven_expected_packages() {
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
    for forbidden in ["velnor-control", "velnor-runner", "axum", "clap"] {
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
fn crate_dependency_direction_matches_plan_064() {
    let graph = dependency_graph(&cargo_metadata());
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
    assert_eq!(members_only(&graph["velnor-control"]), vec!["velnor-model"]);
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
fn new_crate_dependency_graph_is_acyclic() {
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
