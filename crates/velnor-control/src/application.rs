//! Shared control-plane application composition.
//!
//! One daemon instance owns one bundle. Transport adapters receive trait
//! objects from this bundle and never construct competing stores or read
//! implementation files themselves.

use std::sync::Arc;

use crate::{
    lifecycle::LifecycleService, logs::LogService, observation::EventStream, query::QueryService,
    storage::StorageService, store::Store,
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
    /// Construct a production service bundle with durable lifecycle state.
    #[must_use]
    pub fn with_store(store: Arc<Store>) -> Self {
        Self {
            query: Arc::new(QueryService::new()),
            events: Arc::new(EventStream::new()),
            logs: Arc::new(LogService::new()),
            lifecycle: Arc::new(LifecycleService::with_store(store)),
            storage: Arc::new(StorageService::new()),
        }
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
    use super::*;
    use crate::ports::{LogPort, QueryPort, WatchPort};

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
}
