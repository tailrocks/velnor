use std::io::Cursor;

use unit_collector::{
    attribute_downstream_fanout, BuildMode, CollectOptions, DependencyEdge, DependencyGraphInput,
    FanoutInput, InvalidationContext, UnitId, UnitKind, UnitRecord,
};

fn collect(names: &[&str]) -> Vec<UnitRecord> {
    let input = names
        .iter()
        .map(|name| {
            serde_json::json!({
                "reason": "compiler-artifact",
                "package_id": format!("{name} 0.1.0 (path+file:///workspace/{name})"),
                "target": {"name": name, "kind": ["lib"], "crate_types": ["lib"]},
                "fresh": false
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    unit_collector::collect_messages(Cursor::new(input), &CollectOptions::default())
        .expect("structured test messages are valid")
}

fn unit_id(name: &str) -> UnitId {
    UnitId::from_parts(
        Some(&format!("{name} 0.1.0 (path+file:///workspace/{name})")),
        name,
        UnitKind::Compilation,
        BuildMode::Check,
        "unknown",
    )
}

fn edge(upstream: &UnitId, downstream: &UnitId) -> DependencyEdge {
    DependencyEdge::new(upstream.clone(), downstream.clone())
}

fn input(units: Vec<UnitId>, edges: Vec<DependencyEdge>, root: UnitId) -> FanoutInput {
    FanoutInput::new(
        Some(DependencyGraphInput::new(units, edges)),
        Some(InvalidationContext::single(root)),
    )
}

#[test]
fn direct_downstream_counts_are_explicit() {
    let mut records = collect(&["core", "app", "cli"]);
    let core = records[0].unit_id.clone();
    let app = records[1].unit_id.clone();
    let cli = records[2].unit_id.clone();

    attribute_downstream_fanout(
        &mut records,
        &input(
            vec![core.clone(), app.clone(), cli.clone()],
            vec![edge(&core, &app), edge(&core, &cli)],
            core.clone(),
        ),
    );

    assert_eq!(records[0].fanout.direct_downstream_count, Some(2));
    assert_eq!(records[0].fanout.reachable_downstream_count, Some(2));
    assert_eq!(records[0].fanout.observed_downstream_count, Some(2));
    assert_eq!(records[0].fanout.invalidation_root, Some(true));
}

#[test]
fn transitive_reachability_is_separate_from_observed_units() {
    let mut records = collect(&["core", "app", "cli"]);
    let core = records[0].unit_id.clone();
    let app = records[1].unit_id.clone();
    let cli = records[2].unit_id.clone();
    records.retain(|record| record.unit_name != "app");

    attribute_downstream_fanout(
        &mut records,
        &input(
            vec![core.clone(), app.clone(), cli.clone()],
            vec![edge(&core, &app), edge(&app, &cli)],
            core,
        ),
    );

    assert_eq!(records[0].fanout.direct_downstream_count, Some(1));
    assert_eq!(records[0].fanout.reachable_downstream_count, Some(2));
    assert_eq!(records[0].fanout.observed_downstream_count, Some(1));
}

#[test]
fn unrelated_unit_has_known_zero_counts() {
    let mut records = collect(&["core", "app", "unrelated"]);
    let core = records[0].unit_id.clone();
    let app = records[1].unit_id.clone();
    let unrelated = records[2].unit_id.clone();

    attribute_downstream_fanout(
        &mut records,
        &input(
            vec![core.clone(), app.clone(), unrelated.clone()],
            vec![edge(&core, &app)],
            core,
        ),
    );

    assert_eq!(records[2].fanout.direct_downstream_count, Some(0));
    assert_eq!(records[2].fanout.reachable_downstream_count, Some(0));
    assert_eq!(records[2].fanout.observed_downstream_count, Some(0));
    assert_eq!(records[2].fanout.invalidation_root, Some(false));
}

#[test]
fn duplicate_observations_do_not_inflate_observed_count() {
    let mut records = collect(&["core", "app", "app"]);
    let core = records[0].unit_id.clone();
    let app = records[1].unit_id.clone();

    attribute_downstream_fanout(
        &mut records,
        &input(
            vec![core.clone(), app.clone()],
            vec![edge(&core, &app)],
            core,
        ),
    );

    assert_eq!(records[0].fanout.observed_downstream_count, Some(1));
}

#[test]
fn duplicate_edges_make_the_graph_ambiguous() {
    let mut records = collect(&["core", "app"]);
    let core = records[0].unit_id.clone();
    let app = records[1].unit_id.clone();
    let duplicate = edge(&core, &app);

    attribute_downstream_fanout(
        &mut records,
        &input(
            vec![core.clone(), app.clone()],
            vec![duplicate.clone(), duplicate],
            core,
        ),
    );

    assert!(records.iter().all(|record| record.fanout.is_unknown()));
}

#[test]
fn missing_or_unresolved_invalidation_context_is_unknown() {
    let mut records = collect(&["core", "app"]);
    let core = records[0].unit_id.clone();
    let app = records[1].unit_id.clone();
    let graph = DependencyGraphInput::new(vec![core.clone(), app.clone()], vec![edge(&core, &app)]);

    attribute_downstream_fanout(&mut records, &FanoutInput::new(Some(graph.clone()), None));
    assert!(records.iter().all(|record| record.fanout.is_unknown()));

    attribute_downstream_fanout(
        &mut records,
        &input(
            vec![core.clone(), app],
            graph.edges,
            unit_id("not-in-graph"),
        ),
    );
    assert!(records.iter().all(|record| record.fanout.is_unknown()));
}
