//! Who holds a task, worked out one way for every view.

use std::collections::HashMap;

use jiff::Timestamp;
use trailhead_model::Claim;
use uuid::Uuid;

/// The claim holding each task at `now`, by task id. Core expires claims lazily, so one past its `expires_at` may
/// still be in the mirror: it holds nothing, and no view shows it.
pub fn live_claims(claims: &[Claim], now: Timestamp) -> HashMap<Uuid, &Claim> {
    claims.iter().filter(|c| c.is_live(now)).map(|c| (c.task_id, c)).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fixture;

    #[test]
    fn an_expired_claim_holds_nothing() {
        let now: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        let claims = [
            fixture::claim(json!({"task_id": "t1", "holder_id": "a1", "expires_at": "2025-12-31T23:59:59Z"})),
            fixture::claim(json!({"task_id": "t2", "holder_id": "a1", "expires_at": "2026-01-01T00:30:00Z"})),
            fixture::claim(json!({"task_id": "t3", "holder_id": "u1", "expires_at": null})),
        ];
        let mut held: Vec<Uuid> = live_claims(&claims, now).into_keys().collect();
        held.sort();
        let mut want = vec![claims[1].task_id, claims[2].task_id];
        want.sort();
        assert_eq!(held, want);
    }
}
