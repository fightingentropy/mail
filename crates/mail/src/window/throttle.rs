//! Limits how often sync progress reloads the thread list.

use std::time::{Duration, Instant};

/// Reloading reads every message of every listed thread, so while a large
/// sync reports progress the window reloads at most this often.
const RELOAD_EVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct ReloadThrottle {
    last: Option<Instant>,
}

impl ReloadThrottle {
    /// True, and starts a new wait, if enough time has passed since the last
    /// reload that this allowed.
    pub fn ready(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < RELOAD_EVERY)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reloads_at_most_once_per_interval() {
        let start = Instant::now();
        let mut throttle = ReloadThrottle::default();
        assert!(throttle.ready(start));
        assert!(!throttle.ready(start + Duration::from_millis(500)));
        assert!(!throttle.ready(start + RELOAD_EVERY - Duration::from_millis(1)));
        assert!(throttle.ready(start + RELOAD_EVERY));
        assert!(!throttle.ready(start + RELOAD_EVERY + Duration::from_millis(1)));
    }
}
