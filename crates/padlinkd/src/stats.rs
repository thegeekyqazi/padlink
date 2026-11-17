//! Latency figures the daemon measures itself (SPEC.md section 9).

/// The most recent samples of one duration, in milliseconds.
#[derive(Debug, Clone)]
pub struct Samples {
    values: Vec<f64>,
    next: usize,
}

/// About five seconds at 60 frames a second.
const WINDOW: usize = 300;

impl Default for Samples {
    fn default() -> Self {
        Samples {
            values: Vec::with_capacity(WINDOW),
            next: 0,
        }
    }
}

impl Samples {
    pub fn add(&mut self, ms: f64) {
        if !ms.is_finite() {
            return;
        }
        if self.values.len() < WINDOW {
            self.values.push(ms);
        } else {
            self.values[self.next] = ms;
        }
        self.next = (self.next + 1) % WINDOW;
    }

    /// The value below which `p` (0 to 1) of the samples fall.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(f64::total_cmp);
        let index = ((sorted.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
        Some(sorted[index])
    }
}

/// `p50 8.1 / p95 9.4 ms`, or a dash when there is nothing yet.
pub fn describe(samples: &Samples) -> String {
    match (samples.percentile(0.5), samples.percentile(0.95)) {
        (Some(p50), Some(p95)) => format!("p50 {p50:.1} / p95 {p95:.1} ms"),
        _ => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_one_to_a_hundred() {
        let mut s = Samples::default();
        (1..=100).for_each(|v| s.add(f64::from(v)));
        assert_eq!(s.percentile(0.0), Some(1.0));
        assert_eq!(s.percentile(0.5), Some(51.0));
        assert_eq!(s.percentile(0.95), Some(95.0));
        assert_eq!(s.percentile(1.0), Some(100.0));
        assert_eq!(describe(&s), "p50 51.0 / p95 95.0 ms");
    }

    #[test]
    fn only_the_most_recent_window_counts() {
        let mut s = Samples::default();
        (0..WINDOW).for_each(|_| s.add(100.0));
        (0..WINDOW).for_each(|_| s.add(5.0));
        assert_eq!(s.percentile(1.0), Some(5.0));
    }

    #[test]
    fn empty_and_non_finite_are_handled() {
        let mut s = Samples::default();
        assert_eq!(s.percentile(0.5), None);
        assert_eq!(describe(&s), "-");
        s.add(f64::NAN);
        assert_eq!(s.percentile(0.5), None);
    }
}
