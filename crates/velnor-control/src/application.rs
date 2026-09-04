//! Shared control-plane application composition.
//!
//! One daemon instance owns one bundle. Transport adapters receive trait
//! objects from this bundle and never construct competing stores or read
//! implementation files themselves.

use std::sync::Arc;

use crate::{
    lifecycle::LifecycleService,
    logs::LogService,
    observation::EventStream,
    query::QueryService,
    storage::StorageService,
    store::{Store, StoreResult},
    telemetry::{path_for_instance, TelemetryService},
};

/// All local application services owned by one daemon instance.
#[derive(Clone)]
pub struct ApplicationServices {
    query: Arc<QueryService>,
    events: Arc<EventStream>,
    logs: Arc<LogService>,
    lifecycle: Arc<LifecycleService>,
    storage: Arc<StorageService>,
    telemetry: Arc<TelemetryService>,
}

impl ApplicationServices {
    /// Construct one production service bundle with Plan 066 durable state.
    pub fn with_store(store: Arc<Store>, instance_slug: impl Into<String>) -> StoreResult<Self> {
        let instance_slug = instance_slug.into();
        Ok(Self {
            // Normalized Store readers are not wired yet; never expose an
            // empty projection as if it were authoritative.
            query: Arc::new(QueryService::unsupported()),
            events: Arc::new(EventStream::with_store(Arc::clone(&store), &instance_slug)?),
            // Raw job logs remain outside the operational database; fail
            // closed until their durable access path is wired.
            logs: Arc::new(LogService::unsupported()),
            lifecycle: Arc::new(LifecycleService::with_store_for_instance(
                Arc::clone(&store),
                &instance_slug,
            )?),
            // Storage catalog durability is not wired to this bundle yet.
            storage: Arc::new(StorageService::unsupported()),
            telemetry: Arc::new(TelemetryService::new(path_for_instance(
                store.path(),
                &instance_slug,
            ))),
        })
    }

    /// Construct an explicitly in-memory bundle for isolated tests.
    #[must_use]
    pub fn in_memory_for_tests() -> Self {
        Self {
            query: Arc::new(QueryService::new()),
            events: Arc::new(EventStream::new()),
            logs: Arc::new(LogService::new()),
            lifecycle: Arc::new(LifecycleService::new()),
            storage: Arc::new(StorageService::new()),
            telemetry: Arc::new(TelemetryService::empty()),
        }
    }

    /// Resource query projection.
    #[must_use]
    pub fn query(&self) -> Arc<QueryService> {
        Arc::clone(&self.query)
    }

    /// Ordered event stream.
    #[must_use]
    pub fn events(&self) -> Arc<EventStream> {
        Arc::clone(&self.events)
    }

    /// Sanitized log service.
    #[must_use]
    pub fn logs(&self) -> Arc<LogService> {
        Arc::clone(&self.logs)
    }

    /// Lifecycle mutation owner.
    #[must_use]
    pub fn lifecycle(&self) -> Arc<LifecycleService> {
        Arc::clone(&self.lifecycle)
    }

    /// Storage catalog and GC planner.
    #[must_use]
    pub fn storage(&self) -> Arc<StorageService> {
        Arc::clone(&self.storage)
    }

    /// Shared process telemetry reader.
    #[must_use]
    pub fn telemetry(&self) -> Arc<TelemetryService> {
        Arc::clone(&self.telemetry)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ports::{LogPort, PortError, QueryPort, TelemetryPort, WatchPort};
    use velnor_model::{Event, ResourceMeta, Source, StorageClass, StorageObject, Timestamp};

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "velnor-application-{}-{}",
                std::process::id(),
                Timestamp::now()
                    .as_offset_datetime()
                    .unix_timestamp_nanos()
                    .unsigned_abs()
            ));
            std::fs::create_dir_all(&path).expect("temp db directory");
            Self {
                path: path.join("state.db"),
                dir: path,
            }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn event() -> Event {
        Event {
            meta: ResourceMeta::new("instance/default", Source::Local, Timestamp::UNIX_EPOCH),
            sequence: 1,
            occurred_at: Timestamp::UNIX_EPOCH,
            event_kind: "ready".to_owned(),
            subject: "instance/default".to_owned(),
            detail: None,
        }
    }

    #[test]
    fn bundle_reuses_one_identity_per_service() {
        let services = ApplicationServices::in_memory_for_tests();
        assert!(Arc::ptr_eq(&services.query(), &services.query()));
        assert!(Arc::ptr_eq(&services.events(), &services.events()));
        assert!(Arc::ptr_eq(&services.logs(), &services.logs()));
        assert!(Arc::ptr_eq(&services.lifecycle(), &services.lifecycle()));
        assert!(Arc::ptr_eq(&services.storage(), &services.storage()));
        assert!(Arc::ptr_eq(&services.telemetry(), &services.telemetry()));
    }

    #[test]
    fn service_types_implement_their_transport_ports() {
        fn assert_query<T: QueryPort>() {}
        fn assert_watch<T: WatchPort>() {}
        fn assert_logs<T: LogPort>() {}
        fn assert_telemetry<T: TelemetryPort>() {}
        assert_query::<QueryService>();
        assert_watch::<EventStream>();
        assert_logs::<LogService>();
        assert_telemetry::<TelemetryService>();
    }

    #[test]
    fn production_bundle_fails_closed_for_undurable_projections() {
        let database = TempDb::new();
        let services = ApplicationServices::with_store(
            Arc::new(Store::open(&database.path).expect("open store")),
            "default",
        )
        .expect("compose services");

        assert!(matches!(
            services
                .query()
                .query(crate::ports::QueryRequest::default()),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.query().replace(Vec::new()),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.query().generation(),
            Err(PortError::Unsupported { .. })
        ));
        services
            .events()
            .publish(event())
            .expect("event projection");
        assert!(matches!(
            services.logs().logs(crate::ports::LogRequest {
                subject: "instance/default".to_owned(),
                source: None,
                cursor: None,
                limit: 10,
            }),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.logs().append("job-1", "active", "safe", &[]),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.storage().snapshot("default"),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.storage().upsert(StorageObject {
                id: "cache/default".to_owned(),
                class: StorageClass::Cache,
                scope: "default".to_owned(),
                owner: "daemon".to_owned(),
                logical_bytes: 0,
                physical_bytes: Some(0),
                active: false,
                resource_version: 0,
                observed_at: Timestamp::UNIX_EPOCH,
            }),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.storage().plan_gc("default"),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            services.storage().execute_gc("gc-1", "digest", false),
            Err(PortError::Unsupported { .. })
        ));
        drop(services);

        let reopened = ApplicationServices::with_store(
            Arc::new(Store::open(&database.path).expect("reopen store")),
            "default",
        )
        .expect("recompose services");
        assert_eq!(
            reopened
                .events()
                .watch(crate::ports::WatchRequest {
                    resource_kind: None,
                    after_version: None,
                    limit: 10,
                })
                .expect("events after reopen")
                .len(),
            1
        );
        assert!(matches!(
            reopened
                .query()
                .query(crate::ports::QueryRequest::default()),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            reopened.logs().logs(crate::ports::LogRequest {
                subject: "instance/default".to_owned(),
                source: None,
                cursor: None,
                limit: 10,
            }),
            Err(PortError::Unsupported { .. })
        ));
        assert!(matches!(
            reopened.storage().snapshot("default"),
            Err(PortError::Unsupported { .. })
        ));
    }
}
