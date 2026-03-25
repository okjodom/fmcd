use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use tracing::{debug, instrument, warn};

use crate::events::{EventBus, FmcdEvent};
use crate::observability::correlation::RequestContext;

/// Statistics for database operations
#[derive(Debug, Default)]
pub struct DatabaseStats {
    pub total_operations: AtomicU64,
    pub successful_operations: AtomicU64,
    pub failed_operations: AtomicU64,
    pub slow_operations: AtomicU64,
    pub total_duration_ms: AtomicU64,
}

impl DatabaseStats {
    pub fn record_operation(&self, duration: Duration, success: bool) {
        self.total_operations.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);

        if success {
            self.successful_operations.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_operations.fetch_add(1, Ordering::Relaxed);
        }

        // Track slow operations (>100ms)
        if duration.as_millis() > 100 {
            self.slow_operations.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn get_summary(&self) -> DatabaseStatsSummary {
        let total = self.total_operations.load(Ordering::Relaxed);
        let successful = self.successful_operations.load(Ordering::Relaxed);
        let failed = self.failed_operations.load(Ordering::Relaxed);
        let slow = self.slow_operations.load(Ordering::Relaxed);
        let total_duration = self.total_duration_ms.load(Ordering::Relaxed);

        DatabaseStatsSummary {
            total_operations: total,
            successful_operations: successful,
            failed_operations: failed,
            slow_operations: slow,
            average_duration_ms: if total > 0 { total_duration / total } else { 0 },
            success_rate: if total > 0 {
                (successful as f64 / total as f64) * 100.0
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseStatsSummary {
    pub total_operations: u64,
    pub successful_operations: u64,
    pub failed_operations: u64,
    pub slow_operations: u64,
    pub average_duration_ms: u64,
    pub success_rate: f64,
}

/// Trait for database operations that we want to instrument
#[async_trait]
pub trait DatabaseInterface: Send + Sync {
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    async fn set(&self, key: &[u8], value: &[u8]) -> Result<()>;
    async fn delete(&self, key: &[u8]) -> Result<()>;
    async fn exists(&self, key: &[u8]) -> Result<bool>;
    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
}

/// Instrumented database wrapper that logs all operations
pub struct InstrumentedDatabase<T: DatabaseInterface> {
    inner: T,
    event_bus: Arc<EventBus>,
    stats: Arc<DatabaseStats>,
    service_name: String,
}

impl<T: DatabaseInterface> InstrumentedDatabase<T> {
    pub fn new(inner: T, event_bus: Arc<EventBus>, service_name: impl Into<String>) -> Self {
        Self {
            inner,
            event_bus,
            stats: Arc::new(DatabaseStats::default()),
            service_name: service_name.into(),
        }
    }

    pub fn stats(&self) -> Arc<DatabaseStats> {
        self.stats.clone()
    }

    /// Get key prefix for logging (first 8 bytes, hex encoded)
    pub(crate) fn get_key_prefix(&self, key: &[u8]) -> String {
        let prefix_len = std::cmp::min(key.len(), 8);
        hex::encode(&key[..prefix_len])
    }

    /// Publish database event
    async fn publish_db_event(
        &self,
        operation: &str,
        key_prefix: &str,
        duration: Duration,
        success: bool,
        error_message: Option<String>,
        context: Option<&RequestContext>,
    ) {
        let event = FmcdEvent::DatabaseQueryExecuted {
            operation: operation.to_string(),
            key_prefix: key_prefix.to_string(),
            duration_ms: duration.as_millis(),
            success,
            error_message,
            correlation_id: context.map(|c| c.correlation_id.clone()),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_bus.publish(event).await {
            warn!(
                operation = %operation,
                key_prefix = %key_prefix,
                error = ?e,
                "Failed to publish database event"
            );
        }
    }

    /// Execute a database operation with instrumentation
    #[instrument(skip(self, operation, context), fields(operation = %op_name, key_prefix))]
    async fn execute_with_instrumentation<F, R>(
        &self,
        operation: F,
        op_name: &str,
        key: &[u8],
        context: Option<&RequestContext>,
    ) -> Result<R>
    where
        F: std::future::Future<Output = Result<R>>,
    {
        let start = Instant::now();
        let key_prefix = self.get_key_prefix(key);

        // Set the key_prefix field on the span
        tracing::Span::current().record("key_prefix", &key_prefix);

        debug!(
            operation = %op_name,
            key_prefix = %key_prefix,
            service = %self.service_name,
            "Database operation started"
        );

        let result = operation.await;
        let duration = start.elapsed();
        let success = result.is_ok();

        // Record statistics
        self.stats.record_operation(duration, success);

        // Log operation result
        match &result {
            Ok(_) => {
                if duration.as_millis() > 100 {
                    warn!(
                        operation = %op_name,
                        key_prefix = %key_prefix,
                        duration_ms = %duration.as_millis(),
                        service = %self.service_name,
                        "Slow database operation detected"
                    );
                } else {
                    debug!(
                        operation = %op_name,
                        key_prefix = %key_prefix,
                        duration_ms = %duration.as_millis(),
                        service = %self.service_name,
                        "Database operation completed successfully"
                    );
                }
            }
            Err(e) => {
                warn!(
                    operation = %op_name,
                    key_prefix = %key_prefix,
                    duration_ms = %duration.as_millis(),
                    error = ?e,
                    service = %self.service_name,
                    "Database operation failed"
                );
            }
        }

        // Publish event
        let error_message = result.as_ref().err().map(|e| e.to_string());
        self.publish_db_event(
            op_name,
            &key_prefix,
            duration,
            success,
            error_message,
            context,
        )
        .await;

        result
    }
}

#[async_trait]
impl<T: DatabaseInterface> DatabaseInterface for InstrumentedDatabase<T> {
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.execute_with_instrumentation(
            self.inner.get(key),
            "get",
            key,
            None, // TODO: Pass context when available
        )
        .await
    }

    async fn set(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.execute_with_instrumentation(
            self.inner.set(key, value),
            "set",
            key,
            None, // TODO: Pass context when available
        )
        .await
    }

    async fn delete(&self, key: &[u8]) -> Result<()> {
        self.execute_with_instrumentation(
            self.inner.delete(key),
            "delete",
            key,
            None, // TODO: Pass context when available
        )
        .await
    }

    async fn exists(&self, key: &[u8]) -> Result<bool> {
        self.execute_with_instrumentation(
            self.inner.exists(key),
            "exists",
            key,
            None, // TODO: Pass context when available
        )
        .await
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.execute_with_instrumentation(
            self.inner.scan_prefix(prefix),
            "scan_prefix",
            prefix,
            None, // TODO: Pass context when available
        )
        .await
    }
}

/// Helper trait for adding context to database operations
pub trait DatabaseWithContext<T: DatabaseInterface> {
    fn with_context<'a>(&'a self, context: &'a RequestContext) -> DatabaseContextWrapper<'a, T>;
}

/// Wrapper that carries request context for database operations
pub struct DatabaseContextWrapper<'a, T: DatabaseInterface> {
    db: &'a InstrumentedDatabase<T>,
    context: &'a RequestContext,
}

impl<'a, T: DatabaseInterface> DatabaseContextWrapper<'a, T> {
    pub fn new(db: &'a InstrumentedDatabase<T>, context: &'a RequestContext) -> Self {
        Self { db, context }
    }

    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.db
            .execute_with_instrumentation(self.db.inner.get(key), "get", key, Some(self.context))
            .await
    }

    pub async fn set(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.db
            .execute_with_instrumentation(
                self.db.inner.set(key, value),
                "set",
                key,
                Some(self.context),
            )
            .await
    }

    pub async fn delete(&self, key: &[u8]) -> Result<()> {
        self.db
            .execute_with_instrumentation(
                self.db.inner.delete(key),
                "delete",
                key,
                Some(self.context),
            )
            .await
    }

    pub async fn exists(&self, key: &[u8]) -> Result<bool> {
        self.db
            .execute_with_instrumentation(
                self.db.inner.exists(key),
                "exists",
                key,
                Some(self.context),
            )
            .await
    }

    pub async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.db
            .execute_with_instrumentation(
                self.db.inner.scan_prefix(prefix),
                "scan_prefix",
                prefix,
                Some(self.context),
            )
            .await
    }
}

impl<T: DatabaseInterface> DatabaseWithContext<T> for InstrumentedDatabase<T> {
    fn with_context<'a>(&'a self, context: &'a RequestContext) -> DatabaseContextWrapper<'a, T> {
        DatabaseContextWrapper::new(self, context)
    }
}
