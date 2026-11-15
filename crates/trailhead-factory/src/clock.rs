//! The factory's clock, shared by the control plane and the testkit so paused-clock tests agree
//! with it.

/// The unix time, following tokio's clock so a test that pauses and advances it moves the time too.
/// Unpaused, tokio's clock is the real one and this is the wall clock.
pub fn unix_now() -> i64 {
    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let tokio_now = tokio::time::Instant::now().into_std();
    let real_now = std::time::Instant::now();
    let skew = if tokio_now >= real_now {
        (tokio_now - real_now).as_secs() as i64
    } else {
        -((real_now - tokio_now).as_secs() as i64)
    };
    wall + skew
}

#[cfg(test)]
mod tests {
    use super::unix_now;

    #[test]
    fn unpaused_it_is_the_wall_clock() {
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!((unix_now() - wall).abs() <= 1);
    }
}
