//! The daemon's state machine (SPEC.md section 4), free of I/O so its
//! transitions can be tested with made-up events.
//!
//! `Paused` (the user said disconnect), `Idle` (no device), `Attached`
//! (device on USB, dialing with backoff until the app is open), `Connected`
//! (a session is running; the session itself reports handshake or
//! streaming), and back.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Paused,
    Idle,
    Attached,
    Connected,
}

/// Why a session ended, as far as the next step is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The iPad locked, or the link dropped: it may be back at once.
    WillReturn,
    /// The app quit: it listens again only when the user reopens it.
    AppClosed,
    /// padlink could not set the display up. Retrying at once would loop.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    UserConnect,
    UserDisconnect,
    DeviceAttached,
    DeviceDetached,
    /// Nothing listens on the receiver port: the app is not open.
    DialRefused,
    DialSucceeded,
    SessionEnded(Ended),
}

/// What the driver must do after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Nothing,
    /// Dial the receiver after this delay.
    Dial(Duration),
}

/// R1.3: 0.5 s rising to 3 s.
const FIRST_RETRY: Duration = Duration::from_millis(500);
const LAST_RETRY: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct Machine {
    phase: Phase,
    device: bool,
    retry: Duration,
}

impl Machine {
    /// `connect` is whether to connect without being asked (R2.2).
    pub fn new(connect: bool) -> Self {
        Machine {
            phase: if connect { Phase::Idle } else { Phase::Paused },
            device: false,
            retry: FIRST_RETRY,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(test)]
    pub fn device_present(&self) -> bool {
        self.device
    }

    /// The next retry delay, growing by half each time up to the limit.
    fn backoff(&mut self) -> Duration {
        let now = self.retry;
        self.retry = (self.retry * 3 / 2).min(LAST_RETRY);
        now
    }

    fn attach(&mut self, delay: Duration) -> Command {
        self.phase = Phase::Attached;
        Command::Dial(delay)
    }

    /// Start over from whatever the device situation is.
    fn resume(&mut self, delay: Duration) -> Command {
        if self.device {
            self.attach(delay)
        } else {
            self.phase = Phase::Idle;
            Command::Nothing
        }
    }

    pub fn on(&mut self, event: Event) -> Command {
        match (self.phase, event) {
            (_, Event::DeviceAttached) => {
                self.device = true;
                if self.phase == Phase::Idle {
                    self.retry = FIRST_RETRY;
                    return self.attach(Duration::ZERO);
                }
                Command::Nothing
            }
            (_, Event::DeviceDetached) => {
                self.device = false;
                if self.phase == Phase::Attached {
                    self.phase = Phase::Idle;
                }
                // A running session notices the dead link itself.
                Command::Nothing
            }
            (_, Event::UserDisconnect) => {
                self.phase = Phase::Paused;
                Command::Nothing
            }
            (Phase::Paused, Event::UserConnect) => {
                self.retry = FIRST_RETRY;
                self.resume(Duration::ZERO)
            }
            (_, Event::UserConnect) => Command::Nothing,

            (Phase::Attached, Event::DialRefused) => {
                let delay = self.backoff();
                Command::Dial(delay)
            }
            (Phase::Attached, Event::DialSucceeded) => {
                self.phase = Phase::Connected;
                self.retry = FIRST_RETRY;
                Command::Nothing
            }
            (Phase::Connected, Event::SessionEnded(why)) => match why {
                Ended::WillReturn => self.resume(FIRST_RETRY),
                Ended::AppClosed => {
                    let delay = self.backoff();
                    self.resume(delay)
                }
                Ended::Failed => self.resume(LAST_RETRY),
            },
            // Events that do not apply in this phase, such as a dial result
            // arriving after the user paused.
            _ => Command::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Event::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn plugging_in_dials_at_once_and_a_closed_app_backs_off_to_three_seconds() {
        let mut m = Machine::new(true);
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.on(DeviceAttached), Command::Dial(Duration::ZERO));
        assert_eq!(m.phase(), Phase::Attached);

        let delays: Vec<Duration> = (0..7)
            .map(|_| match m.on(DialRefused) {
                Command::Dial(d) => d,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(delays[..3], [ms(500), ms(750), ms(1125)]);
        assert_eq!(delays[0], ms(500));
        assert!(delays.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*delays.last().unwrap(), ms(3000));
    }

    #[test]
    fn a_successful_dial_connects_and_resets_the_backoff() {
        let mut m = Machine::new(true);
        m.on(DeviceAttached);
        m.on(DialRefused);
        m.on(DialRefused);
        assert_eq!(m.on(DialSucceeded), Command::Nothing);
        assert_eq!(m.phase(), Phase::Connected);
        // The iPad locks: redial soon, starting from the short delay again.
        assert_eq!(
            m.on(SessionEnded(Ended::WillReturn)),
            Command::Dial(ms(500))
        );
        assert_eq!(m.phase(), Phase::Attached);
        assert_eq!(m.on(DialRefused), Command::Dial(ms(500)));
    }

    #[test]
    fn unplugging_while_dialing_goes_idle_and_replugging_dials_again() {
        let mut m = Machine::new(true);
        m.on(DeviceAttached);
        assert_eq!(m.on(DeviceDetached), Command::Nothing);
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.on(DeviceAttached), Command::Dial(Duration::ZERO));
    }

    #[test]
    fn a_dropped_cable_ends_the_session_and_waits_for_the_device() {
        let mut m = Machine::new(true);
        m.on(DeviceAttached);
        m.on(DialSucceeded);
        // The device goes first, then the session notices.
        assert_eq!(m.on(DeviceDetached), Command::Nothing);
        assert_eq!(m.phase(), Phase::Connected);
        assert_eq!(m.on(SessionEnded(Ended::WillReturn)), Command::Nothing);
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn a_closed_app_is_redialed_with_backoff_and_a_failure_waits_three_seconds() {
        let mut m = Machine::new(true);
        m.on(DeviceAttached);
        m.on(DialSucceeded);
        assert_eq!(m.on(SessionEnded(Ended::AppClosed)), Command::Dial(ms(500)));
        m.on(DialSucceeded);
        assert_eq!(m.on(SessionEnded(Ended::Failed)), Command::Dial(ms(3000)));
        assert_eq!(m.phase(), Phase::Attached);
    }

    #[test]
    fn disconnect_pauses_from_any_phase_and_connect_resumes() {
        for prepare in [
            &[][..],
            &[DeviceAttached][..],
            &[DeviceAttached, DialSucceeded][..],
        ] {
            let mut m = Machine::new(true);
            prepare.iter().for_each(|e| {
                m.on(*e);
            });
            assert_eq!(m.on(UserDisconnect), Command::Nothing);
            assert_eq!(m.phase(), Phase::Paused);
            // While paused nothing dials, whatever happens.
            assert_eq!(m.on(DialRefused), Command::Nothing);
            assert_eq!(m.on(SessionEnded(Ended::WillReturn)), Command::Nothing);
            assert_eq!(m.phase(), Phase::Paused);

            let expect = if m.device_present() {
                (Command::Dial(Duration::ZERO), Phase::Attached)
            } else {
                (Command::Nothing, Phase::Idle)
            };
            assert_eq!((m.on(UserConnect), m.phase()), expect);
        }
    }

    #[test]
    fn without_autoconnect_nothing_happens_until_the_user_connects() {
        let mut m = Machine::new(false);
        assert_eq!(m.phase(), Phase::Paused);
        assert_eq!(m.on(DeviceAttached), Command::Nothing);
        assert_eq!(m.phase(), Phase::Paused);
        assert_eq!(m.on(UserConnect), Command::Dial(Duration::ZERO));
    }

    #[test]
    fn connect_while_already_connecting_changes_nothing() {
        let mut m = Machine::new(true);
        m.on(DeviceAttached);
        assert_eq!(m.on(UserConnect), Command::Nothing);
        assert_eq!(m.phase(), Phase::Attached);
    }
}
