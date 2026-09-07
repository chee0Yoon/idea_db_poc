//! Narrow whole-operation retry for Neo4j transaction lock contention.

use std::future::Future;

use crate::error::{ApiError, ApiResult};

const BACKOFF_MS: [u64; 3] = [20, 50, 100];
const TRANSIENT_CODES: [&str; 2] = [
    "Neo.TransientError.Transaction.DeadlockDetected",
    "Neo.TransientError.Transaction.LockAcquisitionTimeout",
];

/// Retry a complete operation after Neo4j has aborted its transaction for
/// lock contention. CAS/domain errors and generic storage failures are never
/// retried. Callers must ensure every failed attempt rolls its transaction back.
pub async fn transient<T, F, Fut>(mut operation: F) -> ApiResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ApiResult<T>>,
{
    for delay in BACKOFF_MS {
        match operation().await {
            Err(error) if is_transient_lock_error(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            result => return result,
        }
    }
    operation().await
}

fn is_transient_lock_error(error: &ApiError) -> bool {
    error.code == "storage_error"
        && TRANSIENT_CODES
            .iter()
            .any(|code| error.message.contains(code))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;
    use crate::error::Issue;

    #[tokio::test]
    async fn retries_exact_transient_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let value = transient(|| {
            let attempt = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(ApiError::storage(
                        "Neo.TransientError.Transaction.DeadlockDetected: lock",
                    ))
                } else {
                    Ok(42)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_domain_or_generic_storage_errors() {
        for error in [
            ApiError::validation(vec![Issue::new("x", "invalid_field", "bad")]),
            ApiError::head_conflict(serde_json::json!({})),
            ApiError::storage("connection refused"),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let seen = calls.clone();
            let mut error = Some(error);
            let result: ApiResult<()> = transient(|| {
                seen.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(error.take().expect("called once")))
            })
            .await;
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn stops_after_three_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let result: ApiResult<()> = transient(|| {
            seen.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(ApiError::storage(
                "Neo.TransientError.Transaction.LockAcquisitionTimeout: lock",
            )))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
