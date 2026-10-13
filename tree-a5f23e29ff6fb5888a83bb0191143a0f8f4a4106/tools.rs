mod common;

use std::fs;

use velnor_actions_generator::tools::ToolRegistry;

fn registry() -> ToolRegistry {
    let root = common::repo_root();
    ToolRegistry::load(&root.join("fleet/fleet-tools.toml")).expect("load registry")
}

#[test]
fn registry_renders_deterministically() {
    let registry = registry();
    let first = registry
        .render_tools_block(["gh", "actionlint"])
        .expect("render tools");
    let second = registry
        .render_tools_block(["actionlint", "gh"])
        .expect("render tools");

    assert_eq!(first, second);
    assert_eq!(first, "[tools]\nactionlint = \"1.7.12\"\ngh = \"2.96.0\"\n");
}

#[test]
fn canonical_root_graph_is_locked_and_rust_uses_mise_from_toolchain_policy() {
    let root = common::repo_root();
    let registry = registry();

    assert_eq!(
        registry
            .check_generator_files(&root.join("mise.toml"), &root.join("mise.lock"))
            .expect("check canonical tool graph"),
        6
    );
    assert!(!registry.entries().contains_key("rust"));
}

#[test]
fn normalizing_consumer_preserves_authored_sections() {
    let body = "[tools] # generated tool section\nactionlint = \"1.7.12\"\n[settings] # authored settings\nlockfile = true\n[tasks.check]\nrun = \"actionlint\"\n";
    let normalized = registry()
        .normalize_mise_file(body)
        .expect("normalize consumer");

    assert!(
        normalized
            .starts_with("[tools]\nactionlint = \"1.7.12\"\n[settings] # authored settings\n")
    );
    assert!(normalized.contains("[tasks.check]\nrun = \"actionlint\"\n"));
}

#[test]
fn fixture_corpus_rejects_each_violation_class() {
    let root = common::repo_root().join("tests/fixtures/tools");
    let registry = ToolRegistry::load(&root.join("registry.toml")).expect("load fixture registry");

    let clean_mise = fs::read_to_string(root.join("clean/mise.toml")).unwrap();
    let clean_lock = fs::read_to_string(root.join("clean/mise.lock")).unwrap();
    assert_eq!(registry.check_text(&clean_mise, &clean_lock).unwrap(), 2);

    let cases = [
        ("registry-drift", "diverges"),
        ("rust-pin", "rust pin is forbidden"),
        ("unpinned", "unpinned or invalid version"),
        ("floating", "unpinned or invalid version"),
        ("lock-drift", "no lock entry"),
    ];
    for (case, expected) in cases {
        let mise = fs::read_to_string(root.join(case).join("mise.toml")).unwrap();
        let lock = fs::read_to_string(root.join(case).join("mise.lock")).unwrap();
        let error = registry
            .check_text(&mise, &lock)
            .expect_err("fixture must be rejected");
        assert!(
            error.contains(expected),
            "{case}: expected {expected:?} in {error:?}"
        );
    }
}
