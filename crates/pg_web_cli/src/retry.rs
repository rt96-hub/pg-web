//! Transient-conflict retry helper for `pg-web push`.
//!
//! Two concurrent pushers (typically a forgotten `pg-web dev` plus a new
//! one, or a developer racing CI on a shared staging DB) can race on the
//! `CREATE OR REPLACE FUNCTION` calls push issues against `pg_proc`.
//! Postgres's MVCC reports the loser as either `SerializationFailure`
//! (SQLSTATE 40001) or the message `tuple concurrently updated` —
//! raised as `XX000` internal_error for concurrent DDL specifically,
//! so message-string matching is unavoidable for that branch.
//!
//! The retry is safe because push runs every change inside one big
//! transaction. A retry rolls everything back via the existing tx
//! lifecycle and starts fresh; nothing the host side committed needs
//! to be undone. Retries are capped at [`MAX_ATTEMPTS`] — beyond that
//! the conflict pattern is structural (e.g., the user really does have
//! two `pg-web dev` processes locked in a steady-state race) and the
//! fix is a human one, not a longer backoff.

use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;

/// Total attempt budget (initial try + retries). Matches PostgreSQL's
/// own guidance for serializable-conflict retry loops; beyond three
/// attempts a structural problem is more likely than a transient one.
pub const MAX_ATTEMPTS: u32 = 3;

/// Run `f`, retrying on transient-conflict errors with jittered backoff.
/// Returns the first successful value or — after [`MAX_ATTEMPTS`] —
/// the last error wrapped with a retry-context message so callers can
/// distinguish exhausted-retry failures from a single-attempt failure.
pub fn with_retry<F, T>(f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    with_retry_inner(f, MAX_ATTEMPTS, is_retryable, default_sleep)
}

fn with_retry_inner<F, T, P, S>(
    mut f: F,
    max_attempts: u32,
    mut should_retry: P,
    mut sleep: S,
) -> Result<T>
where
    F: FnMut() -> Result<T>,
    P: FnMut(&anyhow::Error) -> bool,
    S: FnMut(u32),
{
    let mut attempt: u32 = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = should_retry(&e);
                if retryable && attempt < max_attempts {
                    sleep(attempt);
                    attempt += 1;
                    continue;
                }
                if retryable {
                    return Err(e.context(format!(
                        "push retried {max_attempts} times against concurrent DDL"
                    )));
                }
                return Err(e);
            }
        }
    }
}

/// True if `err`'s chain contains a Postgres error indicating a
/// transient concurrency conflict — SQLSTATE 40001 (serialization
/// failure) or the literal `tuple concurrently updated` message that
/// surfaces as XX000 internal_error from concurrent DDL.
pub fn is_retryable(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<postgres::Error>()
            .map(is_pg_retryable)
            .unwrap_or(false)
    })
}

fn is_pg_retryable(e: &postgres::Error) -> bool {
    let Some(db) = e.as_db_error() else {
        return false;
    };
    if db.code() == &postgres::error::SqlState::T_R_SERIALIZATION_FAILURE {
        return true;
    }
    db.message().contains("tuple concurrently updated")
}

fn default_sleep(_attempt: u32) {
    // Jitter source is the system clock's subsec nanos — different
    // pushers will desync naturally. Range is 10–100 ms; tight enough
    // not to feel like a hang, wide enough to break ties between two
    // processes that woke at the same wall-clock instant.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = u64::from(nanos % 91);
    thread::sleep(Duration::from_millis(10 + jitter));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn returns_ok_without_retry_on_success() {
        let calls = RefCell::new(0u32);
        let sleeps = RefCell::new(0u32);
        let out = with_retry_inner(
            || {
                *calls.borrow_mut() += 1;
                Ok::<_, anyhow::Error>(42)
            },
            3,
            |_| true, // would retry, but we never error
            |_| *sleeps.borrow_mut() += 1,
        )
        .unwrap();
        assert_eq!(out, 42);
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(*sleeps.borrow(), 0);
    }

    #[test]
    fn retries_then_succeeds_on_retryable_error() {
        let calls = RefCell::new(0u32);
        let sleeps = RefCell::new(0u32);
        let out = with_retry_inner(
            || {
                let n = {
                    let mut c = calls.borrow_mut();
                    *c += 1;
                    *c
                };
                if n < 2 {
                    Err(anyhow::anyhow!("retryable"))
                } else {
                    Ok::<_, anyhow::Error>("done")
                }
            },
            3,
            |e| e.to_string().contains("retryable"),
            |_| *sleeps.borrow_mut() += 1,
        )
        .unwrap();
        assert_eq!(out, "done");
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(*sleeps.borrow(), 1);
    }

    #[test]
    fn exhausts_attempts_and_wraps_with_retry_context() {
        let calls = RefCell::new(0u32);
        let sleeps = RefCell::new(0u32);
        let err = with_retry_inner(
            || {
                *calls.borrow_mut() += 1;
                Err::<(), _>(anyhow::anyhow!("retryable inner"))
            },
            3,
            |e| e.to_string().contains("retryable"),
            |_| *sleeps.borrow_mut() += 1,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("retried 3 times"),
            "expected exhaustion context: {msg}"
        );
        assert!(msg.contains("retryable inner"), "inner cause: {msg}");
        assert_eq!(*calls.borrow(), 3, "all attempts consumed");
        assert_eq!(*sleeps.borrow(), 2, "sleep before each retry, not after last");
    }

    #[test]
    fn non_retryable_error_returns_immediately() {
        let calls = RefCell::new(0u32);
        let sleeps = RefCell::new(0u32);
        let err = with_retry_inner(
            || {
                *calls.borrow_mut() += 1;
                Err::<(), _>(anyhow::anyhow!("permanent"))
            },
            3,
            |e| e.to_string().contains("retryable"),
            |_| *sleeps.borrow_mut() += 1,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        // No retry context wrapping — bare error.
        assert!(!msg.contains("retried"), "should not wrap: {msg}");
        assert!(msg.contains("permanent"));
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(*sleeps.borrow(), 0);
    }
}
