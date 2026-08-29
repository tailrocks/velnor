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
};

/// All local application services owned by one daemon instance.
#[derive(Clone)]
pub struct ApplicationServices {
    query: Arc<QueryService>,
    events: Arc<EventStream>,
    logs: Arc<LogService>,
    lifecycle: Arc<LifecycleService>,
    storage: Arc<StorageService>,
}

impl ApplicationServices {
    /// Construct one production service bundle with Plan 066 durable state.
    pub fn with_store(store: Arc<Store>, instance_slug: impl Into<String>) -> StoreResult<Self> {
        let instance_slug = instance_slug.into();
        Ok(Self {
            // Query resources are rebuilt from authoritative normalized rows
            // by the Plan 067 adapter; never persist a whole projection blob.
            query: Arc::new(QueryService::new()),
            events: Arc::new(EventStream::with_store(Arc::clone(&store), &instance_slug)?),
            // Raw job logs remain outside the operational database. Plan 070
            // owns their log-access persistence path.
            logs: Arc::new(LogService::new()),
            lifecycle: Arc::new(LifecycleService::with_store_for_instance(
                Arc::clone(&store),
                &instance_slug,
            )?),
            // Storage catalog durability belongs to Plan 075.
            storage: Arc::new(StorageService::new()),
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ports::{LogPort, QueryPort, WatchPort};
    use velnor_model::{
        AnyResource, Event, ResourceMeta, Source, StorageClass, StorageObject, Timestamp,
    };

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
    }

    #[test]
    fn service_types_implement_their_transport_ports() {
        fn assert_query<T: QueryPort>() {}
        fn assert_watch<T: WatchPort>() {}
        fn assert_logs<T: LogPort>() {}
        assert_query::<QueryService>();
        assert_watch::<EventStream>();
        assert_logs::<LogService>();
    }

    #[test]
    fn production_bundle_reopens_normalized_event_store() {
        let database = TempDb::new();
        let services = ApplicationServices::with_store(
            Arc::new(Store::open(&database.path).expect("open store")),
            "default",
        )
        .expect("compose services");

        services
            .query()
            .replace(vec![AnyResource::Event(event())])
            .expect("query projection");
        services
            .events()
            .publish(event())
            .expect("event projection");
        services
            .logs()
            .append("instance/default", "daemon", "ready", &[])
            .expect("log projection");
        services
            .storage()
            .upsert(StorageObject {
                id: "cache/default".to_owned(),
                class: StorageClass::Cache,
                scope: "default".to_owned(),
                owner: "daemon".to_owned(),
                logical_bytes: 10,
                physical_bytes: Some(10),
                active: true,
                resource_version: 0,
                observed_at: Timestamp::UNIX_EPOCH,
            })
            .expect("storage projection");
        drop(services);

        let reopened = ApplicationServices::with_store(
            Arc::new(Store::open(&database.path).expect("reopen store")),
            "default",
        )
        .expect("recompose services");
        assert_eq!(
            reopened
                .query()
                .query(crate::ports::QueryRequest::default())
                .expect("query after reopen")
                .resources
                .len(),
            0,
            "query projection remains in-memory until Plan 067 normalized readers"
        );
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
        assert_eq!(
            reopened
                .logs()
                .logs(crate::ports::LogRequest {
                    subject: "instance/default".to_owned(),
                    source: None,
                    cursor: None,
                    limit: 10,
                })
                .expect("logs after reopen")
                .len(),
            0,
            "raw logs remain in-memory until Plan 070"
        );
        assert_eq!(
            reopened
                .storage()
                .snapshot("default")
                .expect("storage after reopen")
                .objects
                .len(),
            0,
            "storage catalog remains in-memory until Plan 075"
        );
    }
}
