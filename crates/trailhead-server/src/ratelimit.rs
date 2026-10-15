use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use trailhead_core::{Error, Token};
use uuid::Uuid;

/// Per-token token buckets, in memory. A bucket holds a minute's worth of
/// requests and refills continuously, so a token can burst up to its limit and
/// then gets an even rate. Limits reset when the server restarts.
#[derive(Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

struct Bucket {
    available: f64,
    at: Instant,
}

impl RateLimiter {
    /// Spends one request from the token's bucket.
    pub fn check(&self, token: &Token) -> Result<(), Error> {
        let Some(limit) = token.rate_limit_per_minute.filter(|n| *n > 0) else {
            return Ok(());
        };
        let capacity = limit as f64;
        let per_sec = capacity / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter poisoned");
        let b = buckets.entry(token.id).or_insert(Bucket { available: capacity, at: now });
        b.available = (b.available + now.duration_since(b.at).as_secs_f64() * per_sec).min(capacity);
        b.at = now;
        if b.available >= 1.0 {
            b.available -= 1.0;
            Ok(())
        } else {
            Err(Error::RateLimited { retry_after: ((1.0 - b.available) / per_sec).ceil().max(1.0) as u64 })
        }
    }
}
