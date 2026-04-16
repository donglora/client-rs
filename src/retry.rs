//! TX retry policy + outcome reporting.
//!
//! `DongLoRa Protocol v2` surfaces CAD-detected channel activity as a distinct
//! `TX_DONE(CHANNEL_BUSY)` result (`PROTOCOL.md §6.10`). The spec
//! recommends "randomized backoff and retry with a new tag" —
//! [`Dongle::tx_with_retry`](crate::Dongle::tx_with_retry) implements
//! that policy and returns a [`TxOutcome`] capturing every attempt so
//! callers (the bridge TUI especially) can surface retry counts.

use std::time::Duration;

use donglora_protocol::TxDonePayload;

use crate::errors::ClientError;

/// Policy for [`crate::Dongle::tx_with_retry`].
///
/// Defaults match the example in `PROTOCOL.md §C.5.5`: 3 attempts,
/// randomized 20-100 ms backoff on the first retry, doubling up to
/// 500 ms cap. Only `CHANNEL_BUSY` (CAD) and `EBUSY` (TX queue full)
/// trigger a retry — every other error propagates immediately.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u8,
    /// Lower bound of the initial randomized backoff (ms).
    pub backoff_ms_min: u32,
    /// Upper bound of the initial randomized backoff (ms). The jitter
    /// range is `[0, backoff_ms_max - backoff_ms_min]`.
    pub backoff_ms_max: u32,
    /// Multiplier applied to the backoff floor on each subsequent retry
    /// (standard exponential backoff).
    pub backoff_multiplier: f32,
    /// Absolute ceiling on the backoff floor (ms). Prevents runaway
    /// delays at high attempt counts.
    pub backoff_cap_ms: u32,
    /// Per-attempt command deadline. Must accommodate CAD + airtime on
    /// the slowest configuration likely to be in play.
    pub per_attempt_timeout: Duration,
    /// If true, bypass CAD (sends `skip_cad = 1`). Usually false —
    /// retrying without CAD defeats the purpose of the retry.
    pub skip_cad: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_ms_min: 20,
            backoff_ms_max: 100,
            backoff_multiplier: 2.0,
            backoff_cap_ms: 500,
            per_attempt_timeout: Duration::from_secs(5),
            skip_cad: false,
        }
    }
}

impl RetryPolicy {
    /// Sample one jitter value in `[0, backoff_ms_max - backoff_ms_min]`.
    pub(crate) fn jitter_ms(&self) -> u32 {
        use rand::Rng;
        let spread = self.backoff_ms_max.saturating_sub(self.backoff_ms_min);
        if spread == 0 {
            return 0;
        }
        rand::rng().random_range(0..=spread)
    }
}

/// Result of one attempt within a retry loop.
#[derive(Debug)]
pub struct TxAttempt {
    /// 1-indexed attempt number.
    pub attempt: u8,
    /// `Ok` with the wire `TX_DONE` payload on success, `Err` otherwise.
    pub result: Result<TxDonePayload, ClientError>,
    /// Wall-clock time this attempt took (including CAD + airtime or
    /// timeout).
    pub elapsed: Duration,
}

/// Aggregate outcome of a retry loop.
#[derive(Debug)]
pub struct TxOutcome {
    /// The final successful attempt's reported airtime. For retries, the
    /// earlier attempts' airtimes are 0 (CAD-busy doesn't go on the
    /// air), so this is also the total airtime used.
    pub final_airtime_us: u32,
    /// Every attempt, in order.
    pub attempts: Vec<TxAttempt>,
}

impl TxOutcome {
    /// How many attempts were needed (equals `attempts.len()`).
    #[must_use]
    pub fn attempts_used(&self) -> u8 {
        self.attempts.len() as u8
    }

    /// True if this outcome involved at least one retry.
    #[must_use]
    pub fn had_retries(&self) -> bool {
        self.attempts.len() > 1
    }
}
