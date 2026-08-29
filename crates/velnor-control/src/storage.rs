//! Canonical storage observation and dry-run-first GC service.
//!
//! The service owns catalog truth and reviewed plans. Filesystem deletion is an
//! injected operation outside this pure catalog layer; there is no force or
//! stale-TTL bypass for active objects.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use velnor_model::{GcCandidate, GcPlan, StorageObject, StorageSnapshot, Timestamp};

use crate::ports::PortError;

const PLAN_TTL_SECONDS: i64 = 900;
const MAX_CATALOG_OBJECTS: usize = 100_000;
const MAX_PLAN_CANDIDATES: usize = 100_000;
const MAX_RETAINED_PLANS: usize = 1_024;

/// In-memory catalog backing the storage application ports and tests.
#[derive(Clone)]
pub struct StorageService {
    state: Arc<Mutex<StorageState>>,
}

impl Default for StorageService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Default)]
struct StorageState {
    version: u64,
    next_plan_id: u64,
    objects: BTreeMap<String, StorageObject>,
    plans: BTreeMap<String, GcPlan>,
}

impl StorageService {
    /// Create an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(StorageState::default())),
        }
    }

    /// Insert or refresh one catalog object.
    pub fn upsert(&self, mut object: StorageObject) -> Result<u64, PortError> {
        if object.id.is_empty() || object.owner.is_empty() || object.scope.is_empty() {
            return Err(PortError::Invalid {
                field: "storage_object".to_owned(),
                message: "id, owner, and scope are required".to_owned(),
            });
        }
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        if !state.objects.contains_key(&object.id) && state.objects.len() >= MAX_CATALOG_OBJECTS {
            return Err(PortError::Unavailable {
                resource: "storage catalog object budget exhausted".to_owned(),
            });
        }
        state.version = state.version.saturating_add(1);
        object.resource_version = state.version;
        state.objects.insert(object.id.clone(), object);
        Ok(state.version)
    }

    /// Produce truthful accounting for one scope. Unknown physical bytes stay
    /// unknown rather than being replaced with logical size.
    pub fn snapshot(&self, scope: &str) -> Result<StorageSnapshot, PortError> {
        let state = self.state.lock().map_err(|_| unavailable())?;
        let mut objects = Vec::new();
        let mut logical_bytes = 0_u64;
        let mut physical_bytes = Some(0_u64);
        for object in state
            .objects
            .values()
            .filter(|object| object.scope == scope)
        {
            logical_bytes = logical_bytes.saturating_add(object.logical_bytes);
            physical_bytes = physical_bytes
                .zip(object.physical_bytes)
                .map(|(total, bytes)| total.saturating_add(bytes));
            objects.push(object.clone());
        }
        Ok(StorageSnapshot {
            scope: scope.to_owned(),
            objects,
            logical_bytes,
            physical_bytes,
            resource_version: state.version,
            observed_at: Timestamp::now(),
        })
    }

    /// Build an immutable dry-run GC plan for inactive objects in one scope.
    pub fn plan_gc(&self, scope: &str) -> Result<GcPlan, PortError> {
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        purge_expired_plans(&mut state);
        let candidates = state
            .objects
            .values()
            .filter(|object| object.scope == scope && !object.active)
            .map(|object| GcCandidate {
                object_id: object.id.clone(),
                resource_version: object.resource_version,
                physical_bytes: object.physical_bytes,
            })
            .collect::<Vec<_>>();
        if candidates.len() > MAX_PLAN_CANDIDATES {
            return Err(PortError::Unavailable {
                resource: "GC plan candidate budget exhausted".to_owned(),
            });
        }
        let digest = digest_candidates(&candidates);
        let created_at = Timestamp::now();
        let expires_at = Timestamp::parse(
            &(created_at
                .as_offset_datetime()
                .checked_add(time::Duration::seconds(PLAN_TTL_SECONDS))
                .ok_or_else(|| PortError::Operation {
                    operation: "GC plan expiry overflow".to_owned(),
                })?
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| PortError::Operation {
                    operation: "GC plan expiry formatting".to_owned(),
                })?),
        )
        .map_err(|_| PortError::Operation {
            operation: "GC plan expiry construction".to_owned(),
        })?;
        state.next_plan_id = state.next_plan_id.saturating_add(1);
        let plan_id = format!("gc-{}", state.next_plan_id);
        let plan = GcPlan {
            plan_id: plan_id.clone(),
            digest,
            candidates,
            created_at,
            expires_at,
        };
        state.plans.insert(plan_id, plan.clone());
        while state.plans.len() > MAX_RETAINED_PLANS {
            let oldest = state
                .plans
                .iter()
                .min_by_key(|(_, plan)| plan.created_at)
                .map(|(id, _)| id.to_owned());
            if let Some(oldest) = oldest {
                state.plans.remove(&oldest);
            } else {
                break;
            }
        }
        Ok(plan)
    }

    /// Execute exactly one reviewed plan after version/ownership revalidation.
    pub fn execute_gc(
        &self,
        plan_id: &str,
        digest: &str,
        confirmed: bool,
    ) -> Result<u64, PortError> {
        if !confirmed {
            return Err(PortError::Invalid {
                field: "confirmed".to_owned(),
                message: "GC execution requires an explicit reviewed plan".to_owned(),
            });
        }
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        purge_expired_plans(&mut state);
        let plan = state
            .plans
            .get(plan_id)
            .ok_or_else(|| PortError::Unavailable {
                resource: "GC plan".to_owned(),
            })?
            .clone();
        if plan.digest != digest {
            return Err(PortError::Conflict {
                operation: "GC plan digest changed".to_owned(),
            });
        }
        if plan.expires_at < Timestamp::now() {
            return Err(PortError::Conflict {
                operation: "GC plan expired".to_owned(),
            });
        }
        for candidate in &plan.candidates {
            let object =
                state
                    .objects
                    .get(&candidate.object_id)
                    .ok_or_else(|| PortError::Conflict {
                        operation: format!("GC object {} disappeared", candidate.object_id),
                    })?;
            if object.active || object.resource_version != candidate.resource_version {
                return Err(PortError::Conflict {
                    operation: format!("GC object {} changed or is active", candidate.object_id),
                });
            }
        }
        let mut removed = 0_u64;
        for candidate in plan.candidates {
            if state.objects.remove(&candidate.object_id).is_some() {
                removed = removed.saturating_add(1);
            }
        }
        state.version = state.version.saturating_add(1);
        Ok(removed)
    }
}

fn purge_expired_plans(state: &mut StorageState) {
    let now = Timestamp::now();
    state.plans.retain(|_, plan| plan.expires_at >= now);
}

fn digest_candidates(candidates: &[GcCandidate]) -> String {
    let mut hasher = Sha256::new();
    for candidate in candidates {
        hasher.update(candidate.object_id.as_bytes());
        hasher.update(candidate.resource_version.to_be_bytes());
        hasher.update(candidate.physical_bytes.unwrap_or_default().to_be_bytes());
    }
    format!("sha256:{}", hex_digest(&hasher.finalize()))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unavailable() -> PortError {
    PortError::Unavailable {
        resource: "storage catalog".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_model::StorageClass;

    fn object(id: &str, active: bool) -> StorageObject {
        StorageObject {
            id: id.to_owned(),
            class: StorageClass::Cache,
            scope: "trusted".to_owned(),
            owner: "job-1".to_owned(),
            logical_bytes: 10,
            physical_bytes: Some(8),
            active,
            resource_version: 0,
            observed_at: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn gc_plan_excludes_active_and_requires_reviewed_digest() {
        let service = StorageService::new();
        service.upsert(object("active", true)).expect("upsert");
        service.upsert(object("old", false)).expect("upsert");
        let plan = service.plan_gc("trusted").expect("plan");
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(
            service.execute_gc(&plan.plan_id, "wrong", true),
            Err(PortError::Conflict {
                operation: "GC plan digest changed".to_owned(),
            })
        );
        assert_eq!(
            service
                .execute_gc(&plan.plan_id, &plan.digest, true)
                .expect("execute"),
            1
        );
        assert_eq!(
            service.snapshot("trusted").expect("snapshot").objects.len(),
            1
        );
    }
}
