//! Per-request `statement_timeout` arming for background-worker SPI (prompt 014).
//!
//! `SET LOCAL statement_timeout` updates the GUC but does not start the timer:
//! regular backends arm it from `start_xact_command()` → `enable_statement_timeout()`
//! in `tcop/postgres.c`, which bgworker SPI never calls. We replicate that arming
//! here via the public timeout API (`enable_timeout_after` / `disable_timeout`).

use pgrx::pg_sys;

/// `TimeoutId::STATEMENT_TIMEOUT` in `utils/timeout.h` (stable enum order).
const STATEMENT_TIMEOUT: i32 = 3;

extern "C" {
    fn enable_timeout_after(id: i32, delay_ms: i32);
    fn disable_timeout(id: i32, keep_indicator: bool);
    fn get_timeout_active(id: i32) -> bool;
}

/// Arm the statement timer from the current `StatementTimeout` GUC (milliseconds).
/// Call immediately after `SET LOCAL statement_timeout = '...'`.
///
/// # Safety
/// Must run on the bgworker SPI thread inside an open transaction.
pub unsafe fn arm() {
    // Mirrors `enable_statement_timeout()` in tcop/postgres.c.
    let timeout_ms = pg_sys::StatementTimeout;
    let tx_timeout = pg_sys::TransactionTimeout;

    if timeout_ms > 0 && (timeout_ms < tx_timeout || tx_timeout == 0) {
        if !get_timeout_active(STATEMENT_TIMEOUT) {
            enable_timeout_after(STATEMENT_TIMEOUT, timeout_ms);
        }
    } else if get_timeout_active(STATEMENT_TIMEOUT) {
        disable_timeout(STATEMENT_TIMEOUT, false);
    }
}

/// Cancel an armed statement timer before the request transaction commits.
///
/// # Safety
/// Must run on the bgworker SPI thread.
pub unsafe fn disarm() {
    if get_timeout_active(STATEMENT_TIMEOUT) {
        disable_timeout(STATEMENT_TIMEOUT, false);
    }
}

/// Whether the statement timer is currently armed (test / diagnostics).
///
/// # Safety
/// Must run on a Postgres backend thread.
#[cfg(any(test, feature = "pg_test"))]
pub unsafe fn is_active() -> bool {
    get_timeout_active(STATEMENT_TIMEOUT)
}

/// RAII guard: disarms on drop (all `serve_in_tx` exit paths, including panic unwind).
pub struct Guard;

impl Guard {
    /// Arm using the current `StatementTimeout` GUC; returns a guard that disarms on drop.
    ///
    /// # Safety
    /// Same as [`arm`].
    pub unsafe fn arm() -> Self {
        arm();
        Self
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: only constructed on the SPI thread during `serve_in_tx`.
        unsafe { disarm() };
    }
}