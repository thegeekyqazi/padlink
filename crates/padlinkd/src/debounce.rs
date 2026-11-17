//! Debouncing display changes (R2.7).
//!
//! A rotating iPad can send several `hello` messages in quick succession.
//! Rebuilding the display for each would flash the desktop, so a change is
//! held briefly: 25 ms after a single change, 150 ms after the last of a
//! burst. Only the newest request survives.

use std::time::{Duration, Instant};

const SINGLE: Duration = Duration::from_millis(25);
const BURST: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub struct Debounce<T> {
    pending: Option<(T, Instant)>,
}

impl<T> Default for Debounce<T> {
    fn default() -> Self {
        Debounce { pending: None }
    }
}

impl<T> Debounce<T> {
    /// A change arrived at `now`. It replaces any change still waiting.
    pub fn push(&mut self, item: T, now: Instant) {
        let wait = if self.pending.is_some() {
            BURST
        } else {
            SINGLE
        };
        self.pending = Some((item, now + wait));
    }

    /// The change to act on, once its wait is over.
    pub fn take_due(&mut self, now: Instant) -> Option<T> {
        if self.pending.as_ref().is_some_and(|(_, due)| now >= *due) {
            return self.pending.take().map(|(item, _)| item);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_single_change_is_acted_on_after_25_ms() {
        let t = Instant::now();
        let mut d = Debounce::default();
        d.push("portrait", t);
        assert_eq!(d.take_due(t + ms(24)), None);
        assert_eq!(d.take_due(t + ms(25)), Some("portrait"));
        assert_eq!(d.take_due(t + ms(500)), None);
    }

    #[test]
    fn a_burst_waits_150_ms_after_its_last_change_and_keeps_only_the_newest() {
        let t = Instant::now();
        let mut d = Debounce::default();
        d.push("portrait", t);
        d.push("landscape", t + ms(10));
        d.push("portrait again", t + ms(20));
        // Past the single-change wait, but the burst is still settling.
        assert_eq!(d.take_due(t + ms(60)), None);
        assert_eq!(d.take_due(t + ms(169)), None);
        assert_eq!(d.take_due(t + ms(170)), Some("portrait again"));
    }

    #[test]
    fn after_a_change_was_taken_the_next_one_is_single_again() {
        let t = Instant::now();
        let mut d = Debounce::default();
        d.push(1, t);
        d.push(2, t + ms(5));
        assert_eq!(d.take_due(t + ms(155)), Some(2));
        d.push(3, t + ms(1000));
        assert_eq!(d.take_due(t + ms(1025)), Some(3));
    }
}
