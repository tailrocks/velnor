//! Crash-safe desired/observed lifecycle operations.
//!
//! This ledger is the common owner for API and signal-driven lifecycle
//! requests. Replaying an idempotency key returns its prior result and never
//! repeats an effect.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use velnor_model::Slug;

use crate::ports::{MutationKind, MutationPort, MutationRequest, MutationResult, PortError};
use crate::store::Store;

/// One instance's desired and observed lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleState {
    /// Canonical instance name.
    pub instance: String,
    /// Desired state (`ready`, `cordoned`, or `draining`).
    pub desired: String,
    /// Last observed state.
    pub observed: String,
    /// Monotonic resource version.
    pub version: u64,
    /// Desired stable-slot count, when a scale operation supplied one.
    pub desired_slots: Option<u32>,
}

#[derive(Default)]
struct State {
    next_operation: u64,
    instances: BTreeMap<String, LifecycleState>,
    /// Bounded only for the in-memory implementation. Durable services use
    /// SQLite for replay and therefore do not retain a second operation copy.
    operations: BTreeMap<String, MutationResult>,
}

/// Lifecycle application service.
#[derive(Clone)]
pub struct LifecycleService {
    state: Arc<Mutex<State>>,
    store: Option<Arc<Store>>,
    store_instance: Option<String>,
    instance: Option<String>,
}

impl LifecycleService {
    /// Create an empty lifecycle service.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            store: None,
            store_instance: None,
            instance: None,
        }
    }

    /// Create a lifecycle service backed by the host-shared operational store.
    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "tests may panic"
    )]
    #[must_use]
    pub fn with_store(store: Arc<Store>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            store: Some(store),
            store_instance: None,
            instance: None,
        }
    }

    /// Create a durable lifecycle service restricted to one daemon instance.
    ///
    /// # Errors
    /// Returns a store configuration error when `instance` is not a canonical
    /// instance identity.
    pub fn with_store_for_instance(
        store: Arc<Store>,
        instance: impl Into<String>,
    ) -> crate::store::StoreResult<Self> {
        let instance = instance.into();
        Self::with_store_and_api_instance(store, &instance, &instance)
    }

    /// Create a durable lifecycle service with distinct durable and API
    /// identities. The runner store uses the host slug while the local API
    /// may expose an operator-selected instance name.
    pub fn with_store_and_api_instance(
        store: Arc<Store>,
        store_instance: &str,
        api_instance: &str,
    ) -> crate::store::StoreResult<Self> {
        validate_configured_instance(store_instance)?;
        validate_configured_instance(api_instance)?;
        Ok(Self {
            state: Arc::new(Mutex::new(State::default())),
            store: Some(store),
            store_instance: Some(store_instance.to_owned()),
            instance: Some(api_instance.to_owned()),
        })
    }

    /// Register an instance without changing its desired state.
    pub fn register(&self, instance: &str) -> Result<LifecycleState, PortError> {
        self.validate_target(instance)?;
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        Ok(state
            .instances
            .entry(instance.to_owned())
            .or_insert_with(|| LifecycleState {
                instance: instance.to_owned(),
                desired: "ready".to_owned(),
                observed: "ready".to_owned(),
                version: 1,
                desired_slots: None,
            })
            .clone())
    }

    /// Read one instance state through the store, refreshing the cache.
    ///
    /// Unlike [`LifecycleService::get`], this never serves a stale cached
    /// copy when a store is attached: it reads the durable row and replaces
    /// the cached entry before returning. Drain decisions use this so a
    /// desired state written by another process is observed promptly.
    /// Without a store this is identical to `get`.
    pub fn desired_fresh(&self, instance: &str) -> Result<LifecycleState, PortError> {
        self.validate_target(instance)?;
        let Some(store) = &self.store else {
            return self.get(instance);
        };
        let store_instance = self.store_instance.as_deref().unwrap_or(instance);
        let fresh = store
            .lifecycle_instance(store_instance)
            .map_err(store_error)?
            .map(|row| LifecycleState {
                instance: instance.to_owned(),
                desired: row.desired_state,
                observed: row.observed_state,
                version: row.resource_version,
                desired_slots: row.desired_slots,
            })
            .ok_or_else(|| PortError::Unavailable {
                resource: format!("instance {instance}"),
            })?;
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        state.instances.insert(instance.to_owned(), fresh.clone());
        Ok(fresh)
    }

    /// Read one instance state.
    pub fn get(&self, instance: &str) -> Result<LifecycleState, PortError> {
        self.validate_target(instance)?;
        let state = self.state.lock().map_err(|_| unavailable())?;
        if let Some(value) = state.instances.get(instance) {
            return Ok(value.clone());
        }
        drop(state);
        let Some(store) = &self.store else {
            return Err(PortError::Unavailable {
                resource: format!("instance {instance}"),
            });
        };
        let store_instance = self.store_instance.as_deref().unwrap_or(instance);
        store
            .lifecycle_instance(store_instance)
            .map_err(store_error)?
            .map(|row| LifecycleState {
                instance: instance.to_owned(),
                desired: row.desired_state,
                observed: row.observed_state,
                version: row.resource_version,
                desired_slots: row.desired_slots,
            })
            .ok_or_else(|| PortError::Unavailable {
                resource: format!("instance {instance}"),
            })
    }
}

impl MutationPort for LifecycleService {
    fn mutate(&self, request: MutationRequest) -> Result<MutationResult, PortError> {
        validate_request(&request)?;
        self.validate_target(&request.target)?;
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        let desired = desired_state(&request.kind).to_owned();
        if let Some(store) = &self.store {
            let store_instance = self
                .store_instance
                .as_deref()
                .unwrap_or(request.target.as_str());
            let operation_id = format!("op-{}", uuid::Uuid::new_v4());
            let created_at = velnor_model::Timestamp::now();
            let operation_request = crate::store::LifecycleOperationRequest {
                instance_slug: store_instance.to_owned(),
                kind: kind_name(&request.kind).to_owned(),
                target: request.target.clone(),
                reason: request.reason.clone(),
                idempotency_key: request.idempotency_key.clone(),
                desired_state: desired.clone(),
                desired_slots: request.scale_to,
                expected_version: request.expected_version,
                operation_id,
                created_at,
            };
            let (operation, fresh) = store
                .record_lifecycle_operation(&operation_request)
                .map_err(store_error)?;
            if !fresh {
                return Ok(MutationResult {
                    operation_id: operation.operation_id,
                    phase: operation.phase,
                    resource: None,
                });
            }
            state.next_operation = state.next_operation.saturating_add(1);
            state.instances.insert(
                request.target.clone(),
                LifecycleState {
                    instance: request.target.clone(),
                    desired,
                    observed: "ready".to_owned(),
                    version: operation.resource_version,
                    desired_slots: request.scale_to,
                },
            );
            let result = MutationResult {
                operation_id: operation.operation_id,
                phase: operation.phase,
                resource: None,
            };
            return Ok(result);
        }
        if let Some(result) = state.operations.get(&request.idempotency_key) {
            return Ok(result.clone());
        }
        if state.operations.len() >= crate::store::records::MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE {
            return Err(operation_quota_exhausted());
        }
        let instance = state
            .instances
            .entry(request.target.clone())
            .or_insert_with(|| LifecycleState {
                instance: request.target.clone(),
                desired: "ready".to_owned(),
                observed: "ready".to_owned(),
                version: 1,
                desired_slots: None,
            });
        if request
            .expected_version
            .is_some_and(|version| version != instance.version)
        {
            return Err(PortError::Conflict {
                operation: "lifecycle resource version changed".to_owned(),
            });
        }
        instance.desired = desired;
        if request.scale_to.is_some() {
            instance.desired_slots = request.scale_to;
        }
        instance.version = instance.version.saturating_add(1);
        state.next_operation = state.next_operation.saturating_add(1);
        let result = MutationResult {
            operation_id: format!("op-{}", state.next_operation),
            phase: "accepted".to_owned(),
            resource: None,
        };
        state
            .operations
            .insert(request.idempotency_key, result.clone());
        Ok(result)
    }
}

impl LifecycleService {
    fn validate_target(&self, target: &str) -> Result<(), PortError> {
        validate_target(target)?;
        if let Some(instance) = &self.instance
            && target != instance
        {
            return Err(PortError::Invalid {
                field: "target".to_owned(),
                message: "target must match the configured lifecycle instance".to_owned(),
            });
        }
        Ok(())
    }
}

fn desired_state(kind: &MutationKind) -> &'static str {
    match kind {
        MutationKind::Cordon => "cordoned",
        MutationKind::Uncordon | MutationKind::Resume => "ready",
        MutationKind::Drain | MutationKind::Restart => "draining",
        MutationKind::Recycle => "recycling",
        MutationKind::Scale => "scaling",
        MutationKind::Reconcile => "reconciling",
    }
}

fn kind_name(kind: &MutationKind) -> &'static str {
    match kind {
        MutationKind::Cordon => "cordon",
        MutationKind::Uncordon => "uncordon",
        MutationKind::Drain => "drain",
        MutationKind::Recycle => "recycle",
        MutationKind::Resume => "resume",
        MutationKind::Restart => "restart",
        MutationKind::Scale => "scale",
        MutationKind::Reconcile => "reconcile",
    }
}

fn store_error(error: crate::store::StoreError) -> PortError {
    if error.envelope.reason == "store.lifecycle.operation_quota" {
        operation_quota_exhausted()
    } else if error.envelope.class == velnor_model::ExitClass::Conflict.as_str() {
        PortError::Conflict {
            operation: "durable lifecycle precondition changed".to_owned(),
        }
    } else {
        PortError::Operation {
            operation: "durable lifecycle write failed".to_owned(),
        }
    }
}

fn operation_quota_exhausted() -> PortError {
    PortError::Unavailable {
        resource: "lifecycle operation quota exhausted".to_owned(),
    }
}

impl Default for LifecycleService {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_target(target: &str) -> Result<(), PortError> {
    if Slug::validate("target", target).is_err() {
        return Err(PortError::Invalid {
            field: "target".to_owned(),
            message: "target must be a canonical instance identity".to_owned(),
        });
    }
    Ok(())
}

fn validate_configured_instance(instance: &str) -> crate::store::StoreResult<()> {
    if Slug::validate("instance", instance).is_err() {
        return Err(crate::store::StoreError::new(
            velnor_model::ExitClass::Usage,
            "lifecycle.instance.invalid",
        ));
    }
    Ok(())
}

const MAX_LIFECYCLE_TEXT_BYTES: usize = 512;
const MAX_SCALE_SLOTS: u32 = 4_096;

fn validate_request(request: &MutationRequest) -> Result<(), PortError> {
    if !supports_mutation(&request.kind) {
        return Err(PortError::Unsupported {
            operation: kind_name(&request.kind).to_owned(),
        });
    }
    validate_bounded_text("reason", &request.reason, true)?;
    validate_bounded_text("idempotency_key", &request.idempotency_key, true)?;
    match (&request.kind, request.scale_to) {
        (MutationKind::Scale, Some(slots)) if (1..=MAX_SCALE_SLOTS).contains(&slots) => Ok(()),
        (MutationKind::Scale, None) => Err(PortError::Invalid {
            field: "scale_to".to_owned(),
            message: "scale requires between 1 and 4096 slots".to_owned(),
        }),
        (MutationKind::Scale, Some(_)) => Err(PortError::Invalid {
            field: "scale_to".to_owned(),
            message: "scale requires between 1 and 4096 slots".to_owned(),
        }),
        (_, Some(_)) => Err(PortError::Invalid {
            field: "scale_to".to_owned(),
            message: "scale_to is only valid for scale".to_owned(),
        }),
        (_, None) => Ok(()),
    }
}

/// Only mutations with a live actuator may enter the durable ledger. The
/// former implementation accepted recycle/scale/reconcile/restart and left
/// those intents permanently in `accepted` with no controller that could
/// perform them.
fn supports_mutation(kind: &MutationKind) -> bool {
    matches!(
        kind,
        MutationKind::Cordon | MutationKind::Uncordon | MutationKind::Drain | MutationKind::Resume
    )
}

fn validate_bounded_text(
    field: &'static str,
    value: &str,
    required: bool,
) -> Result<(), PortError> {
    if (required && value.trim().is_empty())
        || value.len() > MAX_LIFECYCLE_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(PortError::Invalid {
            field: field.to_owned(),
            message: "must be non-empty, control-free, and at most 512 bytes".to_owned(),
        });
    }
    Ok(())
}

fn unavailable() -> PortError {
    PortError::Unavailable {
        resource: "lifecycle ledger".to_owned(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[test]
    fn replaying_mutation_key_does_not_create_second_operation() {
        let service = LifecycleService::new();
        service.register("primary").expect("register");
        let request = MutationRequest {
            kind: MutationKind::Cordon,
            target: "primary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "request-1".to_owned(),
            expected_version: Some(1),
            scale_to: None,
        };
        let first = service.mutate(request.clone()).expect("mutate");
        let replay = service.mutate(request).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(service.get("primary").expect("state").desired, "cordoned");
    }

    #[test]
    fn durable_service_replays_after_process_state_is_recreated() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-{}-{}",
            std::process::id(),
            velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("state.db");
        let store = Arc::new(crate::store::Store::open(&path).expect("store"));
        let request = MutationRequest {
            kind: MutationKind::Drain,
            target: "primary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "durable-1".to_owned(),
            expected_version: Some(1),
            scale_to: None,
        };
        let first = LifecycleService::with_store(Arc::clone(&store))
            .mutate(request.clone())
            .expect("first mutation");
        let recreated = LifecycleService::with_store(store);
        let replay = recreated.mutate(request).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(recreated.get("primary").expect("state").desired, "draining");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn durable_idempotency_key_binds_expected_version() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-version-{}-{}",
            std::process::id(),
            velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("state.db");
        let store = Arc::new(crate::store::Store::open(&path).expect("store"));
        let service = LifecycleService::with_store(store);
        let request = MutationRequest {
            kind: MutationKind::Drain,
            target: "primary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "durable-version-bound".to_owned(),
            expected_version: Some(1),
            scale_to: None,
        };
        service.mutate(request.clone()).expect("first mutation");
        let mut changed_precondition = request;
        changed_precondition.expected_version = None;
        assert!(matches!(
            service.mutate(changed_precondition),
            Err(PortError::Conflict { .. })
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn desired_fresh_reads_through_a_stale_cache_and_updates_it() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-fresh-{}-{}",
            std::process::id(),
            velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("state.db");
        let store = Arc::new(crate::store::Store::open(&path).expect("store"));
        let stale = LifecycleService::with_store(Arc::clone(&store));
        let writer = LifecycleService::with_store(Arc::clone(&store));
        let request = MutationRequest {
            kind: MutationKind::Cordon,
            target: "primary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "fresh-1".to_owned(),
            expected_version: None,
            scale_to: None,
        };
        stale.mutate(request).expect("first mutation");
        assert_eq!(stale.get("primary").expect("cached").desired, "cordoned");
        // A second service (another process in production) advances the
        // ledger past the first service's cached copy.
        let drain = MutationRequest {
            kind: MutationKind::Drain,
            target: "primary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "fresh-2".to_owned(),
            expected_version: None,
            scale_to: None,
        };
        writer.mutate(drain).expect("second mutation");
        assert_eq!(stale.get("primary").expect("stale").desired, "cordoned");
        let fresh = stale.desired_fresh("primary").expect("fresh read");
        assert_eq!(fresh.desired, "draining");
        assert_eq!(fresh.version, 3);
        // The cache now serves the fresh row too.
        assert_eq!(stale.get("primary").expect("updated").desired, "draining");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn desired_fresh_without_store_matches_get_and_unknown_is_unavailable() {
        let service = LifecycleService::new();
        service.register("primary").expect("register");
        assert_eq!(
            service.desired_fresh("primary").expect("fresh"),
            service.get("primary").expect("cached")
        );
        assert!(service.desired_fresh("ghost").is_err());
    }

    #[test]
    fn mutation_validation_bounds_text_and_scale_requests() {
        let service = LifecycleService::new();
        let base = MutationRequest {
            kind: MutationKind::Cordon,
            target: "primary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "request-1".to_owned(),
            expected_version: None,
            scale_to: None,
        };

        let mut oversized = base.clone();
        oversized.reason = "x".repeat(MAX_LIFECYCLE_TEXT_BYTES + 1);
        assert!(service.mutate(oversized).is_err());

        let mut control = base.clone();
        control.idempotency_key = "request\n1".to_owned();
        assert!(service.mutate(control).is_err());

        let mut missing_scale = base.clone();
        missing_scale.kind = MutationKind::Scale;
        assert!(service.mutate(missing_scale).is_err());

        let mut oversized_scale = base;
        oversized_scale.kind = MutationKind::Scale;
        oversized_scale.scale_to = Some(MAX_SCALE_SLOTS + 1);
        assert!(service.mutate(oversized_scale).is_err());
    }

    #[test]
    fn mutations_without_an_actuator_are_rejected_before_durable_acceptance() {
        let service = LifecycleService::new();
        service.register("primary").expect("register");
        for kind in [
            MutationKind::Restart,
            MutationKind::Recycle,
            MutationKind::Scale,
            MutationKind::Reconcile,
        ] {
            let error = service
                .mutate(MutationRequest {
                    kind,
                    target: "primary".to_owned(),
                    reason: "test".to_owned(),
                    idempotency_key: "unsupported-operation".to_owned(),
                    expected_version: None,
                    scale_to: Some(2),
                })
                .expect_err("unsupported mutation must fail closed");
            assert!(matches!(error, PortError::Unsupported { .. }));
        }
        assert_eq!(service.get("primary").unwrap().desired, "ready");
    }

    #[test]
    fn instance_scoped_service_rejects_foreign_targets() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-scope-{}-{}",
            std::process::id(),
            velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("directory");
        let store = Arc::new(crate::store::Store::open(directory.join("state.db")).expect("store"));
        let service =
            LifecycleService::with_store_for_instance(store, "primary").expect("scoped service");
        let request = MutationRequest {
            kind: MutationKind::Cordon,
            target: "secondary".to_owned(),
            reason: "maintenance".to_owned(),
            idempotency_key: "scope-1".to_owned(),
            expected_version: None,
            scale_to: None,
        };

        assert!(
            matches!(service.mutate(request), Err(PortError::Invalid { field, .. }) if field == "target")
        );
        assert!(
            matches!(service.get("secondary"), Err(PortError::Invalid { field, .. }) if field == "target")
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn in_memory_operation_cache_is_bounded_and_replayable_at_capacity() {
        let service = LifecycleService::new();
        service.register("primary").expect("register");
        let base = MutationRequest {
            kind: MutationKind::Cordon,
            target: "primary".to_owned(),
            reason: "test".to_owned(),
            idempotency_key: String::new(),
            expected_version: None,
            scale_to: None,
        };

        let mut first_result = None;
        for index in 0..crate::store::records::MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE {
            let mut request = base.clone();
            request.idempotency_key = format!("key-{index}");
            let result = service.mutate(request).expect("within operation quota");
            if index == 0 {
                first_result = Some(result);
            }
        }

        let mut fresh = base.clone();
        fresh.idempotency_key = "new-key".to_owned();
        assert_eq!(service.mutate(fresh), Err(operation_quota_exhausted()));

        let mut replay = base;
        replay.idempotency_key = "key-0".to_owned();
        assert_eq!(
            service.mutate(replay),
            Ok(first_result.expect("first result"))
        );
    }
}
