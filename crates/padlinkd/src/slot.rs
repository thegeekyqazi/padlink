//! The latest-wins slot between capture and encode (SPEC.md section 4).
//!
//! It holds at most one frame. A newer frame replaces one the encoder has
//! not taken yet, and the replaced frame is counted as dropped, so a slow
//! encoder or a stalled link costs frames instead of adding latency.

use std::sync::{Condvar, Mutex, MutexGuard};

#[derive(Debug)]
struct State<T> {
    item: Option<T>,
    keyframe: bool,
    closed: bool,
    dropped: u64,
}

/// What the encoder should do next.
#[derive(Debug, PartialEq)]
pub enum Work<T> {
    /// Encode this frame. `keyframe` says whether one was requested.
    Frame { item: T, keyframe: bool },
    /// No new frame, but a keyframe was requested: repeat the last picture.
    RepeatAsKeyframe,
    /// The slot was closed: stop.
    Closed,
}

#[derive(Debug)]
pub struct Slot<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            state: Mutex::new(State {
                item: None,
                keyframe: false,
                closed: false,
                dropped: 0,
            }),
            changed: Condvar::new(),
        }
    }
}

impl<T> Slot<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Offer a frame. One still waiting is replaced and counted.
    pub fn put(&self, item: T) {
        let mut state = self.lock();
        if state.item.replace(item).is_some() {
            state.dropped += 1;
        }
        drop(state);
        self.changed.notify_one();
    }

    /// Ask for the next picture to be a keyframe.
    pub fn request_keyframe(&self) {
        self.lock().keyframe = true;
        self.changed.notify_one();
    }

    /// Put a keyframe request back: there was nothing to repeat yet, so it
    /// waits for the first frame without waking the encoder again.
    pub fn defer_keyframe(&self) {
        self.lock().keyframe = true;
    }

    /// Wake the encoder and make it stop.
    pub fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Frames replaced before the encoder took them.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    /// Block until there is something to do.
    pub fn take(&self) -> Work<T> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return Work::Closed;
            }
            if let Some(item) = state.item.take() {
                return Work::Frame {
                    item,
                    keyframe: std::mem::take(&mut state.keyframe),
                };
            }
            if std::mem::take(&mut state.keyframe) {
                return Work::RepeatAsKeyframe;
            }
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_newer_frame_replaces_an_untaken_one_and_the_drop_is_counted() {
        let slot = Slot::default();
        slot.put(1);
        slot.put(2);
        slot.put(3);
        assert_eq!(slot.dropped(), 2);
        assert_eq!(
            slot.take(),
            Work::Frame {
                item: 3,
                keyframe: false
            }
        );
        // Taken frames are not drops.
        slot.put(4);
        assert_eq!(slot.dropped(), 2);
    }

    #[test]
    fn a_keyframe_request_rides_on_the_next_frame() {
        let slot = Slot::default();
        slot.request_keyframe();
        slot.put(7);
        assert_eq!(
            slot.take(),
            Work::Frame {
                item: 7,
                keyframe: true
            }
        );
        // And is used up.
        slot.put(8);
        assert_eq!(
            slot.take(),
            Work::Frame {
                item: 8,
                keyframe: false
            }
        );
    }

    #[test]
    fn a_keyframe_request_on_a_static_screen_repeats_the_last_picture() {
        let slot = Slot::<u8>::default();
        slot.request_keyframe();
        assert_eq!(slot.take(), Work::RepeatAsKeyframe);
    }

    #[test]
    fn a_deferred_keyframe_waits_for_the_first_frame() {
        let slot = Arc::new(Slot::default());
        slot.defer_keyframe();
        // take() would return RepeatAsKeyframe at once if called now; the
        // point of deferring is that the encoder is not woken for it.
        let other = slot.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            other.put(1u8);
        });
        handle.join().unwrap();
        assert_eq!(
            slot.take(),
            Work::Frame {
                item: 1,
                keyframe: true
            }
        );
    }

    #[test]
    fn closing_wakes_a_waiting_encoder() {
        let slot = Arc::new(Slot::<u8>::default());
        let other = slot.clone();
        let waiter = std::thread::spawn(move || other.take());
        std::thread::sleep(Duration::from_millis(20));
        slot.close();
        assert_eq!(waiter.join().unwrap(), Work::Closed);
        // Closed wins over anything still in the slot.
        slot.put(1);
        assert_eq!(slot.take(), Work::Closed);
    }
}
