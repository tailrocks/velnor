//! Explicit, path-free dependency-graph fan-out attribution.
//!
//! The collector cannot infer dependency edges or invalidation roots from
//! Cargo message order, freshness flags, or human-readable diagnostics. This
//! module therefore accepts both pieces of context explicitly and returns
//! unknown metrics whenever that context is incomplete or contradictory.

use crate::{BuildMode, UnitKind, UnitRecord};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

const UNIT_ID_PREFIX: &str = "unit:v1:";

/// A stable, path-free identity for one Cargo compilation unit.
///
/// The identity is an opaque BLAKE3 digest of structured package, target,
/// build-mode, kind, and target-triple fields. Callers should pass IDs from
/// [`UnitRecord::unit_id`] into [`DependencyGraphInput`] rather than recreate
/// them from paths or log text.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitId(String);

impl UnitId {
    /// Derive a stable identity from structured Cargo fields.
    pub fn from_parts(
        package_id: Option<&str>,
        target_name: &str,
        kind: UnitKind,
        mode: BuildMode,
        target: &str,
        features: &[String],
    ) -> Self {
        let fields = [
            package_identity(package_id),
            crate::redact_absolute_paths(target_name),
            kind.to_string(),
            mode.to_string(),
            crate::redact_absolute_paths(target),
            features.join("\u{1f}"),
        ];
        let mut canonical = String::new();
        for field in fields {
            canonical.push_str(&field.len().to_string());
            canonical.push(':');
            canonical.push_str(&field);
        }
        Self(format!(
            "{UNIT_ID_PREFIX}{}",
            blake3::hash(canonical.as_bytes()).to_hex()
        ))
    }

    /// Parse an opaque ID supplied by a graph producer.
    pub fn parse(value: &str) -> Result<Self, InvalidUnitId> {
        let value = value.trim();
        let Some(digest) = value.strip_prefix(UNIT_ID_PREFIX) else {
            return Err(InvalidUnitId {
                value: value.to_owned(),
            });
        };
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(InvalidUnitId {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// Return the serialized opaque ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this ID came from an older JSONL record without identity data.
    pub fn is_unknown(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for UnitId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for UnitId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() {
            return Ok(Self::default());
        }
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// Error returned when a graph contains a non-opaque unit identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidUnitId {
    value: String,
}

impl fmt::Display for InvalidUnitId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid unit identity '{}'; expected unit:v1:<64 hex>",
            self.value
        )
    }
}

impl std::error::Error for InvalidUnitId {}

/// One explicit dependency edge. `upstream` is the dependency and
/// `downstream` is the unit that depends on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DependencyEdge {
    /// Dependency unit.
    pub upstream: UnitId,
    /// Unit rebuilt or checked after the dependency.
    pub downstream: UnitId,
}

impl DependencyEdge {
    /// Construct one directed dependency edge.
    pub fn new(upstream: UnitId, downstream: UnitId) -> Self {
        Self {
            upstream,
            downstream,
        }
    }
}

/// Path-free graph input supplied by a structured dependency-graph producer.
///
/// Units must be listed exactly once, and every edge endpoint must be listed.
/// The graph is rejected as ambiguous when duplicate units or edges occur.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyGraphInput {
    /// Every unit participating in the graph, including isolated units.
    pub units: Vec<UnitId>,
    /// Directed dependency edges from upstream to downstream units.
    pub edges: Vec<DependencyEdge>,
}

impl DependencyGraphInput {
    /// Construct explicit graph input.
    pub fn new(units: Vec<UnitId>, edges: Vec<DependencyEdge>) -> Self {
        Self { units, edges }
    }
}

/// Explicit invalidation-root context for fan-out attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidationContext {
    /// Unit(s) explicitly identified as invalidation roots.
    pub roots: Vec<UnitId>,
}

impl InvalidationContext {
    /// Construct context for one invalidation root.
    pub fn single(root: UnitId) -> Self {
        Self { roots: vec![root] }
    }

    /// Construct context for one or more explicit invalidation roots.
    pub fn new(roots: Vec<UnitId>) -> Self {
        Self { roots }
    }
}

/// Complete input required for fan-out attribution.
///
/// `None` for either member means the context is incomplete. Malformed
/// present values are also treated as unknown rather than approximated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanoutInput {
    /// Explicit path-free dependency graph.
    pub dependency_graph: Option<DependencyGraphInput>,
    /// Explicit invalidation root(s).
    pub invalidation: Option<InvalidationContext>,
}

impl FanoutInput {
    /// Construct fan-out input from explicit graph and invalidation context.
    pub fn new(
        dependency_graph: Option<DependencyGraphInput>,
        invalidation: Option<InvalidationContext>,
    ) -> Self {
        Self {
            dependency_graph,
            invalidation,
        }
    }
}

/// Downstream fan-out metrics attached to a collected unit.
///
/// `None` means the graph or invalidation context did not establish a safe
/// result. A known zero is emitted only for a validated graph with no matching
/// downstream units.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanoutMetrics {
    /// Number of unique immediate downstream units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_downstream_count: Option<usize>,
    /// Number of unique downstream units at any reachable depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable_downstream_count: Option<usize>,
    /// Number of unique reachable downstream units observed in Cargo JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_downstream_count: Option<usize>,
    /// Whether this unit is one of the explicit invalidation roots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidation_root: Option<bool>,
}

impl FanoutMetrics {
    /// Whether all metrics are unknown and can be omitted from legacy JSONL.
    pub fn is_unknown(&self) -> bool {
        self.direct_downstream_count.is_none()
            && self.reachable_downstream_count.is_none()
            && self.observed_downstream_count.is_none()
            && self.invalidation_root.is_none()
    }
}

/// Apply graph-backed fan-out metrics to collected records.
///
/// This function performs no causality inference: it intersects explicit graph
/// descendants with unit IDs actually present in the structured Cargo stream.
/// Incomplete or ambiguous graph/invalidation context leaves every record's
/// metrics unknown.
pub fn attribute_downstream_fanout(records: &mut [UnitRecord], input: &FanoutInput) {
    let Some(graph_input) = input.dependency_graph.as_ref() else {
        return;
    };
    let Some(invalidation) = input.invalidation.as_ref() else {
        return;
    };
    let Some(graph) = ValidatedGraph::new(graph_input) else {
        return;
    };
    let Some(roots) = graph.validate_roots(invalidation) else {
        return;
    };

    let observed: BTreeSet<UnitId> = records
        .iter()
        .filter(|record| !record.unit_id.is_unknown())
        .map(|record| record.unit_id.clone())
        .collect();

    for record in records {
        let Some(direct) = graph.downstream.get(&record.unit_id) else {
            continue;
        };
        let reachable = graph.reachable_from(&record.unit_id);
        record.fanout = FanoutMetrics {
            direct_downstream_count: Some(direct.len()),
            reachable_downstream_count: Some(reachable.len()),
            observed_downstream_count: Some(reachable.intersection(&observed).count()),
            invalidation_root: Some(roots.contains(&record.unit_id)),
        };
    }
}

#[derive(Debug)]
struct ValidatedGraph {
    nodes: BTreeSet<UnitId>,
    downstream: BTreeMap<UnitId, BTreeSet<UnitId>>,
}

impl ValidatedGraph {
    fn new(input: &DependencyGraphInput) -> Option<Self> {
        if input.units.is_empty() {
            return None;
        }

        let nodes: BTreeSet<_> = input.units.iter().cloned().collect();
        if nodes.len() != input.units.len() || nodes.iter().any(UnitId::is_unknown) {
            return None;
        }

        let mut edges = BTreeSet::new();
        let mut downstream: BTreeMap<_, BTreeSet<_>> = nodes
            .iter()
            .cloned()
            .map(|node| (node, BTreeSet::new()))
            .collect();
        for edge in &input.edges {
            if edge.upstream == edge.downstream
                || !nodes.contains(&edge.upstream)
                || !nodes.contains(&edge.downstream)
                || !edges.insert(edge.clone())
            {
                return None;
            }
            let children = downstream.get_mut(&edge.upstream)?;
            children.insert(edge.downstream.clone());
        }

        if has_cycle(&nodes, &downstream) {
            return None;
        }
        Some(Self { nodes, downstream })
    }

    fn validate_roots(&self, invalidation: &InvalidationContext) -> Option<BTreeSet<UnitId>> {
        if invalidation.roots.is_empty() {
            return None;
        }
        let roots: BTreeSet<_> = invalidation.roots.iter().cloned().collect();
        if roots.len() != invalidation.roots.len()
            || roots.iter().any(UnitId::is_unknown)
            || roots.iter().any(|root| !self.nodes.contains(root))
        {
            return None;
        }
        Some(roots)
    }

    fn reachable_from(&self, root: &UnitId) -> BTreeSet<UnitId> {
        let mut reachable = BTreeSet::new();
        let mut pending: VecDeque<_> = self
            .downstream
            .get(root)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        while let Some(node) = pending.pop_front() {
            if !reachable.insert(node.clone()) {
                continue;
            }
            if let Some(children) = self.downstream.get(&node) {
                pending.extend(children.iter().cloned());
            }
        }
        reachable
    }
}

fn has_cycle(nodes: &BTreeSet<UnitId>, downstream: &BTreeMap<UnitId, BTreeSet<UnitId>>) -> bool {
    let mut indegree: BTreeMap<_, _> = nodes.iter().cloned().map(|node| (node, 0_usize)).collect();
    for children in downstream.values() {
        for child in children {
            let Some(degree) = indegree.get_mut(child) else {
                return true;
            };
            *degree += 1;
        }
    }

    let mut ready: VecDeque<_> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(node, _)| node.clone())
        .collect();
    let mut visited = 0;
    while let Some(node) = ready.pop_front() {
        visited += 1;
        if let Some(children) = downstream.get(&node) {
            for child in children {
                let Some(degree) = indegree.get_mut(child) else {
                    return true;
                };
                *degree -= 1;
                if *degree == 0 {
                    ready.push_back(child.clone());
                }
            }
        }
    }
    visited != nodes.len()
}

fn package_identity(package_id: Option<&str>) -> String {
    let Some(package_id) = package_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return "unknown:package".to_owned();
    };

    if let Some((source, identity)) = package_id.split_once('#') {
        let source_kind = source.split('+').next().unwrap_or("unknown");
        let identity = identity.split('@');
        let parts: Vec<_> = identity.take(2).collect();
        if let Some(name) = parts.first().filter(|name| !name.is_empty()) {
            let version = parts.get(1).copied().unwrap_or("unknown");
            return format!("{source_kind}:{name}@{version}");
        }
    }

    let package = package_id.split(" (").next().unwrap_or(package_id);
    let mut parts = package.split_whitespace();
    let name = parts.next().unwrap_or("unknown");
    let version = parts.next().unwrap_or("unknown");
    format!("pathless:{name}@{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_stable_and_do_not_include_source_paths() {
        let left = UnitId::from_parts(
            Some("demo 0.1.0 (path+file:///Users/alice/demo)"),
            "demo",
            UnitKind::Compilation,
            BuildMode::Check,
            "x86_64-unknown-linux-gnu",
            &[],
        );
        let right = UnitId::from_parts(
            Some("demo 0.1.0 (path+file:///Users/bob/demo)"),
            "demo",
            UnitKind::Compilation,
            BuildMode::Check,
            "x86_64-unknown-linux-gnu",
            &[],
        );
        assert_eq!(left, right);
        assert!(!left.as_str().contains("Users"));
        assert!(UnitId::parse(left.as_str()).is_ok());
    }

    #[test]
    fn feature_variants_have_distinct_identities() {
        let default_features = vec!["default".to_owned()];
        let serde_features = vec!["serde".to_owned()];
        let left = UnitId::from_parts(
            Some("demo 0.1.0 (path+file:///workspace/demo)"),
            "demo",
            UnitKind::Compilation,
            BuildMode::Check,
            "unknown",
            &default_features,
        );
        let right = UnitId::from_parts(
            Some("demo 0.1.0 (path+file:///workspace/demo)"),
            "demo",
            UnitKind::Compilation,
            BuildMode::Check,
            "unknown",
            &serde_features,
        );
        assert_ne!(left, right);
    }
}
