//! Noticing that the machine was suspended (R2.6).
//!
//! A headless output that survives a suspend shows a frozen picture, so
//! after a resume the session must end and start again from nothing.
//!
//! The kernel keeps two clocks that differ only by time spent suspended:
//! `CLOCK_BOOTTIME` keeps counting during suspend and `CLOCK_MONOTONIC`
//! does not. When the gap between them grows, the machine slept. This needs
//! no D-Bus and cannot miss a suspend, but it only sees it afterwards.

use std::time::Duration;

use rustix::time::{ClockId, clock_gettime};

/// A gap this large is a suspend, not scheduling noise.
const THRESHOLD: Duration = Duration::from_secs(1);

fn clock(id: ClockId) -> Duration {
    let t = clock_gettime(id);
    Duration::new(
        t.tv_sec.max(0) as u64,
        t.tv_nsec.clamp(0, 999_999_999) as u32,
    )
}

/// Total time this machine has spent suspended since boot.
fn suspended_total() -> Duration {
    clock(ClockId::Boottime).saturating_sub(clock(ClockId::Monotonic))
}

#[derive(Debug)]
pub struct SuspendWatch {
    seen: Duration,
}

impl SuspendWatch {
    pub fn new() -> Self {
        SuspendWatch {
            seen: suspended_total(),
        }
    }

    /// How long the machine slept since the last call, if it did.
    pub fn slept(&mut self) -> Option<Duration> {
        self.observe(suspended_total())
    }

    fn observe(&mut self, total: Duration) -> Option<Duration> {
        let grew = total.saturating_sub(self.seen);
        if grew >= THRESHOLD {
            self.seen = total;
            Some(grew)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_grown_gap_is_reported_once_with_its_length() {
        let mut watch = SuspendWatch { seen: secs(100) };
        assert_eq!(watch.observe(secs(100)), None);
        assert_eq!(watch.observe(secs(400)), Some(secs(300)));
        assert_eq!(watch.observe(secs(400)), None);
        assert_eq!(watch.observe(secs(405)), Some(secs(5)));
    }

    #[test]
    fn clock_jitter_below_a_second_is_not_a_suspend() {
        let mut watch = SuspendWatch { seen: secs(100) };
        assert_eq!(watch.observe(secs(100) + Duration::from_millis(900)), None);
        // And a gap that seems to shrink is ignored.
        assert_eq!(watch.observe(secs(99)), None);
    }

    #[test]
    fn a_running_machine_reports_no_suspend() {
        let mut watch = SuspendWatch::new();
        assert_eq!(watch.slept(), None);
    }
}
