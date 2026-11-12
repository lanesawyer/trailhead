/// The unix time following tokio's clock, matching the control plane's own `unix_now`: a test that
/// pauses and advances the clock moves it too, so reset times built from it are exact.
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
