use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const INPUT_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Input can wake the loop often; background polling and rendering keep their own deadlines.
pub(crate) struct RefreshSchedule {
    next_poll: Instant,
    last_frame: Option<Instant>,
    dirty: bool,
}

impl RefreshSchedule {
    pub fn new(now: Instant) -> Self {
        Self {
            next_poll: now,
            last_frame: None,
            dirty: true,
        }
    }

    pub fn poll_due(&mut self, now: Instant) -> bool {
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + POLL_INTERVAL;
        true
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn frame_deadline(&self, interval: Duration) -> Instant {
        self.last_frame.map_or(self.next_poll, |last| {
            last + if self.dirty {
                INPUT_FRAME_INTERVAL.min(interval)
            } else {
                interval
            }
        })
    }

    pub fn draw_due(&mut self, now: Instant, interval: Duration) -> bool {
        if self.last_frame.is_some() && now < self.frame_deadline(interval) {
            return false;
        }
        self.last_frame = Some(now);
        self.dirty = false;
        true
    }

    pub fn wait(&self, now: Instant, interval: Duration) -> Duration {
        self.next_poll
            .min(self.frame_deadline(interval))
            .saturating_duration_since(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_events_do_not_accelerate_idle_frames_or_background_work() {
        let start = Instant::now();
        let mut schedule = RefreshSchedule::new(start);
        let mut polls = 0;
        let mut frames = 0;
        for millis in 0..2000 {
            let now = start + Duration::from_millis(millis);
            polls += usize::from(schedule.poll_due(now));
            frames += usize::from(schedule.draw_due(now, Duration::from_secs(1)));
            assert!(!schedule.wait(now, Duration::from_secs(1)).is_zero());
        }
        assert_eq!(polls, 20);
        assert_eq!(frames, 2);
    }

    #[test]
    fn input_is_responsive_but_cannot_trigger_unlimited_frames() {
        let start = Instant::now();
        let mut schedule = RefreshSchedule::new(start);
        assert!(schedule.draw_due(start, Duration::from_secs(1)));
        for millis in 1..33 {
            schedule.invalidate();
            assert!(!schedule.draw_due(
                start + Duration::from_millis(millis),
                Duration::from_secs(1)
            ));
        }
        assert!(schedule.draw_due(start + INPUT_FRAME_INTERVAL, Duration::from_secs(1)));
        assert!(schedule.draw_due(start + Duration::from_millis(133), POLL_INTERVAL));
    }
}
